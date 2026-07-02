# List available recipes
default:
    @just --list

all_features_ring := "json,charset,rustls,rustls-ring,rustls-native-roots,hickory-dns,blocking,tokio,smol,compio,http3,gzip,brotli,zstd,deflate,tower,tracing,otel,wasm,wasi-p2"
all_features_aws_lc_rs := "json,charset,rustls,rustls-aws-lc-rs,rustls-native-roots,hickory-dns,blocking,tokio,smol,compio,http3,gzip,brotli,zstd,deflate,tower,tracing,otel,wasm,wasi-p2"

# ---------- Build ----------

# Build with default features (tokio)
build:
    cargo build --features tokio

# Build with every compatible all-feature provider set
build-all:
    cargo build -p aioduct --features {{ all_features_ring }}
    cargo build -p aioduct --features {{ all_features_aws_lc_rs }}
    cargo build -p aioduct-wasmtime
    cargo build -p aioduct-wasmtime --no-default-features --features tokio,rustls-aws-lc-rs
    cargo build -p aioduct-wasmtime --no-default-features --features smol,rustls-ring

# Check MSRV (1.94)
msrv:
    cargo +1.94.0 check -p aioduct --features {{ all_features_ring }}
    cargo +1.94.0 check -p aioduct --features {{ all_features_aws_lc_rs }}
    cargo +1.94.0 check -p aioduct-wasmtime
    cargo +1.94.0 check -p aioduct-wasmtime --no-default-features --features tokio,rustls-aws-lc-rs
    cargo +1.94.0 check -p aioduct-wasmtime --no-default-features --features smol,rustls-ring

# ---------- Lint ----------

# Run clippy across selected feature combinations
clippy:
    cargo clippy -p aioduct --features tokio              --all-targets -- -D warnings
    cargo clippy -p aioduct --features smol               --all-targets -- -D warnings
    cargo clippy -p aioduct --features compio             --all-targets -- -D warnings
    cargo clippy -p aioduct --features tokio,rustls,rustls-ring       --all-targets -- -D warnings
    cargo clippy -p aioduct --features smol,rustls,rustls-ring        --all-targets -- -D warnings
    cargo clippy -p aioduct --features tokio,json         --all-targets -- -D warnings
    cargo clippy -p aioduct --features tokio,rustls,rustls-ring,json  --all-targets -- -D warnings

# Run clippy with every compatible all-feature provider set
clippy-all:
    cargo clippy -p aioduct --features {{ all_features_ring }} --all-targets -- -D warnings
    cargo clippy -p aioduct --features {{ all_features_aws_lc_rs }} --all-targets -- -D warnings
    cargo clippy -p aioduct-wasmtime --all-targets -- -D warnings
    cargo clippy -p aioduct-wasmtime --no-default-features --features tokio,rustls-aws-lc-rs --all-targets -- -D warnings
    cargo clippy -p aioduct-wasmtime --no-default-features --features smol,rustls-ring --all-targets -- -D warnings
    cargo check --workspace --all-targets

# Run clippy with a specific feature set
clippy-features features:
    cargo clippy -p aioduct --features {{ features }} --all-targets -- -D warnings

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
    cargo nextest run -p aioduct-wasmtime
    cargo nextest run -p aioduct-wasmtime --no-default-features --features tokio,rustls-aws-lc-rs
    cargo nextest run -p aioduct-wasmtime --no-default-features --features smol,rustls-ring

# Run tests with a specific feature set
test-features features:
    cargo nextest run --features {{ features }}

# Run WASI-P2 unit tests under wasmtime
test-wasi-p2:
    CARGO_TARGET_WASM32_WASIP2_RUNNER="wasmtime run --" \
        cargo test -p aioduct --target wasm32-wasip2 --features wasi-p2,json --lib

# Run WASM integration tests in headless Chrome (requires wasm-pack + Chrome)
test-wasm:
    cargo run -p wasm-test-server & SERVER_PID=$!; \
    sleep 1; \
    cd crates/aioduct && wasm-pack test --headless --chrome --features wasm,json; \
    STATUS=$?; kill $SERVER_PID 2>/dev/null; exit $STATUS

