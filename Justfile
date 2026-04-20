# List available recipes
default:
    @just --list

# ---------- Build ----------

# Build with default features (tokio)
build:
    cargo build --features tokio

# Build with all common features
build-all:
    cargo build --features tokio,rustls,json,gzip,brotli,zstd,deflate,blocking

# Check MSRV (1.85)
msrv:
    cargo +1.85.0 check --features tokio

# ---------- Lint ----------

# Run clippy across all CI feature combinations
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
    cargo clippy -p aioduct --features {{ all-features }} --all-targets -- -D warnings

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
    cargo test --features tokio,json

# Run tests across all CI feature combinations
test-all:
    cargo nextest run --features tokio
    cargo nextest run --features smol
    cargo nextest run --features compio,tokio
    cargo nextest run --features tokio,json
    cargo nextest run --features tokio,rustls
    cargo nextest run --features smol,rustls

# Run tests with a specific feature set
test-features features:
    cargo test --features {{ features }}

# Run tests with nextest
test-nextest:
    cargo nextest run --features tokio,json

# Run doctests only
test-doc:
    cargo test --doc --features tokio,json

# ---------- Coverage ----------

# Show coverage summary table
coverage:
    cargo llvm-cov --features tokio,json,blocking,gzip report --summary-only

# Generate HTML coverage report and open in browser
coverage-html:
    mkdir -p coverage/html
    cargo llvm-cov --features tokio,json,blocking,gzip --html --output-dir coverage/html
    open coverage/html/index.html 2>/dev/null || xdg-open coverage/html/index.html 2>/dev/null || true

# Generate LCOV output for CI/editors
coverage-lcov:
    mkdir -p coverage
    cargo llvm-cov --features tokio,json --lcov --output-path coverage/lcov.info

# Generate LCOV output using nextest (matches CI)
coverage-nextest:
    mkdir -p coverage
    cargo llvm-cov nextest --features tokio,json --lcov --output-path coverage/lcov.info

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
    RUSTDOCFLAGS="-Dwarnings" cargo doc --features tokio,rustls,json --no-deps --open

# Build rustdoc without opening (CI mode)
doc-check:
    RUSTDOCFLAGS="-Dwarnings" cargo doc --features tokio,rustls,json --no-deps

# Build the mdbook
book:
    mdbook build doc

# Serve the mdbook with live reload
book-serve:
    mdbook serve doc --open

# ---------- Publish ----------

all-features := "tokio,smol,rustls,json,gzip,brotli,zstd,deflate,blocking,charset,tower,tracing"

# Dry-run publish to verify packaging
publish-dry-run:
    cargo publish --dry-run -p aioduct --features {{ all-features }}

# Publish aioduct to crates.io
publish:
    cargo publish -p aioduct --features {{ all-features }}

# ---------- CI (run everything) ----------

# Run the full CI pipeline locally
ci: fmt-check clippy doc-check msrv test-all
