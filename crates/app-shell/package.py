#!/usr/bin/env python3
"""Build the installable WebAgent app for one platform.

    python3 crates/app-shell/package.py linux|windows|android|macos [--debug] [--skip-server]

Steps: build the core server (`linked-context` + `stable-abilities`, no Linux server
tiers) for the target; zip the app files the server reads (UI, abilities, configuration,
gate and engine descriptors, and the live settings files) into the shell; then run the
Tauri build. Output paths are printed at the end.

Needs: Rust targets for the platform, the Tauri CLI (`cargo tauri`), and per platform:
Linux WebKitGTK dev packages; Windows MinGW-w64 and NSIS (`makensis`); Android the SDK
and NDK (ANDROID_HOME, NDK_HOME) and Java 17+; macOS a Mac (CI) with Xcode.
"""
import argparse, glob, json, os, shutil, subprocess, sys, zipfile
from pathlib import Path

SHELL = Path(__file__).resolve().parent
ROOT = SHELL.parent.parent
TARGETS = {
    "linux": ["x86_64-unknown-linux-gnu"],
    "windows": ["x86_64-pc-windows-gnu"],
    "macos": ["aarch64-apple-darwin"],
    # The phone build (arm64) and the emulator build (x86_64) share one APK.
    "android": ["aarch64-linux-android", "x86_64-linux-android"],
}
ANDROID_ABI = {"aarch64-linux-android": ("arm64-v8a", "aarch64"),
               "x86_64-linux-android": ("x86_64", "x86_64")}
APP_FOLDERS = ["ui", "abilities", "config", "plugins/gates", "plugins/engine"]
# Server-only parts: the build broker and host helper pages, the Administrator ability
# pack (a drop-in pack the core build does not include) and the developer docs.
APP_EXCLUDE = ["ui/main-panel/builds", "ui/main-panel/host-admin", "abilities/Administrator", "docs"]
FEATURES = "linked-context,stable-abilities"


def run(argv, **kw):
    print("+", " ".join(map(str, argv)), flush=True)
    subprocess.run(argv, check=True, **kw)


def android_env(env):
    sdk = Path(env.get("ANDROID_HOME", Path.home() / "Android"))
    ndk = Path(env.get("NDK_HOME") or sorted((sdk / "ndk").iterdir())[-1])
    env.update(ANDROID_HOME=str(sdk), NDK_HOME=str(ndk))
    if "JAVA_HOME" not in env and shutil.which("java"):
        env["JAVA_HOME"] = str(Path(shutil.which("java")).resolve().parent.parent)
    tools = ndk / "toolchains/llvm/prebuilt/linux-x86_64/bin"
    for triple in ANDROID_ABI:
        key = triple.replace("-", "_")
        env[f"CC_{key}"] = str(tools / f"{triple}26-clang")
        env[f"AR_{key}"] = str(tools / "llvm-ar")
        env[f"CARGO_TARGET_{key.upper()}_LINKER"] = str(tools / f"{triple}26-clang")
    return env


NETWORK_SECURITY = """<?xml version="1.0" encoding="utf-8"?>
<!-- The app talks to its own server over plain HTTP on loopback only. -->
<network-security-config>
    <domain-config cleartextTrafficPermitted="true">
        <domain includeSubdomains="false">127.0.0.1</domain>
        <domain includeSubdomains="false">localhost</domain>
    </domain-config>
</network-security-config>
"""


def prepare_android():
    """Edits to the generated Android project, applied after every `tauri android init`:
    keep native libraries extracted on disk (the server runs from the APK's native-library
    folder) and allow plain HTTP to loopback only."""
    app = SHELL / "gen/android/app"
    gradle = app / "build.gradle.kts"
    text = gradle.read_text()
    if "useLegacyPackaging" not in text:
        text = text.replace("    buildFeatures {", "    packaging {\n        jniLibs.useLegacyPackaging = true\n    }\n    buildFeatures {", 1)
        gradle.write_text(text)
    (app / "src/main/res/xml").mkdir(parents=True, exist_ok=True)
    (app / "src/main/res/xml/network_security_config.xml").write_text(NETWORK_SECURITY)
    manifest = app / "src/main/AndroidManifest.xml"
    text = manifest.read_text()
    if "networkSecurityConfig" not in text:
        text = text.replace("<application\n", '<application\n        android:networkSecurityConfig="@xml/network_security_config"\n        android:extractNativeLibs="true"\n', 1)
        manifest.write_text(text)


def sign_apks(env):
    """Release APKs come out unsigned. Sign them with WEBAGENT_APK_KEYSTORE (password in
    WEBAGENT_APK_KEYSTORE_PASS, alias WEBAGENT_APK_KEY_ALIAS) or else the local Android
    debug keystore, which is enough for sideloading but not for a store listing."""
    sdk = Path(env["ANDROID_HOME"])
    tools = sorted((sdk / "build-tools").iterdir())[-1]
    keystore = env.get("WEBAGENT_APK_KEYSTORE")
    if keystore:
        key = ["--ks", keystore, "--ks-pass", "env:WEBAGENT_APK_KEYSTORE_PASS",
               "--ks-key-alias", env.get("WEBAGENT_APK_KEY_ALIAS", "webagent")]
    else:
        debug = next((p for p in [Path.home() / ".android/debug.keystore",
                                   Path.home() / ".config/.android/debug.keystore"] if p.exists()), None)
        if not debug:
            print("no keystore: APKs left unsigned")
            return
        # The debug keystore's published, non-secret password.
        key = ["--ks", str(debug), "--ks-pass", "pass:android", "--ks-key-alias", "androiddebugkey"]
    for unsigned in glob.glob(str(SHELL / "gen/android/app/build/outputs/apk/**/*-unsigned.apk"), recursive=True):
        aligned = unsigned.replace("-unsigned.apk", "-aligned.apk")
        signed = unsigned.replace("-unsigned.apk", ".apk")
        run([str(tools / "zipalign"), "-f", "-p", "4", unsigned, aligned])
        run([str(tools / "apksigner"), "sign", *key, "--out", signed, aligned])
        os.remove(aligned)


