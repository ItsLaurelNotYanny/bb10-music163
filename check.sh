#!/usr/bin/env bash
# Project-gate: portable shell syntax plus isolated Python standard-library tests.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
FAST=0
case "${1:-}" in
    "") [ "$#" -eq 0 ] || { echo "ERROR: unexpected arguments" >&2; exit 2; } ;;
    --fast) [ "$#" -eq 1 ] || { echo "ERROR: unexpected arguments" >&2; exit 2; }; FAST=1 ;;
    *) echo "Usage: ./check.sh [--fast]" >&2; exit 2 ;;
esac
MODE=full
[ "$FAST" -eq 0 ] || MODE=fast
umask 077
LOG=$(mktemp "${TMPDIR:-/tmp}/project-gate.XXXXXX")
trap 'echo "ERROR: gate interrupted; log=$LOG" >&2; exit 2' INT TERM
need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "ERROR: missing required tool: $1; log=$LOG" >&2
        exit 2
    fi
}
COUNT=0
run() {
    local label=$1 status
    shift
    printf '\n--- %s ---\n' "$label" >>"$LOG"
    if "$@" >>"$LOG" 2>&1; then
        COUNT=$((COUNT + 1))
    else
        status=$?
        printf 'FAIL: %s (exit=%s); log=%s\n' "$label" "$status" "$LOG" >&2
        tail -n 60 "$LOG" | { head -c 8000; cat >/dev/null; } >&2
        printf '\n' >&2
        case "$status" in 126|127) exit 2 ;; *) exit 1 ;; esac
    fi
}
need bash
need sh
need cargo
run 'Gate shell syntax' bash -n check.sh
for script in scripts/*.sh; do
    [ -f "$script" ] || continue
    case "$(head -n 1 "$script")" in
        *bash*)
            run "Bash syntax ($script)" bash -n "$script"
            ;;
        *)
            run "POSIX shell syntax ($script)" sh -n "$script"
            ;;
    esac
done
run 'Rust workspace check' cargo check --workspace --all-targets
if [ "$FAST" -eq 0 ]; then
    need python3
    need awk
    if [ ! -f fixtures/audio/track_long_30min.mp3 ] && command -v ffmpeg >/dev/null 2>&1; then
        run 'Generate synthetic audio fixtures' sh scripts/generate-test-audio.sh
    fi
    run 'M0 server Python syntax' python3 -m py_compile scripts/m0_server.py
    run 'Python regression tests' env PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v
    run 'Independent shared crate tests (provider-model)' cargo test -p provider-model
    run 'Independent shared crate tests (media-relay)' cargo test -p media-relay
    run 'Independent shared crate tests (playback-session)' cargo test -p playback-session
    run 'Rust workspace test suite' cargo test --workspace
fi
[ "$COUNT" -gt 0 ] || { echo "ERROR: no checks ran; log=$LOG" >&2; exit 2; }
printf 'OK: project-gate (%s, checks=%s)\n' "$MODE" "$COUNT"
rm -f -- "$LOG"
