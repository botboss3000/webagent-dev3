FROM cloudflare/cloudflared@sha256:0aa26e284f05e6c77ae375b8c9c11d9eb6a448fb7bcd8d40f31cb6176189eb38 AS preview_tunnel
FROM rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f AS tested_builder
WORKDIR /build
COPY v3/Cargo.toml v3/Cargo.lock ./
COPY v3/build.rs ./build.rs
COPY v3/src ./src
COPY v3/crates ./crates
# COMPACTION CONFIG: build-time tests read this fixture; runtime uses the single
# host config directory mounted by deploy.sh, not an embedded configuration.
COPY v3/config ./config
# GATE-CONFIG: only the two selected non-secret live JSON files enter the image.
COPY v3/data/config/gates ./data/config/gates
COPY v3/build-service ./build-service
COPY v3/plugins ./plugins
# Runtime tests load the installed ability catalog and chat configuration.
COPY v3/plugins/abilities ./plugins/abilities
COPY v3/ui ./ui
# Bound compiler concurrency on the shared 7 GiB development host.
ENV CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
# Keep Cargo downloads and artifacts across source changes; publish the tested
# binary outside the cache mount so both runtime targets copy one exact build.
RUN --mount=type=cache,id=webagent-v3-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=webagent-v3-target,target=/build/target,sharing=locked \
    cargo test --workspace --locked -- --test-threads=1 \
    && cargo build --release --locked --offline --bin webagent-v3 --target-dir /build/linked-target \
    && install -Dm755 /build/linked-target/release/webagent-v3 /out/stable/webagent-v3 \
    && /out/stable/webagent-v3 --context-smoke \
    && cargo build --workspace --release --locked --no-default-features --features webagent-v3/live-context \
    && /build/target/release/webagent-v3 --context-smoke \
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
FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS api
RUN apt-get update && apt-get install -y --no-install-recommends python3 ca-certificates curl tini git ripgrep build-essential pkg-config chromium chromium-driver python3-selenium valgrind heaptrack linux-perf \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 1100 v3 && useradd -u 1100 -g 1100 -M -s /usr/sbin/nologin v3app \
    && useradd -u 1101 -g 1100 -M -s /usr/sbin/nologin v3tools
COPY --from=preview_tunnel /usr/local/bin/cloudflared /usr/local/bin/cloudflared
COPY --from=tested_builder /out/webagent-v3 /usr/local/bin/webagent-v3
COPY --from=tested_builder /out/core-inputs.txt /usr/local/lib/webagent/core-inputs.txt
# Distribution artifacts are installed only by explicit module administration.
COPY --from=tested_builder /out/modules /usr/local/lib/webagent/modules
COPY --from=tested_builder /out/webagent-mcp /usr/local/bin/webagent-mcp
COPY --from=tested_builder /out/webagent-diagnostics /usr/local/bin/webagent-diagnostics
COPY --from=tested_builder /out/webagent-optimization /usr/local/bin/webagent-optimization
COPY --from=tested_builder /out/optimization-cargo /opt/optimization-cargo
COPY --from=tested_builder /usr/local/rustup/toolchains/1.90.0-x86_64-unknown-linux-gnu /opt/rust
COPY v3/worker.py v3/ability_plugin_worker.py v3/sandbox.py v3/launch.py v3/promote.py /opt/v3/
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
# Git invokes this fixed helper directly as the unprivileged tool worker.
RUN chmod 0555 /opt/v3/plugins/abilities/Administrator/git_control/askpass.py
COPY v3 /opt/v3/source-template
COPY v3/ui /opt/v3/ui
COPY v3/ui-baseline.json /opt/v3/ui-baseline.json
COPY ui/shared/css/app1.css ui/shared/css/app2.css ui/shared/css/app3.css ui/shared/css/design-system.css ui/shared/css/index.css /opt/v3/shared-ui/shared/css/
COPY ui/main-panel/agents/agents.html ui/main-panel/agents/agents.css /opt/v3/shared-ui/main-panel/agents/
COPY ui/vendor/fonts /opt/v3/shared-ui/vendor/fonts
COPY ui/vendor/lucide /opt/v3/shared-ui/vendor/lucide
COPY ui/favicon.svg /opt/v3/shared-ui/
ENV PATH=/opt/rust/bin:$PATH
ENV V3_LISTEN=0.0.0.0:8080 V3_STATE_ROOT=/state V3_WORKSPACE_ROOT=/workspaces V3_SECURE_COOKIE=true
WORKDIR /state
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=3s --start-period=30s --retries=3 CMD curl --fail http://127.0.0.1:8080/health/ready || exit 1
ENTRYPOINT ["/usr/bin/tini","--"]
CMD ["/usr/bin/python3","-I","/opt/v3/launch.py"]

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS loop
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 1100 v3 && useradd -u 1100 -g 1100 -M -s /usr/sbin/nologin v3loop
COPY --from=tested_builder /out/webagent-v3 /usr/local/bin/webagent-v3
COPY --from=tested_builder /out/core-inputs.txt /usr/local/lib/webagent/core-inputs.txt
COPY v3/plugins/abilities /opt/v3/plugins/abilities
COPY v3/plugins/engine /opt/v3/plugins/engine
COPY --from=tested_builder /out/webagent-context-service /usr/local/lib/webagent/engine/context-assembly/webagent-context-service
COPY --from=tested_builder /out/webagent-background-service /usr/local/lib/webagent/engine/background-dispatch/webagent-background-service
COPY --from=tested_builder /out/webagent-boot-service /usr/local/lib/webagent/engine/boot-supervisor/webagent-boot-service
COPY --from=tested_builder /out/webagent-assets-service /usr/local/lib/webagent/engine/context-assets/webagent-assets-service
COPY --from=tested_builder /out/webagent-capability-service /usr/local/lib/webagent/engine/capability-context/webagent-capability-service
COPY --from=tested_builder /out/webagent-provider-service /usr/local/lib/webagent/engine/provider-projection/webagent-provider-service
COPY v3/plugins/gates /opt/v3/plugins/gates
ENV V3_PROCESS_ROLE=loop V3_STATE_ROOT=/state V3_WORKSPACE_ROOT=/workspaces
WORKDIR /state
USER 1100:1100
HEALTHCHECK --interval=5s --timeout=3s --start-period=20s --retries=3 CMD ["env","V3_PROCESS_ROLE=health","/usr/local/bin/webagent-v3"]
ENTRYPOINT ["/usr/bin/tini","--"]
CMD ["/usr/local/bin/webagent-v3"]
