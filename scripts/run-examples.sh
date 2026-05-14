#!/usr/bin/env bash
#
# Run all examples with a per-example timeout.
#
# Native runtimes (tokio, smol, compio) are built and run with cargo.
# wasi-p2 is cross-compiled and run with wasmtime (if installed).
# wasm is compile-checked only (requires a browser to run).
#
# Each runtime's examples run in parallel, but runtimes run sequentially
# to avoid overwhelming external test servers (httpbin.org).
#
# Crate names are derived from path: examples/<runtime>/<name> → example-<runtime>-<name>
#
# Usage:
#   ./scripts/run-examples.sh                  # run all
#   ./scripts/run-examples.sh tokio            # run only tokio examples
#   ./scripts/run-examples.sh compio wasi-p2   # run compio and wasi-p2 examples
#   TIMEOUT=30 ./scripts/run-examples.sh       # custom timeout (default: 15s)
#   JOBS=8 ./scripts/run-examples.sh           # max parallel jobs (default: 4)

set -euo pipefail

TIMEOUT="${TIMEOUT:-15}"
JOBS="${JOBS:-4}"

NATIVE_RUNTIMES=(tokio smol compio)
WASI_RUNTIMES=(wasi-p2)
WASM_RUNTIMES=(wasm)

if [ $# -gt 0 ]; then
    RUNTIMES=("$@")
else
    RUNTIMES=("${NATIVE_RUNTIMES[@]}" "${WASI_RUNTIMES[@]}" "${WASM_RUNTIMES[@]}")
fi

pkg_from_path() {
    echo "$1" | sed 's|examples/||;s|/Cargo.toml||' | tr '/' '-' | sed 's/^/example-/'
}

short_from_path() {
    echo "$1" | sed 's|examples/||;s|/Cargo.toml||'
}

is_in_list() {
    local needle="$1"; shift
    for item in "$@"; do
        [ "$needle" = "$item" ] && return 0
    done
    return 1
}

RESULTS_DIR=$(mktemp -d)
trap 'rm -rf "$RESULTS_DIR"' EXIT

# ── Phase 1: build ──────────────────────────────────────────────────────────

NATIVE_PKGS=()
WASI_PKGS=()
WASM_PKGS=()
ALL_TOMLS=()
RUNTIME_FOR_TOML=()

for runtime in "${RUNTIMES[@]}"; do
    for toml in examples/"$runtime"/*/Cargo.toml; do
        [ -f "$toml" ] || continue
        ALL_TOMLS+=("$toml")
        RUNTIME_FOR_TOML+=("$runtime")
        pkg=$(pkg_from_path "$toml")
        if is_in_list "$runtime" "${NATIVE_RUNTIMES[@]}"; then
            NATIVE_PKGS+=("$pkg")
        elif is_in_list "$runtime" "${WASI_RUNTIMES[@]}"; then
            WASI_PKGS+=("$pkg")
        elif is_in_list "$runtime" "${WASM_RUNTIMES[@]}"; then
            WASM_PKGS+=("$pkg")
        fi
    done
done

if [ ${#NATIVE_PKGS[@]} -gt 0 ]; then
    echo "Building ${#NATIVE_PKGS[@]} native examples..."
    BUILD_ARGS=()
    for pkg in "${NATIVE_PKGS[@]}"; do
        BUILD_ARGS+=(-p "$pkg")
    done
    cargo build "${BUILD_ARGS[@]}" --quiet 2>&1 || true
fi

HAS_WASM32_TARGET=false
if [ ${#WASI_PKGS[@]} -gt 0 ] || [ ${#WASM_PKGS[@]} -gt 0 ]; then
    if rustup target list --installed 2>/dev/null | grep -q wasm32; then
        HAS_WASM32_TARGET=true
    fi
fi

HAS_WASMTIME=false
if [ ${#WASI_PKGS[@]} -gt 0 ]; then
    if command -v wasmtime &>/dev/null && $HAS_WASM32_TARGET; then
        HAS_WASMTIME=true
        echo "Building ${#WASI_PKGS[@]} wasi-p2 examples..."
        BUILD_ARGS=()
        for pkg in "${WASI_PKGS[@]}"; do
            BUILD_ARGS+=(-p "$pkg")
        done
        cargo build "${BUILD_ARGS[@]}" --target wasm32-wasip2 --quiet 2>&1 || true
    else
        echo "Skipping wasi-p2 examples (requires wasmtime + wasm32-wasip2 target)"
    fi
fi

HAS_WASM_TARGET=false
if [ ${#WASM_PKGS[@]} -gt 0 ]; then
    if rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown; then
        HAS_WASM_TARGET=true
        echo "Building ${#WASM_PKGS[@]} wasm examples (compile-check only)..."
        BUILD_ARGS=()
        for pkg in "${WASM_PKGS[@]}"; do
            BUILD_ARGS+=(-p "$pkg")
        done
        cargo build "${BUILD_ARGS[@]}" --target wasm32-unknown-unknown --quiet 2>&1
        WASM_BUILD_STATUS=$?
    else
        echo "Skipping wasm examples (requires wasm32-unknown-unknown target)"
    fi
fi

echo ""

# ── Phase 2: run ────────────────────────────────────────────────────────────

run_native() {
    local toml="$1"
    local results_dir="$2"
    local timeout_secs="$3"
    local pkg short output status

    pkg=$(pkg_from_path "$toml")
    short=$(short_from_path "$toml")
    result_file="$results_dir/$(echo "$short" | tr '/' '_')"

    output=$(timeout "$timeout_secs" cargo run -p "$pkg" --quiet 2>&1) && status=0 || status=$?

    if [ "$status" -eq 0 ]; then
        echo "PASS|$short|" > "$result_file"
    elif [ "$status" -eq 124 ]; then
        echo "TIMEOUT|$short|${timeout_secs}s" > "$result_file"
    else
        last_line=$(echo "$output" | grep -v '^$' | tail -1)
        echo "FAIL|$short|exit $status: $last_line" > "$result_file"
    fi
}

run_wasi() {
    local toml="$1"
    local results_dir="$2"
    local timeout_secs="$3"
    local pkg short output status bin_name

    pkg=$(pkg_from_path "$toml")
    short=$(short_from_path "$toml")
    result_file="$results_dir/$(echo "$short" | tr '/' '_')"
    bin_name=$(echo "$pkg" | tr '-' '_')

    local wasm="target/wasm32-wasip2/debug/${bin_name}.wasm"
    if [ ! -f "$wasm" ]; then
        echo "FAIL|$short|wasm binary not found: $wasm" > "$result_file"
        return
    fi

    output=$(timeout "$timeout_secs" wasmtime run --wasi inherit-network "$wasm" 2>&1) && status=0 || status=$?

    if [ "$status" -eq 0 ]; then
        echo "PASS|$short|" > "$result_file"
    elif [ "$status" -eq 124 ]; then
        echo "TIMEOUT|$short|${timeout_secs}s" > "$result_file"
    else
        last_line=$(echo "$output" | grep -v '^$' | tail -1)
        echo "FAIL|$short|exit $status: $last_line" > "$result_file"
    fi
}

export -f run_native run_wasi pkg_from_path short_from_path

TOTAL_PASS=0
TOTAL_FAIL=0
TOTAL_TIMEOUT=0
TOTAL_SKIP=0
ALL_FAILURES=()

for runtime in "${RUNTIMES[@]}"; do
    TOMLS=()
    for toml in examples/"$runtime"/*/Cargo.toml; do
        [ -f "$toml" ] || continue
        TOMLS+=("$toml")
    done
    [ ${#TOMLS[@]} -eq 0 ] && continue

    if is_in_list "$runtime" "${NATIVE_RUNTIMES[@]}"; then
        echo "══════════════════════════════════════════"
        echo "  Runtime: $runtime  (${#TOMLS[@]} examples, ${JOBS} parallel)"
        echo "══════════════════════════════════════════"

        printf '%s\n' "${TOMLS[@]}" | xargs -P "$JOBS" -I{} \
            bash -c 'run_native "$1" "$2" "$3"' _ {} "$RESULTS_DIR" "$TIMEOUT"

    elif is_in_list "$runtime" "${WASI_RUNTIMES[@]}"; then
        if ! $HAS_WASMTIME; then
            echo "══════════════════════════════════════════"
            echo "  Runtime: $runtime  (skipped)"
            echo "══════════════════════════════════════════"
            for toml in "${TOMLS[@]}"; do
                short=$(short_from_path "$toml")
                printf "  %-45s SKIP (no wasmtime/target)\n" "$short"
                TOTAL_SKIP=$((TOTAL_SKIP + 1))
            done
            echo ""
            continue
        fi

        echo "══════════════════════════════════════════"
        echo "  Runtime: $runtime  (${#TOMLS[@]} examples, ${JOBS} parallel)"
        echo "══════════════════════════════════════════"

        printf '%s\n' "${TOMLS[@]}" | xargs -P "$JOBS" -I{} \
            bash -c 'run_wasi "$1" "$2" "$3"' _ {} "$RESULTS_DIR" "$TIMEOUT"

    elif is_in_list "$runtime" "${WASM_RUNTIMES[@]}"; then
        echo "══════════════════════════════════════════"
        echo "  Runtime: $runtime  (${#TOMLS[@]} examples, compile-check)"
        echo "══════════════════════════════════════════"

        if ! $HAS_WASM_TARGET; then
            for toml in "${TOMLS[@]}"; do
                short=$(short_from_path "$toml")
                printf "  %-45s SKIP (no wasm32-unknown-unknown target)\n" "$short"
                TOTAL_SKIP=$((TOTAL_SKIP + 1))
            done
            echo ""
            continue
        fi

        for toml in "${TOMLS[@]}"; do
            short=$(short_from_path "$toml")
            result_file="$RESULTS_DIR/$(echo "$short" | tr '/' '_')"
            if [ "${WASM_BUILD_STATUS:-1}" -eq 0 ]; then
                echo "PASS|$short|compile-check" > "$result_file"
            else
                echo "FAIL|$short|compile failed" > "$result_file"
            fi
        done

    else
        echo "══════════════════════════════════════════"
        echo "  Runtime: $runtime  (skipped — unknown runtime)"
        echo "══════════════════════════════════════════"
        for toml in "${TOMLS[@]}"; do
            short=$(short_from_path "$toml")
            printf "  %-45s SKIP (unknown runtime)\n" "$short"
            TOTAL_SKIP=$((TOTAL_SKIP + 1))
        done
        echo ""
        continue
    fi

    for toml in "${TOMLS[@]}"; do
        short=$(short_from_path "$toml")
        result_file="$RESULTS_DIR/$(echo "$short" | tr '/' '_')"

        if [ ! -f "$result_file" ]; then
            printf "  %-45s UNKNOWN\n" "$short"
            continue
        fi

        IFS='|' read -r kind name detail < "$result_file"

        case "$kind" in
            PASS)
                if [ -n "$detail" ]; then
                    printf "  %-45s PASS (%s)\n" "$short" "$detail"
                else
                    printf "  %-45s PASS\n" "$short"
                fi
                TOTAL_PASS=$((TOTAL_PASS + 1))
                ;;
            FAIL)
                printf "  %-45s FAIL (%s)\n" "$short" "$detail"
                TOTAL_FAIL=$((TOTAL_FAIL + 1))
                ALL_FAILURES+=("$short")
                ;;
            TIMEOUT)
                printf "  %-45s TIMEOUT (%s)\n" "$short" "$detail"
                TOTAL_TIMEOUT=$((TOTAL_TIMEOUT + 1))
                ;;
        esac
    done
    echo ""
done

echo "══════════════════════════════════════════"
printf "  Results: %d passed, %d failed, %d timeout" "$TOTAL_PASS" "$TOTAL_FAIL" "$TOTAL_TIMEOUT"
[ "$TOTAL_SKIP" -gt 0 ] && printf ", %d skipped" "$TOTAL_SKIP"
echo ""
echo "══════════════════════════════════════════"

if [ ${#ALL_FAILURES[@]} -gt 0 ]; then
    echo ""
    echo "  Failed:"
    for f in "${ALL_FAILURES[@]}"; do
        echo "    - $f"
    done
fi

exit $( [ "$TOTAL_FAIL" -eq 0 ] && echo 0 || echo 1 )
