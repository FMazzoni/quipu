#!/usr/bin/env bash
# Deployment smoke for the container image: prove `qp serve` deploys, with the
# store on local disk.
#
# It names no host and no real volume. The port is an environment variable with
# a placeholder default, the volume and container names are prefixed and torn
# down, and the host side of the publish is bound to loopback — see
# "Amendment: publishing a container port" in docs/process-contract.md.
#
#   ./docker-smoke.sh              # placeholder port 8787
#   QP_SMOKE_PORT=18787 ./docker-smoke.sh
#
# Requires: docker, curl, and a release `qp` on this machine to act as the
# OUTSIDE client (built here if absent). The outside client is the point: the
# checks below must be driven by a process that is not in the container and has
# no store of its own.
set -uo pipefail

PORT="${QP_SMOKE_PORT:-8787}"
IMAGE="${QP_SMOKE_IMAGE:-quipu:smoke}"
VOL="qp-smoke-store-$$"
NAME="qp-smoke-$$"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
QP="$ROOT/target/release/qp"
CLIENT_DIR="$(mktemp -d)"

fails=0
ok()   { printf '  ok    %s\n' "$1"; }
bad()  { printf '  FAIL  %s\n' "$1"; fails=$((fails + 1)); }
step() { printf '\n== %s\n' "$1"; }
check(){ if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (want '$3', got '$2')"; fi; }

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1
  docker volume rm -f "$VOL" >/dev/null 2>&1
  rm -rf "$CLIENT_DIR"
}
trap cleanup EXIT

# The outside client runs with no store of its own and reaches the board with
# two environment variables and nothing else. That is the supported shape for a
# machine that has no store, and asserting it here keeps it honest.
qpc() { ( cd "$CLIENT_DIR" && QP_SERVER="http://127.0.0.1:$PORT" "$QP" "$@" ); }

step "build"
[ -x "$QP" ] || cargo build --release --jobs 2 || exit 1
docker build -t "$IMAGE" "$ROOT" >/dev/null || { bad "docker build"; exit 1; }
ok "image $IMAGE"

step "the store is on a named volume on local disk — asserted, not assumed"
docker volume create "$VOL" >/dev/null
driver=$(docker volume inspect -f '{{.Driver}}' "$VOL")
check "volume driver is local" "$driver" "local"
opts=$(docker volume inspect -f '{{if .Options}}{{len .Options}}{{else}}0{{end}}' "$VOL")
# A `local` volume can still be an NFS or SMB mount through driver options, so
# the driver name alone proves nothing.
check "volume carries no driver options" "$opts" "0"
fstype=$(docker run --rm -v "$VOL":/mnt --entrypoint stat "$IMAGE" -f -c %T /mnt)
case "$fstype" in
  nfs|nfs4|smb2|cifs|fuseblk|fuse*|9p|afs|ncpfs|coda|gfs*|ocfs2)
    bad "store filesystem is local disk (got network fs '$fstype')" ;;
  *) ok "store filesystem is local disk ($fstype)" ;;
esac

step "a named project whose store is missing refuses to start"
# Per the startup rule: the store set is fixed before the listener accepts, so
# an unresolvable project is a non-zero exit, never a container that comes up
# healthy and answers no_store forever.
out=$(docker run --rm -v "$VOL":/var/lib/quipu "$IMAGE" \
        --bind 0.0.0.0:8787 --project no-such-project 2>&1); rc=$?
check "exit 6 (no_store)" "$rc" "6"
case "$out" in *registry*|*"no-such-project"*) ok "names the project it could not resolve" ;;
  *) bad "names the project it could not resolve (got: $out)" ;; esac

step "the container starts and health answers"
docker run --rm -v "$VOL":/var/lib/quipu --entrypoint qp "$IMAGE" init >/dev/null \
  || { bad "init store on the volume"; exit 1; }
docker run -d --name "$NAME" -p "127.0.0.1:$PORT:8787" -v "$VOL":/var/lib/quipu \
  "$IMAGE" >/dev/null || { bad "start container"; exit 1; }
health=""
for _ in $(seq 1 50); do
  health=$(curl -fsS "http://127.0.0.1:$PORT/healthz" 2>/dev/null) && break
  sleep 0.2
done
case "$health" in
  *'"version"'*) ok "GET /healthz answers ($health)" ;;
  *) bad "GET /healthz answers (got: '$health')"; exit 1 ;;
esac
case "$health" in *'"projects"'*) ok "health names the projects opened at startup" ;;
  *) bad "health names the projects opened at startup" ;; esac

step "an outside client drives the core verbs through the published port"
id=$(qpc add "smoke ticket" --json | sed -n 's/.*"display_id":"\([^"]*\)".*/\1/p')
case "$id" in QP-*) ok "add → $id" ;; *) bad "add returned a display id (got '$id')"; exit 1 ;; esac
qpc list --json | grep -q "\"$id\"" && ok "list sees it" || bad "list sees it"
taken=$(qpc take --as smoke-worker --json | sed -n 's/.*"display_id":"\([^"]*\)".*/\1/p')
check "take → $id" "$taken" "$id"
qpc claim "$id" --as smoke-worker >/dev/null && ok "claim" || bad "claim"
qpc complete "$id" --as smoke-worker >/dev/null && ok "complete" || bad "complete"
state=$(qpc show "$id" --json | sed -n 's/.*"state":"\([^"]*\)".*/\1/p' | head -1)
check "state is done" "$state" "done"
left=$(ls -A "$CLIENT_DIR" | wc -l)
check "the client left nothing on its own disk" "$left" "0"

step "restart: committed state survives, and a client in the gap sees the transport kind"
docker stop "$NAME" >/dev/null
gap=$(qpc list --json 2>&1); rc=$?
check "exit 7 while the front door is down" "$rc" "7"
case "$gap" in *'"kind":"unavailable"'*) ok "kind is unavailable" ;;
  *) bad "kind is unavailable (got: $gap)" ;; esac
docker start "$NAME" >/dev/null
for _ in $(seq 1 50); do curl -fsS "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && break; sleep 0.2; done
after=$(qpc show "$id" --json | sed -n 's/.*"state":"\([^"]*\)".*/\1/p' | head -1)
check "committed state survived the restart" "$after" "done"

step "result"
if [ "$fails" -eq 0 ]; then echo "docker smoke: PASS"; else echo "docker smoke: $fails FAILED"; fi
exit $((fails > 0))
