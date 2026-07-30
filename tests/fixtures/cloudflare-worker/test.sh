#!/usr/bin/env bash

set -euo pipefail

fixture_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_dir="$(cd "$fixture_dir/../../.." && pwd)"
server_pid=""
wrangler_pid=""

cleanup() {
    if [[ -n "$wrangler_pid" ]]; then
        kill "$wrangler_pid" 2>/dev/null || true
        wait "$wrangler_pid" 2>/dev/null || true
    fi
    if [[ -n "$server_pid" ]]; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

cargo install --quiet --locked --version 0.8.4 worker-build

(
    cd "$workspace_dir"
    cargo run -p wasm-test-server
) &
server_pid=$!

(
    cd "$fixture_dir"
    npx --yes wrangler@4.115.0 dev --port 8787 --ip 127.0.0.1
) &
wrangler_pid=$!

ready=false
for _ in $(seq 1 720); do
    if ! kill -0 "$wrangler_pid" 2>/dev/null; then
        wait "$wrangler_pid"
        exit 1
    fi
    if curl --fail --silent --show-error http://127.0.0.1:9877/hello >/dev/null 2>&1 \
        && curl --fail --silent --show-error http://127.0.0.1:8787/ >/dev/null 2>&1
    then
        ready=true
        break
    fi
    sleep 0.25
done

if [[ "$ready" != true ]]; then
    echo "Cloudflare Worker fixture did not become ready" >&2
    exit 1
fi

assert_route() {
    local route="$1"
    local expected_status="$2"
    local expected_body="$3"
    local body_file
    local actual_status
    local actual_body

    body_file="$(mktemp)"
    actual_status="$(
        curl --silent --show-error \
            --output "$body_file" \
            --write-out '%{http_code}' \
            "http://127.0.0.1:8787/$route"
    )"
    actual_body="$(<"$body_file")"
    rm "$body_file"

    if [[ "$actual_status" != "$expected_status" || "$actual_body" != "$expected_body" ]]; then
        echo "$route: expected $expected_status '$expected_body', got $actual_status '$actual_body'" >&2
        return 1
    fi
}

assert_route control 200 204
assert_route aioduct 200 204
assert_route aioduct-timed-fast 200 204
assert_route aioduct-timeout 200 timeout
