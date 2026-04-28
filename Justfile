# List available recipes
default:
    @just --list

# ---------- Build ----------

# Build with default features (tokio)
build:
    cargo build --features tokio

# Build with every feature enabled
build-all:
    cargo build --all-features

# Check MSRV (1.88)
msrv:
    cargo +1.88.0 check --all-features

# ---------- Lint ----------

# Run clippy across selected feature combinations
clippy:
    cargo clippy --features tokio              --all-targets -- -D warnings
    cargo clippy --features smol               --all-targets -- -D warnings
    cargo clippy --features compio             --all-targets -- -D warnings
    cargo clippy --features tokio,rustls       --all-targets -- -D warnings
    cargo clippy --features smol,rustls        --all-targets -- -D warnings
    cargo clippy --features tokio,json         --all-targets -- -D warnings
    cargo clippy --features tokio,rustls,json  --all-targets -- -D warnings

# Run clippy with all features enabled
clippy-all:
    cargo clippy -p aioduct --all-features --all-targets -- -D warnings

# Run clippy with a specific feature set
clippy-features features:
    cargo clippy --features {{ features }} --all-targets -- -D warnings

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Apply formatting
fmt:
    cargo fmt --all

# ---------- Test ----------

# Run tests with default feature set (tokio,json)
test:
    cargo nextest run --features tokio,json

# Run tests with every feature enabled
test-all:
    cargo nextest run --all-features

# Run tests with a specific feature set
test-features features:
    cargo nextest run --features {{ features }}

# ---------- Coverage ----------

# Show coverage summary table
coverage:
    cargo llvm-cov nextest --all-features --summary-only

# Generate HTML coverage report and open in browser
coverage-html:
    mkdir -p coverage/html
    cargo llvm-cov nextest --all-features --html --output-dir coverage/html
    open coverage/html/index.html 2>/dev/null || xdg-open coverage/html/index.html 2>/dev/null || true

# Generate LCOV output for CI/editors
coverage-lcov:
    mkdir -p coverage
    cargo llvm-cov nextest --all-features --lcov --output-path coverage/lcov.info

# ---------- Bench ----------

# Run all benchmarks
bench:
    cargo bench -p aioduct-bench

# Run a specific benchmark binary (h1, h2, pooling, features, json)
bench-bin name:
    cargo bench -p aioduct-bench --bench {{ name }}

# Run a specific benchmark group by name filter
bench-group group:
    cargo bench -p aioduct-bench -- {{ group }}

# Run benchmarks and save baseline
bench-save name:
    cargo bench -p aioduct-bench -- --save-baseline {{ name }}

# Compare against a saved baseline
bench-compare baseline:
    cargo bench -p aioduct-bench -- --baseline {{ baseline }}

# ---------- Docs ----------

# Build and open rustdoc
doc:
    RUSTDOCFLAGS="-Dwarnings" cargo doc --all-features --no-deps --open

# Build rustdoc without opening (CI mode)
doc-check:
    RUSTDOCFLAGS="-Dwarnings" cargo doc --all-features --no-deps

# Build the mdbook
book:
    mdbook build doc

# Serve the mdbook with live reload
book-serve:
    mdbook serve doc --open

# ---------- Publish ----------

# Dry-run publish to verify packaging
publish-dry-run:
    cargo publish --dry-run -p aioduct --all-features

# Publish aioduct to crates.io
publish:
    cargo publish -p aioduct --all-features

# ---------- CI (run everything) ----------

# Run the full CI pipeline locally
ci: fmt-check clippy-all doc-check book msrv test-all coverage-lcov
