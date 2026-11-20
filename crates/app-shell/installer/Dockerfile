# Installer builder for production releases: Linux (.deb, AppImage), Windows (NSIS .exe) and
# Android (.apk) from one Linux image, run on the broker sidekick. macOS builds on GitHub Actions.
# Pinned to the server's Rust image so the core server compiles with the same toolchain.
FROM rust:1.99-bookworm@sha256:e9da6f6c3ed5095fd5d33bf7cdf309830a178a8147888f1b2f76d5769bd9306c

RUN apt-get update && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev patchelf file \
        xdg-utils squashfs-tools desktop-file-utils \
        mingw-w64 nsis openjdk-17-jdk-headless unzip zip ca-certificates curl python3 \
    && rm -rf /var/lib/apt/lists/*

# Bookworm's NSIS 3.08 lacks this header (NSIS 3.09+), which Tauri's installer script includes.
COPY RestartManager.nsh /usr/share/nsis/Include/Win/RestartManager.nsh

RUN rustup toolchain install 1.99.1 --profile minimal \
    && rustup target add --toolchain 1.99.1 \
        x86_64-pc-windows-gnu aarch64-linux-android x86_64-linux-android
RUN cargo install tauri-cli --version 2.12.6 --locked && rm -rf /usr/local/cargo/registry

# Android SDK and NDK at the versions the app has been built and tested with. The license
# file is the SDK license already accepted for this project's Android builds.
ENV ANDROID_HOME=/opt/android NDK_HOME=/opt/android/ndk/27.2.12479018
COPY android-licenses /opt/android/licenses
RUN curl -fsSL -o /tmp/tools.zip https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip \
    && mkdir -p /opt/android/cmdline-tools && unzip -q /tmp/tools.zip -d /opt/android/cmdline-tools \
    && mv /opt/android/cmdline-tools/cmdline-tools /opt/android/cmdline-tools/latest && rm /tmp/tools.zip \
    && /opt/android/cmdline-tools/latest/bin/sdkmanager --sdk_root=/opt/android \
        "platform-tools" "platforms;android-37.0" "build-tools;36.0.0" "ndk;27.2.12479018" \
    && /opt/android/cmdline-tools/latest/bin/sdkmanager --sdk_root=/opt/android "platforms;android-36" \
    && test -d /opt/android/platforms/android-36 \
    && rm -rf /opt/android/.temp /root/.android/cache

ENV JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64 RUSTUP_TOOLCHAIN=1.99.1
WORKDIR /src