def build_server(triple, debug, env):
    profile = [] if debug else ["--release"]
    run(["cargo", "build", *profile, "--locked", "--target", triple, "-p", "webagent-v3",
         "--bin", "webagent-v3", "--no-default-features", "--features", FEATURES],
        cwd=ROOT, env=env)
    target_dir = Path(env.get("CARGO_TARGET_DIR", ROOT / "target"))
    suffix = ".exe" if "windows" in triple else ""
    return target_dir / triple / ("debug" if debug else "release") / f"webagent-v3{suffix}"


def editable(relative):
    """Files the installed app keeps editable in checkout/: every config/ file, the
    live-files.json entries, and ability JSON and Markdown (read live over app/abilities)."""
    return relative.startswith("config/") or (
        relative.startswith("abilities/") and relative.endswith((".json", ".md")))


def bundle():
    """app/ is replaced on upgrade; checkout/ holds the editable files, which an upgrade
    replaces only where the user has not changed them."""
    out = SHELL / "bundle" / "app-bundle.zip"
    out.parent.mkdir(exist_ok=True)
    spec = json.loads((ROOT / "live-files.json").read_text())
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as archive:
        for folder in APP_FOLDERS:
            for path in sorted((ROOT / folder).rglob("*")):
                relative = path.relative_to(ROOT).as_posix()
                if any(relative == x or relative.startswith(x + "/") for x in APP_EXCLUDE):
                    continue
                if path.is_file() and "__pycache__" not in path.parts:
                    archive.write(path, Path("app") / path.relative_to(ROOT))
                    if editable(relative):
                        archive.write(path, Path("checkout") / relative)
        archive.write(ROOT / "live-files.json", "checkout/live-files.json")
        written = {name for name in archive.namelist() if name.startswith("checkout/")}
        for pattern in spec["files"]:
            for name in sorted(glob.glob(pattern, root_dir=ROOT)):
                excluded = any(name == x or name.startswith(x + "/") for x in APP_EXCLUDE)
                if "checkout/" + name not in written and not excluded:
                    archive.write(ROOT / name, Path("checkout") / name)
    print("bundle", out, out.stat().st_size // 1024, "KiB")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("platform", choices=TARGETS)
    parser.add_argument("--debug", action="store_true")
    parser.add_argument("--skip-server", action="store_true", help="reuse the last server build")
    parser.add_argument("--bundle-only", action="store_true", help="only write the app file zip")
    args = parser.parse_args()
    env = dict(os.environ)
    # The toolchain the server's release image pins (Dockerfile `FROM rust:1.99`).
    env.setdefault("RUSTUP_TOOLCHAIN", "1.99.1")
    if args.platform == "android":
        android_env(env)
    bundle()
    if args.bundle_only:
        return
    # Icons are generated from the web app's own icon on every build (not committed).
    run(["cargo", "tauri", "icon", str(ROOT / "ui/app-icon.svg")], cwd=SHELL, env=env)
    if args.platform == "android" and not (SHELL / "gen/android/build.gradle.kts").exists():
        run(["cargo", "tauri", "android", "init", "--ci"], cwd=SHELL, env=env)
    binaries = SHELL / "binaries"
    binaries.mkdir(exist_ok=True)
    for triple in TARGETS[args.platform]:
        server = build_server(triple, args.debug, env) if not args.skip_server else None
        if args.platform == "android":
            abi = ANDROID_ABI[triple][0]
            jni = SHELL / "gen/android/app/src/main/jniLibs" / abi
            jni.mkdir(parents=True, exist_ok=True)
            if server:
                shutil.copy2(server, jni / "libwebagent_server.so")
        elif server:
            suffix = ".exe" if "windows" in triple else ""
            shutil.copy2(server, binaries / f"webagent-server-{triple}{suffix}")
    profile = ["--debug"] if args.debug else []
    if args.platform == "android":
        prepare_android()
        run(["cargo", "tauri", "android", "build", *profile, "--apk",
             "--target", *[ANDROID_ABI[t][1] for t in TARGETS["android"]]], cwd=SHELL, env=env)
        if not args.debug:
            sign_apks(env)
        print("outputs:", *glob.glob(str(SHELL / "gen/android/app/build/outputs/apk/**/*.apk"), recursive=True))
    else:
        triple = TARGETS[args.platform][0]
        bundles = ["--bundles", "nsis"] if args.platform == "windows" else []
        run(["cargo", "tauri", "build", *profile, "--target", triple, *bundles], cwd=SHELL, env=env)
        shell_target = Path(env.get("CARGO_TARGET_DIR", SHELL / "target"))
        print("outputs:", *glob.glob(str(shell_target / triple / "*/bundle/*/*.*"), recursive=True))


if __name__ == "__main__":
    sys.exit(main())
