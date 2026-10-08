#!/bin/sh
# Separate Phase 4 metadata gate. Runs no WebAgent app, tool, or state volume.
set -eu
cd /home/ar/webagent-dev3
case "${1:-build}" in
  build)
    tar -cf - gate.Dockerfile gate_worker.py tests/test_gate_worker.py \
      plugins/gates/publication_size_guard_v1 \
      | docker build -f gate.Dockerfile -t webagent-dev-v3-gate:local -
    ;;
  start)
    if docker inspect webagent-dev-v3-gate >/dev/null 2>&1; then
      echo "webagent-dev-v3-gate already exists; inspect before replacement" >&2
      exit 1
    fi
    mkdir -p gate-runtime
    docker run --rm --user 0:0 --network none \
      --mount type=bind,src=/home/ar/webagent-dev3/gate-runtime,dst=/run/v3/gate \
      --entrypoint /bin/sh webagent-dev-v3-gate:local \
      -c 'chown 1102:1100 /run/v3/gate && chmod 0770 /run/v3/gate'
    docker run -d --name webagent-dev-v3-gate --restart unless-stopped \
      --network none --read-only --user 1102:1100 --cap-drop ALL \
      --security-opt no-new-privileges:true --pids-limit 32 --memory 64m --cpus 0.25 \
      --mount type=bind,src=/home/ar/webagent-dev3/gate-runtime,dst=/run/v3/gate \
      --stop-timeout 5 webagent-dev-v3-gate:local
    ;;
  *) echo "Usage: sh gate-deploy.sh build|start" >&2; exit 2 ;;
esac
