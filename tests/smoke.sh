#!/usr/bin/env bash
# End-to-end smoke test: start the daemon under the current Wayland session,
# confirm it survives each signal it claims to handle, then shut it down
# cleanly and check the exit status.
#
# Requires a real compositor session — there is no headless substitute, since
# the daemon's first act is to connect to $WAYLAND_DISPLAY.
#
# Prints OK on success. Any other exit code is the test failing.
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ -z "${WAYLAND_DISPLAY:-}" ]]; then
  echo "SKIP: WAYLAND_DISPLAY is unset; run this under a Wayland session"
  exit 0
fi

CARGO="${CARGO:-cargo}"
BIN="target/debug/rime-popup"

"$CARGO" build

# Use a scratch config so a reload test never touches the user's real file.
WORKDIR="$(mktemp -d)"
CFG="$WORKDIR/rime-popup.toml"
LOG="$WORKDIR/daemon.log"
printf '[general]\nlog = "info"\n' >"$CFG"

cleanup() {
  if [[ -n "${PID:-}" ]] && kill -0 "$PID" 2>/dev/null; then
    kill -INT "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

"$BIN" --config "$CFG" >"$LOG" 2>&1 &
PID=$!

alive() { kill -0 "$PID" 2>/dev/null; }

# Startup: the daemon must still be running a second in, having bound its
# globals and entered the loop.
sleep 1
if ! alive; then
  echo "FAIL: daemon exited unexpectedly during startup"
  sed -n '1,40p' "$LOG"
  exit 1
fi

# SIGHUP reloads config without dropping the wayland connection.
kill -HUP "$PID"
sleep 1
if ! alive; then
  echo "FAIL: daemon died after SIGHUP"
  sed -n '1,40p' "$LOG"
  exit 1
fi

# A broken config must be rejected without taking the daemon down: a user
# saving a half-typed file should not lose their input method.
printf 'not valid toml [[[\n' >"$CFG"
kill -HUP "$PID"
sleep 1
if ! alive; then
  echo "FAIL: daemon died after SIGHUP with a broken config"
  sed -n '1,40p' "$LOG"
  exit 1
fi

# SIGUSR1 dumps state and keeps running.
kill -USR1 "$PID"
sleep 1
if ! alive; then
  echo "FAIL: daemon died after SIGUSR1"
  sed -n '1,40p' "$LOG"
  exit 1
fi

# SIGINT must unwind the loop and exit 0 — not die on the default disposition
# with 130/143, which is the regression calloop 0.12 -> 0.13 introduced.
kill -INT "$PID"
if ! wait "$PID"; then
  status=$?
  echo "FAIL: daemon exited with status $status, expected 0"
  sed -n '1,40p' "$LOG"
  exit 1
fi
PID=""

# The dump must actually have been emitted; a silent SIGUSR1 handler would
# pass every check above.
if ! grep -q "SIGUSR1 state dump" "$LOG"; then
  echo "FAIL: SIGUSR1 produced no state dump"
  sed -n '1,40p' "$LOG"
  exit 1
fi

echo "OK"