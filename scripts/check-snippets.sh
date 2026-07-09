#!/usr/bin/env bash
set -euo pipefail

# Compile-check each aioduct snippet against the current API.
# Usage: ./scripts/check-snippets.sh [--verbose]

VERBOSE=false
if [[ "${1:-}" == "--verbose" || "${1:-}" == "-v" ]]; then
    VERBOSE=true
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SNIPPETS_DIR="$WORKSPACE_ROOT/site/snippets"
TARGET_DIR="$WORKSPACE_ROOT/target/snippet-check"
AIODUCT_PATH="$WORKSPACE_ROOT/crates/aioduct"

# Given a crate name, echo its Cargo.toml dependency line (if known)
dep_for_crate() {
    case "$1" in
        serde_json)      echo 'serde_json = "1"' ;;
        serde)           echo 'serde = "1"' ;;
        base64)          echo 'base64 = "0.22"' ;;
        smol)            echo 'smol = "2"' ;;
        url)             echo 'url = "2"' ;;
        serde_urlencoded) echo 'serde_urlencoded = "0.7"' ;;
        tokio)           echo 'tokio = { version = "1", features = ["macros", "rt-multi-thread"] }' ;;
        http)            echo 'http = "1"' ;;
        *)               return 1 ;;
    esac
}

# List of all known extra crate names (space-separated)
KNOWN_EXTRA_CRATES="serde_json serde base64 smol url serde_urlencoded tokio http"

pass=0
fail=0
skip=0
failed_names=()

check_snippet() {
    local snippet="$1"
    local name
    name="$(basename "$snippet" .rs)"

    # Parse the features header (portable sed)
    local features
    features=$(sed -n 's/^\/\/ features:[[:space:]]*//p' "$snippet" | head -1)
    if [[ -z "$features" ]]; then
        echo "  SKIP $name: no features header"
        ((skip++)) || true
        return
    fi

    # Determine extra crate dependencies needed
    local extra_deps=()
    local seen=()

    # Detect crates from `use crate_name::...` statements
    while IFS= read -r crate; do
        case "$crate" in
            aioduct|std|core|alloc) continue ;;
            *)
                local dep
                if dep=$(dep_for_crate "$crate" 2>/dev/null); then
                    if ! printf '%s\n' "${extra_deps[@]:-}" | grep -qF "$dep"; then
                        extra_deps+=("$dep")
                    fi
                fi
                ;;
        esac
    done < <(sed -n 's/^[[:space:]]*use[[:space:]]\{1,\}\([a-zA-Z0-9_]\{1,\}\)::.*/\1/p' "$snippet" | sort -u)

    # Detect inline paths like serde_json::Value (crates in known list)
    for crate in $KNOWN_EXTRA_CRATES; do
        if grep -qE "[[:space:](:<,]${crate}::" "$snippet"; then
            local dep
            if dep=$(dep_for_crate "$crate" 2>/dev/null); then
                if ! printf '%s\n' "${extra_deps[@]:-}" | grep -qF "$dep"; then
                    extra_deps+=("$dep")
                fi
            fi
        fi
    done

    # #[tokio::main] implies tokio crate
    if grep -q '#\[tokio::main\]' "$snippet"; then
        if ! printf '%s\n' "${extra_deps[@]:-}" | grep -qF 'tokio ='; then
            extra_deps+=("$(dep_for_crate tokio)")
        fi
    fi
    # smol::block_on implies smol crate
    if grep -q 'smol::block_on' "$snippet"; then
        if ! printf '%s\n' "${extra_deps[@]:-}" | grep -qF 'smol ='; then
            extra_deps+=("$(dep_for_crate smol)")
        fi
    fi

    # Build temp project
    local proj_dir="$TARGET_DIR/$name"
    rm -rf "$proj_dir"
    mkdir -p "$proj_dir/src"

    # Quote features for TOML array (avoid paste -d issues with multi-char delimiters)
    local quoted_features
    quoted_features=$(echo "$features" | tr ',' '\n' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' | grep -v '^$' | sed 's/.*/"&"/' | tr '\n' ',' | sed 's/,$//;s/,/, /g')

    # Build Cargo.toml
    local cargo_toml="$proj_dir/Cargo.toml"
    {
        echo '[workspace]'
        echo ''
        echo '[package]'
        echo "name = \"snippet-${name//_/-}\""
        echo 'version = "0.0.0"'
        echo 'edition = "2021"'
        echo 'publish = false'
        echo ''
        echo '[dependencies]'
        echo "aioduct = { path = \"$AIODUCT_PATH\", features = [$quoted_features] }"
        if [[ ${#extra_deps[@]} -gt 0 ]]; then
            printf '%s\n' "${extra_deps[@]}"
        fi
    } > "$cargo_toml"

    cp "$snippet" "$proj_dir/src/main.rs"

    # Run cargo check
    local output
    if output=$(cargo check --manifest-path "$cargo_toml" \
                   --target-dir "$TARGET_DIR/target" 2>&1); then
        if $VERBOSE; then
            echo "  OK   $name  (features: $features)"
        fi
        ((pass++)) || true
    else
        echo "  FAIL $name  (features: $features)"
        # Print just the error lines
        echo "$output" | grep -E 'error(\[|:)' | sed 's/^/    /' || true
        ((fail++)) || true
        failed_names+=("$name")
    fi
}

main() {
    local snippets=()
    local snippet
    while IFS= read -r snippet; do
        snippets+=("$snippet")
    done < <(find "$SNIPPETS_DIR" -name '*_aioduct.rs' -type f | sort)
    if [[ ${#snippets[@]} -eq 0 ]]; then
        echo "No snippets found in $SNIPPETS_DIR"
        exit 1
    fi

    echo "Checking ${#snippets[@]} snippets..."
    echo ""

    for snippet in "${snippets[@]}"; do
        check_snippet "$snippet"
    done

    echo ""
    echo "── Results ──"
    echo "  Passed:  $pass"
    echo "  Failed:  $fail"

    if [[ $fail -gt 0 ]]; then
        echo ""
        echo "Failed snippets:"
        for name in "${failed_names[@]}"; do
            echo "  - $name"
        done
        exit 1
    fi
}

main
