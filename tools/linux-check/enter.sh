#!/usr/bin/env bash
# Runs inside the glimpsfy-linux-check image before any command: copies the source run.sh put in
# /src to /work, the way a fresh checkout would look, then runs the command inside a session bus
# and a virtual X display, so tests that need a screen have one.
#
# CI (.github/workflows/ci.yml) calls it with --in-place on its own checkout: the copy is skipped
# and only the session is shared, so the runner and the container give the tests the same screen.
set -uo pipefail

if [ "${1:-}" = --in-place ]; then
  shift
else
  # Build output and symlinks from the host must not come along.
  rsync -a --delete \
    --exclude=/.git --exclude=/node_modules --exclude=/dist --exclude=/target \
    --exclude=/design --exclude=/src-tauri/gen \
    /src/ /work/ || exit 1
  cd /work || exit 1
fi

exec dbus-run-session -- xvfb-run --auto-servernum --server-args="-screen 0 1920x1080x24" "$@"
