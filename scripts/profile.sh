#!/usr/bin/env bash
#
# Stack-level profiling for CrabPort on macOS.
#
# The app ships stripped, so macOS's sampler can only show addresses inside it.
# This wraps `sample` (no extra installs, no sudo for your own process) around
# a profiling build, then reduces the call tree to a flat self-time table with
# scripts/flat_profile.py.
#
# Usage:
#   scripts/profile.sh                     # profile the running CrabPort, 10s
#   scripts/profile.sh 20                  # sample for 20 seconds
#   scripts/profile.sh --pid 1234          # attach to a specific process
#   scripts/profile.sh --build             # build --profile profiling first
#   scripts/profile.sh --build --run       # ...and launch it (opens a window)
#
# Typical workflow:
#   1. scripts/profile.sh --build --run     (a profiling build appears)
#   2. reproduce the heavy scenario in that window
#   3. scripts/profile.sh --pid "$(pgrep -f 'target/profiling/CrabPort' | head -1)"

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
seconds=10
pid=""
build=0
run=0
out_dir="${TMPDIR:-/tmp}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --pid) pid="$2"; shift 2 ;;
        --build) build=1; shift ;;
        --run) run=1; shift ;;
        --out) out_dir="$2"; shift 2 ;;
        -h|--help) sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        ''|*[!0-9]*) echo "unknown argument: $1" >&2; exit 2 ;;
        *) seconds="$1"; shift ;;
    esac
done

if [[ "$(uname)" != "Darwin" ]]; then
    echo "this script uses macOS 'sample'; on Linux use 'perf record -p <pid>' or samply" >&2
    exit 1
fi

if [[ $build -eq 1 ]]; then
    echo "building --profile profiling (symbols kept)…" >&2
    cargo build --profile profiling --manifest-path "$repo_root/Cargo.toml"
fi

if [[ $run -eq 1 ]]; then
    binary="$repo_root/target/profiling/CrabPort"
    [[ -x "$binary" ]] || { echo "no profiling build at $binary — pass --build" >&2; exit 1; }
    echo "launching $binary" >&2
    "$binary" >/dev/null 2>&1 &
    sleep 2
    pid="$!"
fi

if [[ -z "$pid" ]]; then
    pid="$(pgrep -f 'target/(profiling|release)/CrabPort|/CrabPort.app/Contents/MacOS/CrabPort' | head -1 || true)"
    [[ -n "$pid" ]] || { echo "no CrabPort process found — start it, or pass --pid" >&2; exit 1; }
fi

if ! ps -p "$pid" >/dev/null 2>&1; then
    echo "pid $pid is not running" >&2
    exit 1
fi

report="$out_dir/crabport-sample-$(date +%Y%m%d-%H%M%S).txt"
echo "sampling pid $pid for ${seconds}s → $report" >&2
sample "$pid" "$seconds" -mayDie -file "$report" >/dev/null

echo
python3 "$repo_root/scripts/flat_profile.py" "$report" 25
echo
echo "raw call tree: $report" >&2