# ---------- Coverage ----------

# Show coverage summary table
coverage:
    cargo llvm-cov clean --workspace
    cargo llvm-cov nextest -p aioduct --features {{ all_features_ring }} --no-report
    cargo llvm-cov nextest -p aioduct --features {{ all_features_aws_lc_rs }} --no-clean

# Generate HTML coverage report and open in browser
coverage-html:
    mkdir -p coverage/html
    cargo llvm-cov clean --workspace
    cargo llvm-cov nextest -p aioduct --features {{ all_features_ring }} --no-report
    cargo llvm-cov nextest -p aioduct --features {{ all_features_aws_lc_rs }} --no-clean --html --output-dir coverage/html
    open coverage/html/index.html 2>/dev/null || xdg-open coverage/html/index.html 2>/dev/null || true

# Generate LCOV output for CI/editors
coverage-lcov:
    mkdir -p coverage
    cargo llvm-cov clean --workspace
    cargo llvm-cov nextest -p aioduct --features {{ all_features_ring }} --no-report
    cargo llvm-cov nextest -p aioduct --features {{ all_features_aws_lc_rs }} --no-clean --lcov --output-path coverage/lcov.info

# ---------- Bench ----------

# Run all benchmarks
bench:
    cargo bench -p aioduct-bench

# Run a specific benchmark group by name filter
bench-group group:
    cargo bench -p aioduct-bench --bench bench_main -- {{ group }}

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
    RUSTDOCFLAGS="-Dwarnings" cargo doc -p aioduct-wasmtime --no-deps
    RUSTDOCFLAGS="-Dwarnings" cargo doc -p aioduct-wasmtime --no-default-features --features tokio,rustls-aws-lc-rs --no-deps
    RUSTDOCFLAGS="-Dwarnings" cargo doc -p aioduct-wasmtime --no-default-features --features smol,rustls-ring --no-deps

# Build the mdbook
book:
    mdbook build docs

# Serve the mdbook with live reload
book-serve:
    mdbook serve docs --open

# ---------- Publish ----------

# Dry-run publish to verify packaging
publish-dry-run:
    cargo publish --dry-run -p aioduct --features {{ all_features_ring }}

# Publish aioduct to crates.io
publish:
    cargo publish -p aioduct --features {{ all_features_ring }}

# ---------- CI (run everything) ----------

# Run the full CI pipeline locally
ci: fmt-check clippy-all doc-check book msrv test-all test-wasm test-wasi-p2 coverage-lcov check-snippets

# Cross-check the CLI for all cargo-dist targets (requires cross-compilers)
cross-check:
    cargo check -p aioduct-cli --target x86_64-unknown-linux-gnu
    cargo check -p aioduct-cli --target aarch64-unknown-linux-gnu
    cargo check -p aioduct-cli --target x86_64-apple-darwin
    cargo check -p aioduct-cli --target aarch64-apple-darwin
    cargo check -p aioduct-cli --target x86_64-pc-windows-msvc

# ---------- Site ----------

# Build the landing page WASM demo (requires wasm-pack)
site-wasm:
    wasm-pack build examples/wasm/demo \
        --target web \
        --out-dir ../../../site/wasm \
        --out-name aioduct_wasm_demo \
        --release

# Check that all comparison site snippets compile
check-snippets:
    bash scripts/check-snippets.sh
    rm -f site/wasm/.gitignore site/wasm/package.json

# Assemble the full site (landing page + mdBook docs)
site-build: book site-wasm
    rm -rf _site
    mkdir -p _site
    cp site/index.html site/style.css site/app.js _site/
    cp -r site/wasm _site/wasm
    cp -r docs/book _site/docs

# Serve the site locally on port 8080 (requires: cargo install miniserve)
site-serve: site-build
    @echo "Serving at http://localhost:8080"
    miniserve _site --port 8080 --index index.html
