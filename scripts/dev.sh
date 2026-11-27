#!/bin/bash
# Development launcher for Apple Silicon macOS (including its Bash 3.2).
set -u

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P) || exit 1
command -v cargo >/dev/null 2>&1 || {
    echo 'dev.sh: required command not found: cargo' >&2
    exit 1
}

build_pid=
signal_status=0
cleanup() {
    local status=$? i
    trap '' INT TERM HUP
    if [[ -n $build_pid ]]; then
        kill -TERM -- "-$build_pid" 2>/dev/null || :
        for ((i=0; i<50; i++)); do
            kill -0 -- "-$build_pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL -- "-$build_pid" 2>/dev/null || :
        wait "$build_pid" 2>/dev/null || :
    fi
    ((signal_status == 0)) || status=$signal_status
    exit "$status"
}
trap cleanup EXIT
# Record cancellation even if it arrives before the build PID is captured.
trap 'signal_status=130' INT
trap 'signal_status=143' TERM
trap 'signal_status=129' HUP

# Keep the build in its own process group and retain the caller's directory.
set -m
(cd -- "$repo" && exec cargo build --locked --manifest-path "$repo/Cargo.toml" --target-dir "$repo/target" --bin cognix) &
build_pid=$!
set +m
((signal_status == 0)) || exit "$signal_status"
wait "$build_pid"
status=$?
((signal_status == 0)) || exit "$signal_status"
build_pid=
((status == 0)) || exit "$status"

exec "$repo/target/debug/cognix" --dev-provision-model "$@"
