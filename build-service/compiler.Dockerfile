FROM rust:1.99-bookworm@sha256:e9da6f6c3ed5095fd5d33bf7cdf309830a178a8147888f1b2f76d5769bd9306c
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
    && rustup component add rustfmt clippy \
    && cargo fmt --version && cargo clippy --version \
    && rm -rf /var/lib/apt/lists/*
