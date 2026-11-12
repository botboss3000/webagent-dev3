FROM cloudflare/cloudflared@sha256:4955524aeaa379e154023d5a027c250f7b35041a2e7e950cf13f144bc9b9c5ba AS preview_tunnel
FROM rust:1.99-bookworm@sha256:e9da6f6c3ed5095fd5d33bf7cdf309830a178a8147888f1b2f76d5769bd9306c AS tested_builder
RUN mkdir -p /usr/local/share/webagent/compiler \
    && dpkg-query -W -f='${binary:Package}\t${Version}\n' | LC_ALL=C sort > /usr/local/share/webagent/compiler/before.tsv \
    && apt-get update \
    && apt-get install -y --no-install-recommends --no-upgrade mold=1.10.1+dfsg-1 \
    && dpkg-query -W -f='${binary:Package}\t${Version}\n' | LC_ALL=C sort > /usr/local/share/webagent/compiler/after.tsv \
    && test -z "$(comm -23 /usr/local/share/webagent/compiler/before.tsv /usr/local/share/webagent/compiler/after.tsv)" \
    && test "$(comm -13 /usr/local/share/webagent/compiler/before.tsv /usr/local/share/webagent/compiler/after.tsv)" = "$(printf 'mold\t1.10.1+dfsg-1')" \
    && printf '%s  /usr/bin/mold\n' 'f85b0102cabeaee34b76ab195aacfa9ff9442d670948c88f6f353dd666ea5b64' | sha256sum --check - \
    && mold --version > /usr/local/share/webagent/compiler/version.txt \
    && cc -fuse-ld=mold -Wl,--version -x c /dev/null -o /tmp/mold-linker-probe | grep -F "mold 1.10.1" \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY v3/Cargo.toml v3/Cargo.lock ./
COPY v3/build.rs ./build.rs
COPY v3/src ./src
COPY v3/crates ./crates
# COMPACTION CONFIG: build-time tests read this fixture; runtime uses the single
# host config directory mounted by deploy.sh, not an embedded configuration.
COPY v3/config ./config
COPY v3/build-service ./build-service
COPY v3/plugins ./plugins
# Runtime tests load the installed ability catalog and chat configuration.
COPY v3/plugins/abilities ./plugins/abilities
COPY v3/ui ./ui
# Bound compiler concurrency to three workers on the development host.
ENV CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 RUSTFLAGS="-C debuginfo=0 -C link-arg=-fuse-ld=mold"
# Keep Cargo downloads and artifacts across source changes; publish the tested
# binary outside the cache mount so both runtime targets copy one exact build.
RUN --mount=type=cache,id=webagent-v3-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=webagent-v3-target,target=/build/target,sharing=locked \
    cargo test --workspace --locked -- --test-threads=1 \
    && cargo build --release --locked --offline --bin webagent-v3 \
    && install -Dm755 /build/target/release/webagent-v3 /out/stable/webagent-v3 \
    && /out/stable/webagent-v3 --context-smoke \
    && readelf -p .comment /out/stable/webagent-v3 | grep -F "mold 1.10.1" \
    && cargo clean --release --package webagent-v3 --locked --offline \
    && cargo build --workspace --release --locked --no-default-features --features webagent-v3/live-context \
    && /build/target/release/webagent-v3 --context-smoke \
    && readelf -p .comment /build/target/release/webagent-v3 | grep -F "mold 1.10.1" \
    && install -Dm755 /usr/bin/mold /out/compiler/mold \
    && install -Dm644 /usr/share/doc/mold/copyright /out/compiler/copyright \
    && install -Dm644 /usr/share/doc/mold/LICENSE.gz /out/compiler/LICENSE.gz \
    && install -Dm755 /build/target/release/webagent-context-service /out/webagent-context-service \
    && install -Dm755 /build/target/release/webagent-boot-service /out/webagent-boot-service \
    && install -Dm755 /build/target/release/webagent-background-service /out/webagent-background-service \
    && install -Dm755 /build/target/release/webagent-assets-service /out/webagent-assets-service \
    && install -Dm755 /build/target/release/webagent-capability-service /out/webagent-capability-service \
    && install -Dm755 /build/target/release/webagent-provider-service /out/webagent-provider-service \
    && install -Dm755 /build/target/release/webagent-boot-host /out/webagent-boot-host \
    && install -Dm755 /build/target/release/webagent-v3 /out/webagent-v3 \
    && install -Dm755 /build/target/release/webagent-mcp /out/webagent-mcp \
    && install -Dm755 /build/target/release/webagent-diagnostics /out/webagent-diagnostics \
    && install -Dm755 /build/target/release/webagent-optimization /out/webagent-optimization \
    && install -Dm755 /build/target/release/webagent-outbound-notifications /out/modules/outbound_notifications/webagent-outbound-notifications \
    && cp /build/src/plugin_packages/outbound_notifications/package.json /out/modules/outbound_notifications/ \
    && cp /build/src/plugin_packages/outbound_notifications/page.* /out/modules/outbound_notifications/ \
    && mkdir -p /out/optimization-cargo \
    && cp -a /usr/local/cargo/registry /out/optimization-cargo/registry

