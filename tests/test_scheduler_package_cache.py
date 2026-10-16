"""Verify Debian source confinement and atomic, checksum-bound archive reuse."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import types
import unittest

spec = importlib.util.spec_from_file_location("apt_prefetch_under_test", Path(__file__).resolve().parents[1] / "build-service/apt_prefetch.py")
cache = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cache)


class ArchiveCacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.payload = b"verified archive fixture"
        self.entry = {"filename": "probe_1.0+v1_amd64.deb", "bytes": len(self.payload),
                      "sha256": hashlib.sha256(self.payload).hexdigest(),
                      "url": "https://deb.debian.org/debian/pool/main/p/probe/probe_1.0%2bv1_amd64.deb"}

    def line(self, url=None, digest=None, filename=None):
        return repr(url or self.entry["url"]) + " " + (filename or self.entry["filename"]) + " " + str(self.entry["bytes"]) + " SHA256:" + (digest or self.entry["sha256"])

    def test_signed_selection_retains_encoded_names_and_upgrades_transport(self):
        line = self.line(self.entry["url"].replace("https:", "http:"))
        selected = cache.selections(line)[0]
        self.assertEqual(selected, self.entry)
        self.assertEqual(cache.selections(line + "\n" + line), [self.entry])

    def test_signed_epoch_archive_name_matches_epochless_pool_name(self):
        filename = "git_1%3a2.39.5-0+deb12u3_amd64.deb"
        url = "http://deb.debian.org/debian/pool/main/g/git/git_2.39.5-0%2bdeb12u3_amd64.deb"
        entry = cache.selections(self.line(url, filename=filename))[0]
        self.assertEqual(entry["filename"], filename)
        self.assertEqual(entry["url"], url.replace("http:", "https:"))
        with self.assertRaises(ValueError):
            cache.selections(self.line(url.replace("git_2.39", "git_9.39"), filename=filename))

    def test_unapproved_sources_names_and_weak_digests_fail_closed(self):
        for line in (
            self.line("https://deb.debian.org.evil.invalid/debian/pool/p.deb"),
            self.line("https://user:password@deb.debian.org/debian/pool/p.deb"),
            self.line("https://deb.debian.org/debian/pool/../p.deb"),
            self.line("https://deb.debian.org/debian/pool/%2e%2e/p.deb"),
            self.line(filename="../probe.deb"),
            self.line().replace("SHA256:", "MD5Sum:"),
            self.line(digest="f" * 63),
        ):
            with self.subTest(line=line), self.assertRaises(ValueError):
                cache.selections(line)

    def test_verified_archive_reuse_does_not_touch_or_download(self):
        target = self.root / self.entry["filename"]
        target.write_bytes(self.payload)
        stamp = target.stat().st_mtime_ns
        def forbid(*args, **kwargs):
            raise AssertionError("Cached bytes must not trigger network")
        self.assertEqual(cache.fetch(self.entry, self.root, command=forbid)["outcome"], "cached")
        self.assertEqual(target.stat().st_mtime_ns, stamp)

    def test_seed_requires_exact_hash_before_reuse(self):
        seed = self.root / "seed"
        seed.mkdir()
        (seed / self.entry["filename"]).write_bytes(self.payload)
        def forbid(*args, **kwargs):
            raise AssertionError("Verified seed must not trigger network")
        archives = self.root / "archives"
        self.assertEqual(cache.fetch(self.entry, archives, seed, forbid)["outcome"], "seeded")
        self.assertEqual((archives / self.entry["filename"]).read_bytes(), self.payload)

    def test_corrupt_same_size_archive_is_replaced_only_after_verified_download(self):
        target = self.root / self.entry["filename"]
        target.write_bytes(b"x" * self.entry["bytes"])
        def download(argv, **kwargs):
            self.assertNotIn("--insecure", argv)
            Path(argv[argv.index("--output") + 1]).write_bytes(self.payload)
            self.assertEqual(target.read_bytes(), b"x" * self.entry["bytes"])
            return types.SimpleNamespace(returncode=0)
        self.assertEqual(cache.fetch(self.entry, self.root, command=download)["outcome"], "downloaded")
        self.assertEqual(target.read_bytes(), self.payload)

    def test_failed_or_corrupt_download_never_becomes_final_archive(self):
        target = self.root / self.entry["filename"]
        for code in (1, 0):
            def bad(argv, **kwargs):
                Path(argv[argv.index("--output") + 1]).write_bytes(b"partial")
                return types.SimpleNamespace(returncode=code)
            with self.subTest(exit=code), self.assertRaises((RuntimeError, ValueError)):
                cache.fetch(self.entry, self.root, command=bad)
            self.assertFalse(target.exists())

    def test_symlink_cache_cannot_redirect_download(self):
        other = self.root / "other"
        other.mkdir()
        archives = self.root / "archives"
        archives.symlink_to(other, target_is_directory=True)
        with self.assertRaises(ValueError):
            cache.fetch(self.entry, archives)

    def test_parallel_downloads_are_bounded(self):
        for workers in (0, 5, True):
            with self.subTest(workers=workers), self.assertRaises(ValueError):
                cache.prefetch([], self.root, workers=workers)

    def test_plan_preserves_signature_checks_and_strong_hashes(self):
        def fake(argv, **kwargs):
            self.assertIn("Acquire::ForceHash=SHA256", argv)
            self.assertIn("APT::Get::AllowUnauthenticated=false", argv)
            return types.SimpleNamespace(returncode=0, stdout=self.line())
        self.assertEqual(cache.plan(["chromium-common"], fake), [self.entry])
        with self.assertRaises(ValueError):
            cache.plan(["--allow-unauthenticated"], fake)


class RuntimeRecipeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = Path(__file__).resolve().parents[1] / "build-service/packaging_cache.py"
        module_spec = importlib.util.spec_from_file_location("runtime_cache_under_test", path)
        cls.runtime = importlib.util.module_from_spec(module_spec)
        module_spec.loader.exec_module(cls.runtime)
        cls.dockerfile = (Path(__file__).resolve().parents[1] / "Dockerfile").read_text()

    def test_os_base_reuse_ignores_unrelated_application_copy_edits(self):
        first = self.runtime.runtime_recipe(self.dockerfile, "api", b"helper")
        changed = self.dockerfile.replace("COPY v3/ui /opt/v3/ui", "COPY v3/ui /opt/v3/alternate-ui")
        self.assertEqual(first["key"], self.runtime.runtime_recipe(changed, "api", b"helper")["key"])
        for modified in (
            self.dockerfile.replace("python3-selenium valgrind", "python3-selenium jq valgrind"),
        ):
            self.assertNotEqual(first["key"], self.runtime.runtime_recipe(modified, "api", b"helper")["key"])
        self.assertNotEqual(first["key"], self.runtime.runtime_recipe(self.dockerfile, "api", b"new helper")["key"])
        self.assertNotEqual(first["key"], self.runtime.runtime_recipe(self.dockerfile, "api", b"helper", generation=2)["key"])

    def test_loop_base_does_not_rebuild_for_api_prefetch_helper_change(self):
        first = self.runtime.runtime_recipe(self.dockerfile, "loop", b"one")
        self.assertEqual(first["key"], self.runtime.runtime_recipe(self.dockerfile, "loop", b"two")["key"])

    def test_prepared_api_contains_runtime_dependencies_not_application_source(self):
        recipe = self.runtime.runtime_recipe(self.dockerfile, "api", b"helper")
        self.assertIn("sharing=locked", recipe["dockerfile"])
        self.assertIn("--no-download", recipe["dockerfile"])
        self.assertIn("/opt/rust", recipe["dockerfile"])
        self.assertIn("/usr/local/bin/cloudflared", recipe["dockerfile"])
        self.assertNotIn("COPY v3/src", recipe["dockerfile"])
        self.assertNotIn("rm -rf /var/lib/apt/lists", recipe["dockerfile"])

    def test_application_packaging_has_no_package_download_or_registry_base(self):
        output = self.runtime.package_dockerfile(self.dockerfile, "api")
        self.assertNotIn("apt-get", output)
        self.assertNotIn("FROM debian", output)
        self.assertNotIn("FROM rust", output)
        self.assertNotIn("FROM cloudflare", output)
        self.assertIn("FROM webagent_runtime_api AS api", output)
        self.assertIn("FROM webagent_runtime_loop AS loop", output)
        self.assertIn("COPY --from=tested_builder /out/webagent-v3", output)
        self.assertIn('CMD ["/usr/bin/python3","-I","/opt/v3/launch.py"]', output)

    def test_unknown_stage_and_repository_boundaries_require_review(self):
        for text in (
            self.dockerfile + "\nFROM alpine:latest AS surprise\n",
            self.dockerfile.replace("debian:bookworm-slim@sha256:", "unapproved.invalid/debian@sha256:"),
            self.dockerfile.replace("RUN apt-get update && apt-get install", "RUN arbitrary-init && apt-get install", 1),
        ):
            with self.subTest(text=text[:30]), self.assertRaises(ValueError):
                self.runtime.runtime_recipe(text, "api", b"helper")

    def layout(self, root, role="api", recipe="a" * 64):
        import json
        blobs = root / "blobs/sha256"
        blobs.mkdir(parents=True)
        def put(value):
            data = value if isinstance(value, bytes) else json.dumps(value, sort_keys=True).encode()
            digest = hashlib.sha256(data).hexdigest()
            (blobs / digest).write_bytes(data)
            return {"digest": "sha256:" + digest, "size": len(data)}
        layer = put(b"opaque fixture layer")
        config = put({"architecture": "amd64", "os": "linux",
                      "config": {"Labels": {"webagent.runtime-base.role": role,
                                           "webagent.runtime-base.recipe": recipe}},
                      "rootfs": {"diff_ids": ["sha256:" + "b" * 64]}})
        manifest = put({"schemaVersion": 2, "config": config, "layers": [layer]})
        (root / "oci-layout").write_text(json.dumps({"imageLayoutVersion": "1.0.0"}))
        (root / "index.json").write_text(json.dumps({"schemaVersion": 2, "manifests": [manifest]}))
        return blobs / layer["digest"].split(":")[1]

    def test_oci_bytes_and_recipe_are_both_authoritative(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            layer = self.layout(root)
            result = self.runtime.verify_layout(root, "api", "a" * 64)
            self.assertTrue(result["manifest_digest"].startswith("sha256:"))
            with self.assertRaises(ValueError):
                self.runtime.verify_layout(root, "loop", "a" * 64)
            with self.assertRaises(ValueError):
                self.runtime.verify_layout(root, "api", "c" * 64)
            data = bytearray(layer.read_bytes())
            data[0] ^= 1
            layer.write_bytes(data)
            with self.assertRaisesRegex(ValueError, "checksum"):
                self.runtime.verify_layout(root, "api", "a" * 64)

    def test_oci_symlink_never_becomes_a_reusable_base(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "base"
            root.mkdir()
            self.layout(root)
            link = Path(directory) / "alias"
            link.symlink_to(root, target_is_directory=True)
            with self.assertRaises(ValueError):
                self.runtime.verify_layout(link, "api", "a" * 64)

if __name__ == "__main__":
    unittest.main()
