# List available recipes
default:
    @just --list

all_features_ring := "json,charset,rustls,rustls-ring,rustls-native-roots,hickory-dns,blocking,tokio,smol,compio,http3,gzip,brotli,zstd,deflate,tower,tracing,otel,wasm"
all_features_aws_lc_rs := "json,charset,rustls,rustls-aws-lc-rs,rustls-native-roots,hickory-dns,blocking,tokio,smol,compio,http3,gzip,brotli,zstd,deflate,tower,tracing,otel,wasm"

# ---------- Build ----------

# Build with default features (tokio)
build:
    cargo build --features tokio

# Build with every compatible all-feature provider set
build-all:
    cargo build -p aioduct --features {{ all_features_ring }}
    cargo build -p aioduct --features {{ all_features_aws_lc_rs }}

# Check MSRV (1.88)
msrv:
    cargo +1.88.0 check -p aioduct --features {{ all_features_ring }}
    cargo +1.88.0 check -p aioduct --features {{ all_features_aws_lc_rs }}

# ---------- Lint ----------

# Run clippy across selected feature combinations
clippy:
    cargo clippy --features tokio              --all-targets -- -D warnings
    cargo clippy --features smol               --all-targets -- -D warnings
    cargo clippy --features compio             --all-targets -- -D warnings
    cargo clippy --features tokio,rustls,rustls-ring       --all-targets -- -D warnings
    cargo clippy --features smol,rustls,rustls-ring        --all-targets -- -D warnings
    cargo clippy --features tokio,json         --all-targets -- -D warnings
    cargo clippy --features tokio,rustls,rustls-ring,json  --all-targets -- -D warnings

# Run clippy with every compatible all-feature provider set
clippy-all:
    cargo clippy -p aioduct --features {{ all_features_ring }} --all-targets -- -D warnings
    cargo clippy -p aioduct --features {{ all_features_aws_lc_rs }} --all-targets -- -D warnings

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

# Run tests with every compatible all-feature provider set
test-all:
    cargo nextest run -p aioduct --features {{ all_features_ring }}
    cargo nextest run -p aioduct --features {{ all_features_aws_lc_rs }}

# Run tests with a specific feature set
test-features features:
    cargo nextest run --features {{ features }}

# ---------- Coverage ----------

# Show coverage summary table
coverage:
    cargo llvm-cov clean --workspace
    cargo llvm-cov nextest -p aioduct --features {{ all_features_ring }} --no-report
    cargo llvm-cov nextest -p aioduct --features {{ all_features_aws_lc_rs }} --no-report --no-clean
    cargo llvm-cov report --summary-only

# Generate HTML coverage report and open in browser
coverage-html:
    mkdir -p coverage/html
    cargo llvm-cov clean --workspace
    cargo llvm-cov nextest -p aioduct --features {{ all_features_ring }} --no-report
    cargo llvm-cov nextest -p aioduct --features {{ all_features_aws_lc_rs }} --no-report --no-clean
    cargo llvm-cov report --html --output-dir coverage/html
    open coverage/html/index.html 2>/dev/null || xdg-open coverage/html/index.html 2>/dev/null || true

# Generate LCOV output for CI/editors
coverage-lcov:
    mkdir -p coverage
    cargo llvm-cov clean --workspace
    cargo llvm-cov nextest -p aioduct --features {{ all_features_ring }} --no-report
    cargo llvm-cov nextest -p aioduct --features {{ all_features_aws_lc_rs }} --no-report --no-clean
    cargo llvm-cov report --lcov --output-path coverage/lcov.info

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
    RUSTDOCFLAGS="-Dwarnings" cargo doc -p aioduct --features {{ all_features_ring }} --no-deps --open

# Build rustdoc without opening (CI mode)
doc-check:
    RUSTDOCFLAGS="-Dwarnings" cargo doc -p aioduct --features {{ all_features_ring }} --no-deps
    RUSTDOCFLAGS="-Dwarnings" cargo doc -p aioduct --features {{ all_features_aws_lc_rs }} --no-deps

# Build the mdbook
book:
    mdbook build doc

# Serve the mdbook with live reload
book-serve:
    mdbook serve doc --open

# ---------- Publish ----------

# Dry-run publish to verify packaging
publish-dry-run:
    cargo publish --dry-run -p aioduct --features {{ all_features_ring }}

# Publish aioduct to crates.io
publish:
    cargo publish -p aioduct --features {{ all_features_ring }}

# ---------- CI (run everything) ----------

# Run the full CI pipeline locally
ci: fmt-check clippy-all doc-check book msrv test-all coverage-lcov
