#!/bin/bash
# Development launcher for Apple Silicon macOS (including its Bash 3.2).
set -u

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P) || exit 1
endpoint=${COGNIX_OLLAMA_URL-http://127.0.0.1:11434}
# Read only the endpoint override; leave the original argv untouched for Cognix.
# Skip values of the other CLI options, even when a value looks like an option.
read_endpoint() {
    while (($#)); do
        case $1 in
            --ollama-url=*) endpoint=${1#*=} ;;
            --ollama-url)
                (($# >= 2)) || { echo 'dev.sh: --ollama-url needs a value' >&2; exit 1; }
                endpoint=$2; shift ;;
            --root|--model|--task|--allow-command|--max-steps|--num-ctx|--model-timeout|--command-timeout|--output-limit)
                (($# < 2)) || shift ;;
            --) break ;;
        esac
        shift
    done
}
read_endpoint "$@"
if [[ ! $endpoint =~ ^http://(127\.0\.0\.1|localhost|\[::1\])(:[0-9]+)?/?$ ]]; then
    echo 'dev.sh: Ollama URL must be a local HTTP origin (localhost/loopback)' >&2
    exit 1
fi
endpoint=${endpoint%/}
timeout=${COGNIX_DEV_STARTUP_TIMEOUT:-30}
if [[ ! $timeout =~ ^[1-9][0-9]*$ ]] || ((${#timeout} > 3)) || ((timeout > 300)); then
    echo 'dev.sh: COGNIX_DEV_STARTUP_TIMEOUT must be 1–300 seconds' >&2
    exit 1
fi
for program in cargo curl /usr/bin/plutil; do
    command -v "$program" >/dev/null 2>&1 || {
        echo "dev.sh: required command not found: $program" >&2; exit 1;
    }
done
scratch=$(mktemp -d "${TMPDIR:-/tmp}/cognix-dev.XXXXXX") || exit 1
ollama_pid=
build_pid=
cognix_pid=
provision_pid=
signal_status=0

# Only build/Ollama jobs get their own process groups. Cognix stays in the
# terminal's foreground group so its interactive input and approvals work.
cancel() {
    signal_status=$1
    [[ -z $cognix_pid ]] || kill -INT "$cognix_pid" 2>/dev/null || :
    [[ -z $provision_pid ]] || kill -INT "$provision_pid" 2>/dev/null || :
    [[ -z $build_pid ]] || kill -TERM -- "-$build_pid" 2>/dev/null || :
    [[ -z $ollama_pid ]] || kill -TERM -- "-$ollama_pid" 2>/dev/null || :
}
cleanup() {
    local status=$? i alive
    trap '' INT TERM HUP
    [[ -z $cognix_pid ]] || kill -INT "$cognix_pid" 2>/dev/null || :
    [[ -z $provision_pid ]] || kill -INT "$provision_pid" 2>/dev/null || :
    [[ -z $build_pid ]] || kill -TERM -- "-$build_pid" 2>/dev/null || :
    [[ -z $ollama_pid ]] || kill -TERM -- "-$ollama_pid" 2>/dev/null || :
    # Bound cleanup too: a stuck child cannot hold the launcher open forever.
    for ((i=0; i<50; i++)); do
        alive=0
        [[ -z $cognix_pid ]] || { kill -0 "$cognix_pid" 2>/dev/null && alive=1; }
        [[ -z $provision_pid ]] || { kill -0 "$provision_pid" 2>/dev/null && alive=1; }
        [[ -z $build_pid ]] || { kill -0 -- "-$build_pid" 2>/dev/null && alive=1; }
        [[ -z $ollama_pid ]] || { kill -0 -- "-$ollama_pid" 2>/dev/null && alive=1; }
        ((alive)) || break
        sleep 0.1
    done
    if [[ -n $cognix_pid ]]; then
        kill -KILL "$cognix_pid" 2>/dev/null || :
        wait "$cognix_pid" 2>/dev/null || :
    fi
    for pid in "$build_pid" "$ollama_pid"; do
        [[ -n $pid ]] || continue
        kill -KILL -- "-$pid" 2>/dev/null || :
        wait "$pid" 2>/dev/null || :
    done
    if [[ -n $provision_pid ]]; then
        kill -KILL "$provision_pid" 2>/dev/null || :
        wait "$provision_pid" 2>/dev/null || :
    fi
    rm -f "$scratch/version" "$scratch/curl-error"
    # Keep startup logs for inspection; remove the directory when none exists.
    rmdir "$scratch" 2>/dev/null || :
    ((signal_status == 0)) || status=$signal_status
    exit "$status"
}
trap cleanup EXIT
# Handlers record cancellation rather than exiting between spawn and $! capture.
trap 'cancel 130' INT
trap 'cancel 143' TERM
trap 'cancel 129' HUP
check_canceled() { ((signal_status == 0)) || exit "$signal_status"; }
fail() {
    echo "dev.sh: $*" >&2
    [[ ! -f $scratch/ollama.log ]] || echo "dev.sh: Ollama startup log: $scratch/ollama.log" >&2
    exit 1
}

# 0 = compatible server, 2 = connection refused, 1 = incompatible/unresponsive.
# Do not follow redirects or consult proxies. Only a refused connection permits
# auto-start; an HTTP listener or ambiguous network failure is never assumed safe.
probe() {
    local code result version body
    code=$(curl --silent --show-error --noproxy '*' --proto '=http' \
        --connect-timeout 1 --max-time 1 --max-filesize 65536 \
        --output "$scratch/version" --write-out '%{http_code}' \
        "$endpoint/api/version" 2>"$scratch/curl-error")
    result=$?
    check_canceled
    ((result != 7)) || return 2
    ((result == 0)) && [[ $code == 200 ]] || return 1
    # Require a JSON object opening with a quoted key and colon. plutil also
    # accepts XML and OpenStep plists, which are not Ollama API responses.
    body=$(LC_ALL=C tr -d '[:space:]' < "$scratch/version")
    [[ $body =~ ^\{\"[^\"]*\": ]] || return 1
    version=$(/usr/bin/plutil -extract version raw -expect string -o - "$scratch/version" 2>/dev/null) || return 1
    [[ -n $version ]]
}

probe
health=$?
check_canceled
case $health in
    0) echo "dev.sh: Reusing Ollama at $endpoint" >&2 ;;
    1) fail "No valid Ollama /api/version response at $endpoint; port may be occupied by another service or unresponsive. Start/check Ollama manually." ;;
    2)
        [[ $endpoint == http://127.0.0.1:11434 ]] || \
            fail "Ollama unavailable at $endpoint; auto-start supports only http://127.0.0.1:11434. Start the custom server manually."
        command -v ollama >/dev/null 2>&1 || fail "ollama command not found; install Ollama and put it on PATH."
        echo "dev.sh: Starting Ollama; startup log: $scratch/ollama.log" >&2
        set -m
        OLLAMA_HOST=127.0.0.1:11434 ollama serve >"$scratch/ollama.log" 2>&1 < /dev/null &
        ollama_pid=$!
        set +m
        check_canceled
        deadline=$((SECONDS + timeout))
        while ((SECONDS < deadline)); do
            kill -0 "$ollama_pid" 2>/dev/null || fail 'ollama serve exited before becoming ready.'
            probe
            health=$?
            check_canceled
            # A concurrent app may win the bind race. Cleanup still targets only
            # our process group, never the PID of the service on the port.
            if ((health == 0)); then break; fi
            ((health != 1)) || fail "No valid Ollama /api/version response at $endpoint during startup."
            sleep 0.1
        done
        ((health == 0)) || fail "Ollama did not become ready within $timeout seconds."
        ;;
esac
check_canceled

# Build from any working directory, then run the native executable directly so
# signals reach Cognix rather than only a cargo run parent. Keep the caller's cwd.
set -m
(cd -- "$repo" && exec cargo build --locked --manifest-path "$repo/Cargo.toml" --target-dir "$repo/target" --bin cognix --bin cognix-dev-model) &
build_pid=$!
set +m
check_canceled
wait "$build_pid"
status=$?
check_canceled
((status == 0)) || exit "$status"
build_pid=
# The helper shares Cognix's CLI parser, original argv and unbuffered stdin.
# Keep it in the foreground terminal group so interactive consent works.
"$repo/target/debug/cognix-dev-model" "$@" <&0 &
provision_pid=$!
check_canceled
wait "$provision_pid"
status=$?
check_canceled
((status == 0)) || exit "$status"
provision_pid=
"$repo/target/debug/cognix" "$@" <&0 &
cognix_pid=$!
check_canceled
wait "$cognix_pid"
status=$?
check_canceled
cognix_pid=
exit "$status"