RUN --mount=type=cache,id=webagent-v3-target,target=/build/target,sharing=locked \
    awk 'FNR==1 {sub(/^[^:]*: /,"");print}' /build/target/release/deps/webagent_v3-*.d /build/target/release/deps/webagent_context_contracts-*.d /build/target/release/deps/webagent_assets_contracts-*.d /build/target/release/deps/webagent_capability_contracts-*.d /build/target/release/deps/webagent_provider_contracts-*.d \
      | tr ' ' '\n' | LC_ALL=C sort -u > /out/core-inputs.txt
RUN test -s /out/core-inputs.txt
FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS voice
# Voice (webagent_media::recognizer and ::speaker load these at run time; python3 here only
# unpacks archives): the sherpa-onnx 2.0.1 C API with ONNX Runtime (Apache-2.0/MIT, the version
# its C layouts mirror); NVIDIA Parakeet TDT 0.6B v3 int8 (CC-BY-4.0) and Kokoro v1.0 in full
# precision (Apache-2.0; about 3x faster here than its int8 export) in sherpa-onnx form; Silero VAD (MIT); espeak-ng 1.52 with its English data
# (GPL-3.0, from the espeakng-loader 0.2.4 wheel) for Kokoro's phonemes. Attribution in
# models/NOTICE.
RUN apt-get update && apt-get install -y --no-install-recommends python3 ca-certificates curl bzip2 \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /opt/inference/lib /opt/inference/models && cd /tmp \
    && curl -fsSLo sherpa.tar.bz2 https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-linux-x64-shared-lib.tar.bz2 \
    && curl -fsSLo parakeet.tar.bz2 https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2 \
    && curl -fsSLo kokoro.tar.bz2 https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2 \
    && curl -fsSLo espeak.whl https://files.pythonhosted.org/packages/de/1e/25ec5ab07528c0fbb215a61800a38eca05c8a99445515a02d7fa5debcb32/espeakng_loader-0.2.4-py3-none-manylinux_2_17_x86_64.manylinux2014_x86_64.whl \
    && curl -fsSLo /opt/inference/models/silero_vad.onnx https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx \
    && printf '%s  %s\n' 532c45bbd41036d83cd70606797529aa8086c2053395a58967736c727625b120 sherpa.tar.bz2 \
        1495e91b59d7fc53ddca91f3431758ec99f8fa253fdb0e1e851be0f72b14b0f9 parakeet.tar.bz2 \
        60703707bc05ccd48d60e1eb723f38b07d6e474795a0b6a588d7220400e0f7a4 kokoro.tar.bz2 \
        08721baf27d13d461f6be6eed9a65277e70d68234ff484fd8b9897b222cdcb6d espeak.whl \
        9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6 /opt/inference/models/silero_vad.onnx | sha256sum -c - \
    && tar xjf sherpa.tar.bz2 && tar xjf parakeet.tar.bz2 && tar xjf kokoro.tar.bz2 \
    && python3 -I -m zipfile -e espeak.whl espeak \
    && mv sherpa-onnx-v2.0.1-linux-x64-shared-lib/lib/libsherpa-onnx-c-api.so sherpa-onnx-v2.0.1-linux-x64-shared-lib/lib/libonnxruntime.so /opt/inference/lib/ \
    && mkdir /opt/inference/models/parakeet /opt/inference/models/kokoro \
    && mv sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/encoder.int8.onnx sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/decoder.int8.onnx \
        sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/joiner.int8.onnx sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/tokens.txt /opt/inference/models/parakeet/ \
    && mv kokoro-multi-lang-v1_0/model.onnx kokoro-multi-lang-v1_0/voices.bin kokoro-multi-lang-v1_0/tokens.txt /opt/inference/models/kokoro/ \
    && mv espeak/espeakng_loader/libespeak-ng.so.1.52.0 /opt/inference/lib/libespeak-ng.so.1 \
    && mv espeak/espeakng_loader/espeak-ng-data /opt/inference/espeak-ng-data \
    && find /opt/inference/espeak-ng-data -maxdepth 1 -name '*_dict' ! -name en_dict -delete \
    && rm -rf /tmp/* \
    && printf '%s\n' 'parakeet/: NVIDIA parakeet-tdt-0.6b-v3 (https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3), CC-BY-4.0, converted by k2-fsa/sherpa-onnx.' \
        'kokoro/: Kokoro-82M v1.0 (https://huggingface.co/hexgrad/Kokoro-82M), Apache-2.0, converted by k2-fsa/sherpa-onnx.' \
        'silero_vad.onnx: Silero VAD (https://github.com/snakers4/silero-vad), MIT.' \
        '../espeak-ng-data, ../lib/libespeak-ng.so.1: espeak-ng 1.52 (https://github.com/espeak-ng/espeak-ng), GPL-3.0.' > /opt/inference/models/NOTICE \
    && chmod -R a+rX /opt/inference
# The local inference service (crates/modules/inference) runs BAAI bge-small-en-v1.5 (MIT)
# in its ONNX export on the ONNX Runtime above; files pinned to one revision and checked.
RUN mkdir -p /opt/inference/models/bge-small-en-v1.5 && cd /opt/inference/models/bge-small-en-v1.5 \
    && curl -fsSLo model.onnx https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/5c38ec7c405ec4b44b94cc5a9bb96e735b38267a/onnx/model.onnx \
    && curl -fsSLo vocab.txt https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/5c38ec7c405ec4b44b94cc5a9bb96e735b38267a/vocab.txt \
    && printf '%s  %s\n' 828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35 model.onnx \
        07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3 vocab.txt | sha256sum -c - \
    && printf '%s\n' 'bge-small-en-v1.5/: BAAI bge-small-en-v1.5 (https://huggingface.co/BAAI/bge-small-en-v1.5), MIT.' >> /opt/inference/models/NOTICE \
    && chmod -R a+rX /opt/inference/models/bge-small-en-v1.5

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS api
RUN apt-get update && apt-get install -y --no-install-recommends python3 ca-certificates curl tini git ripgrep build-essential pkg-config chromium chromium-driver python3-selenium valgrind heaptrack linux-perf \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 1100 v3 && useradd -u 1100 -g 1100 -M -s /usr/sbin/nologin v3app \
    && useradd -u 1101 -g 1100 -M -s /usr/sbin/nologin v3tools
COPY --from=preview_tunnel /usr/local/bin/cloudflared /usr/local/bin/cloudflared
# Named service identities: webagent (1100) runs the app, webagent-worker (1103) the tool
# workers with webagent-source (1110) for checkout writes, webagent-tools (1101) commands.
RUN groupmod -n webagent v3 && usermod -l webagent v3app && usermod -l webagent-tools v3tools \
    && groupadd -g 1110 webagent-source \
    && useradd -u 1103 -g 1100 -G webagent-source -M -s /usr/sbin/nologin webagent-worker
# Refresh inherited security packages even when the prepared runtime base is reused.
RUN apt-get update && apt-get install -y --no-install-recommends --only-upgrade libpcre2-8-0 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=tested_builder /out/webagent-v3 /usr/local/bin/webagent-v3
COPY --from=tested_builder /out/compiler/mold /usr/bin/mold
COPY --from=tested_builder /out/compiler/mold /usr/bin/ld.mold
COPY --from=tested_builder /out/compiler/copyright /usr/share/doc/mold/copyright
COPY --from=tested_builder /out/compiler/LICENSE.gz /usr/share/doc/mold/LICENSE.gz
RUN install -d -m 0755 /usr/local/share/webagent && printf 'mold\n' > /usr/local/share/webagent/compiler-linker
COPY --from=tested_builder /out/core-inputs.txt /usr/local/lib/webagent/core-inputs.txt
# Distribution artifacts are installed only by explicit module administration.
COPY --from=tested_builder /out/modules /usr/local/lib/webagent/modules
COPY --from=tested_builder /out/webagent-mcp /usr/local/bin/webagent-mcp
COPY --from=tested_builder /out/webagent-diagnostics /usr/local/bin/webagent-diagnostics
COPY --from=tested_builder /out/webagent-optimization /usr/local/bin/webagent-optimization
COPY --from=tested_builder /out/optimization-cargo /opt/optimization-cargo
COPY --from=tested_builder /usr/local/rustup/toolchains/1.99.1-x86_64-unknown-linux-gnu /opt/rust
COPY v3/sandbox.py /opt/v3/
COPY v3/build-service /opt/v3/build-service
COPY v3/plugins/abilities /opt/v3/plugins/abilities
COPY v3/plugins/engine /opt/v3/plugins/engine
COPY --from=tested_builder /out/webagent-context-service /usr/local/lib/webagent/engine/context-assembly/webagent-context-service
COPY --from=tested_builder /out/webagent-background-service /usr/local/lib/webagent/engine/background-dispatch/webagent-background-service
COPY --from=tested_builder /out/webagent-boot-service /usr/local/lib/webagent/engine/boot-supervisor/webagent-boot-service
COPY --from=tested_builder /out/webagent-assets-service /usr/local/lib/webagent/engine/context-assets/webagent-assets-service
COPY --from=tested_builder /out/webagent-capability-service /usr/local/lib/webagent/engine/capability-context/webagent-capability-service
COPY --from=tested_builder /out/webagent-provider-service /usr/local/lib/webagent/engine/provider-projection/webagent-provider-service
COPY v3/plugins/gates /opt/v3/plugins/gates
# Agent voice output and composer dictation (crates/modules/media/host/voice.rs): the
# sherpa-onnx and espeak-ng libraries and checked models come from the `voice` stage, prepared
# once as a dependency base.
COPY --from=voice /opt/inference /opt/inference
RUN chmod -R a+rX /opt/v3/plugins
RUN chmod 0755 /opt/v3 /opt/v3/plugins /opt/v3/plugins/gates /opt/v3/plugins/gates/publication_size_guard_v1 && chmod 0444 /opt/v3/plugins/gates/publication_size_guard_v1/gate.json
RUN chmod a+r /opt/v3/*.py
COPY v3 /opt/v3/source-template
COPY v3/ui /opt/v3/ui
COPY v3/ui-baseline.json /opt/v3/ui-baseline.json
COPY ui/shared/css/app1.css ui/shared/css/app2.css ui/shared/css/app3.css ui/shared/css/design-system.css ui/shared/css/index.css /opt/v3/shared-ui/shared/css/
COPY ui/main-panel/agents/agents.html ui/main-panel/agents/agents.css /opt/v3/shared-ui/main-panel/agents/
COPY ui/vendor/fonts /opt/v3/shared-ui/vendor/fonts
COPY ui/vendor/lucide /opt/v3/shared-ui/vendor/lucide
COPY ui/favicon.svg /opt/v3/shared-ui/
ENV PATH=/opt/rust/bin:$PATH
# The checkout is mounted at /code. Stored prompts, history and Files page rows still name
# /live-checkout, so it stays as a link; tool sandboxes never grant the link itself.
RUN ln -s /code /live-checkout
ENV V3_LISTEN=0.0.0.0:8080 V3_STATE_ROOT=/state V3_WORKSPACE_ROOT=/workspaces V3_SECURE_COOKIE=true
WORKDIR /state
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=3s --start-period=30s --retries=3 CMD curl --fail http://127.0.0.1:8080/health/ready || exit 1
ENTRYPOINT ["/usr/bin/tini","--"]
CMD ["/usr/local/bin/webagent-v3","--launch"]

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS loop
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 1100 v3 && useradd -u 1100 -g 1100 -M -s /usr/sbin/nologin v3loop
COPY --from=tested_builder /out/webagent-v3 /usr/local/bin/webagent-v3
RUN groupmod -n webagent v3 && usermod -l webagent v3loop
# These libraries are inherited from the pinned base rather than installed above.
RUN apt-get update && apt-get install -y --no-install-recommends --only-upgrade libpcre2-8-0 perl-base \
    && rm -rf /var/lib/apt/lists/*
COPY --from=tested_builder /out/core-inputs.txt /usr/local/lib/webagent/core-inputs.txt
# Configuration Controller tools and tool/skill ranking run here and start the local inference
# service (crates/modules/inference/host/service.rs): ONNX Runtime and bge-small from the `voice` stage.
COPY --from=voice /opt/inference/lib/libonnxruntime.so /opt/inference/lib/libonnxruntime.so
COPY --from=voice /opt/inference/models/bge-small-en-v1.5 /opt/inference/models/bge-small-en-v1.5
COPY v3/plugins/abilities /opt/v3/plugins/abilities
COPY v3/plugins/engine /opt/v3/plugins/engine
COPY --from=tested_builder /out/webagent-context-service /usr/local/lib/webagent/engine/context-assembly/webagent-context-service
COPY --from=tested_builder /out/webagent-background-service /usr/local/lib/webagent/engine/background-dispatch/webagent-background-service
COPY --from=tested_builder /out/webagent-boot-service /usr/local/lib/webagent/engine/boot-supervisor/webagent-boot-service
COPY --from=tested_builder /out/webagent-assets-service /usr/local/lib/webagent/engine/context-assets/webagent-assets-service
COPY --from=tested_builder /out/webagent-capability-service /usr/local/lib/webagent/engine/capability-context/webagent-capability-service
COPY --from=tested_builder /out/webagent-provider-service /usr/local/lib/webagent/engine/provider-projection/webagent-provider-service
COPY v3/plugins/gates /opt/v3/plugins/gates
RUN chmod -R a+rX /opt/v3/plugins
RUN chmod 0755 /opt/v3 /opt/v3/plugins /opt/v3/plugins/gates /opt/v3/plugins/gates/publication_size_guard_v1 && chmod 0444 /opt/v3/plugins/gates/publication_size_guard_v1/gate.json
ENV V3_PROCESS_ROLE=loop V3_STATE_ROOT=/state V3_WORKSPACE_ROOT=/workspaces
WORKDIR /state
# The checkout is mounted at /code. Stored prompts, history and Files page rows still name
# /live-checkout, so it stays as a link; tool sandboxes never grant the link itself.
RUN ln -s /code /live-checkout
USER 1100:1100
HEALTHCHECK --interval=5s --timeout=3s --start-period=20s --retries=3 CMD ["env","V3_PROCESS_ROLE=health","/usr/local/bin/webagent-v3"]
ENTRYPOINT ["/usr/bin/tini","--"]
CMD ["/usr/local/bin/webagent-v3"]
