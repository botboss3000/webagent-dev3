FROM rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f
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
