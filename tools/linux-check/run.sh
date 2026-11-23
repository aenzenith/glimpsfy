#!/usr/bin/env bash
# CI's Linux checks in Docker, without a Linux machine.
#
#   tools/linux-check/run.sh [command...]
#
# The repository is copied into the container (not mounted, so it works with Colima and any
# other VM that only shares some folders), without .git, node_modules, dist, target or design.
# With a command (e.g. `cargo test -p glimpsfy-capture`, or `bash`) the container runs that
# instead of the checks, in /work, inside a session bus and a virtual X display. Cargo's target
# dir, its registry and npm's cache live in named volumes, so later runs are incremental and the
# host's target is never touched. LINUX_CHECK_SRC names another copy of the repository to check
# instead of this one. Clean up with:
#
#   docker image rm glimpsfy-linux-check
#   docker volume rm glimpsfy-linux-check-target glimpsfy-linux-check-cargo glimpsfy-linux-check-npm
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "${LINUX_CHECK_SRC:-$here/../..}" && pwd)

image=glimpsfy-linux-check
docker build --quiet --tag "$image" "$here" >/dev/null

tty=()
[ -t 0 ] && [ -t 1 ] && tty=(--interactive --tty)

# The wlr portal keeps its screen cast buffers in /dev/shm. Docker's 64 MB fill up after a few
# casts of a 1080p output, and the portal then dies of SIGBUS, so the container gets more.
container=$(docker create ${tty[@]+"${tty[@]}"} \
  --shm-size=512m \
  --volume glimpsfy-linux-check-target:/cargo-target \
  --volume glimpsfy-linux-check-cargo:/usr/local/cargo/registry \
  --volume glimpsfy-linux-check-npm:/root/.npm \
  "$image" "$@")
trap 'docker rm --force "$container" >/dev/null' EXIT

COPYFILE_DISABLE=1 tar -C "$src" -cf - --no-xattrs --no-acls \
  --exclude=./.git --exclude=./node_modules --exclude=./dist --exclude=./target \
  --exclude=./design --exclude=./src-tauri/gen \
  . | docker cp --quiet - "$container:/src"

if ((${#tty[@]})); then
  docker start --attach --interactive "$container"
else
  docker start --attach "$container"
fi
