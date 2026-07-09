// Feature-flag constraint model
const CONSTRAINTS = {
    // feature → features it requires (auto-enabled)
    dependencies: {
        'http3': ['rustls'],
        'doh': ['hickory-dns'],
        'dot': ['hickory-dns'],
        'hickory-dns': ['tokio'],
        'blocking': ['tokio'],
        'rustls-ring': ['rustls'],
        'rustls-aws-lc-rs': ['rustls'],
    },
    // feature → features it's incompatible with (disabled with tooltip)
    incompatible: {
        'wasm': ['http3', 'blocking', 'hickory-dns', 'doh', 'dot', 'compio', 'wasi-p2', 'wasmtime'],
        'wasi-p2': ['wasm', 'http3', 'blocking', 'hickory-dns', 'doh', 'dot', 'compio'],
        'wasmtime': ['wasm'],
    },
};

const VERSION = '0.2.3';

// --- Feature Configurator ---

function getCheckedFeatures() {
    const checkboxes = document.querySelectorAll('input[name="feature"]:checked');
    return Array.from(checkboxes).map(cb => cb.value);
}

function setFeatureChecked(name, checked) {
    const cb = document.querySelector(`input[name="feature"][value="${name}"]`);
    if (cb && !cb.disabled) {
        cb.checked = checked;
    }
}

function setFeatureDisabled(name, disabled, reason) {
    const cb = document.querySelector(`input[name="feature"][value="${name}"]`);
    if (!cb) return;
    const label = cb.closest('.feature-checkbox');
    cb.disabled = disabled;
    if (disabled) {
        cb.checked = false;
        label.classList.add('disabled');
        label.title = reason || '';
    } else {
        label.classList.remove('disabled');
        label.title = '';
    }
}

function applyConstraints(changedFeature) {
    // Apply dependencies: if feature X is checked, also check its deps
    const checked = getCheckedFeatures();
    for (const feature of checked) {
        const deps = CONSTRAINTS.dependencies[feature];
        if (deps) {
            for (const dep of deps) {
                setFeatureChecked(dep, true);
            }
        }
    }

    // Apply incompatibilities: if feature X is checked, disable incompatible features
    // First, re-enable everything
    document.querySelectorAll('input[name="feature"]').forEach(cb => {
        setFeatureDisabled(cb.value, false);
    });

    // Then disable based on current selections
    const currentChecked = getCheckedFeatures();
    for (const feature of currentChecked) {
        const incompat = CONSTRAINTS.incompatible[feature];
        if (incompat) {
            for (const blocked of incompat) {
                if (!currentChecked.includes(blocked)) {
                    setFeatureDisabled(blocked, true, `Incompatible with "${feature}"`);
                }
            }
        }
    }

    updateCargoOutput();
}

function updateCargoOutput() {
    const features = getCheckedFeatures();
    const output = document.getElementById('cargo-output');
    const warnings = document.getElementById('config-warnings');

    let toml;
    if (features.length === 0) {
        toml = `[dependencies]\naioduct = "${VERSION}"`;
    } else {
        const featureStr = features.map(f => `"${f}"`).join(', ');
        toml = `[dependencies]\naioduct = { version = "${VERSION}", features = [${featureStr}] }`;
    }
    output.textContent = toml;

    // Warnings
    let warningHtml = '';
    if (features.includes('http3') && !features.includes('rustls-ring') && !features.includes('rustls-aws-lc-rs')) {
        warningHtml += '<div class="warning">http3 requires a crypto backend: add rustls-ring or rustls-aws-lc-rs</div>';
    }
    if (!features.some(f => ['tokio', 'smol', 'compio', 'wasm', 'wasi-p2'].includes(f))) {
        warningHtml += '<div class="warning">No runtime selected — aioduct requires at least one runtime feature</div>';
    }
    if (features.includes('wasmtime') && !features.some(f => ['tokio', 'smol', 'compio'].includes(f))) {
        warningHtml += '<div class="warning">wasmtime requires a native host runtime: add tokio, smol, or compio</div>';
    }
    if (features.includes('rustls-ring') && features.includes('rustls-aws-lc-rs')) {
        warningHtml += '<div class="info">Both crypto backends selected — only one is needed (ring is simpler to compile)</div>';
    }
    warnings.innerHTML = warningHtml;
}

function initConfigurator() {
    document.querySelectorAll('input[name="feature"]').forEach(cb => {
        cb.addEventListener('change', (e) => {
            applyConstraints(e.target.value);
        });
    });
    // Initial state
    applyConstraints(null);

    // Copy button
    document.getElementById('copy-btn').addEventListener('click', () => {
        const text = document.getElementById('cargo-output').textContent;
        navigator.clipboard.writeText(text).then(() => {
            const btn = document.getElementById('copy-btn');
            btn.textContent = 'Copied!';
            setTimeout(() => { btn.textContent = 'Copy'; }, 1500);
        });
    });
}

// --- Comparison Tabs ---

function initTabs() {
    document.querySelectorAll('.comparison-tabs .tab').forEach(tab => {
        tab.addEventListener('click', () => {
            const target = tab.dataset.tab;

            document.querySelectorAll('.comparison-tabs .tab').forEach(t => t.classList.remove('active'));
            tab.classList.add('active');

            document.querySelectorAll('.comparison-panel').forEach(p => p.classList.remove('active'));
            document.querySelector(`.comparison-panel[data-tab="${target}"]`).classList.add('active');
        });
    });
}

// --- WASM Demo ---

let wasmModule = null;

const DEMO_CODE = {
    fetch_json: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/get")?
    .send().await?;
let data: serde_json::Value = resp.json()?;`,

    fetch_url: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/ip")?
    .send().await?;
let body = resp.text()?;`,

    fetch_ip_info: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/ip")?
    .send().await?;
let json: Value = resp.json()?;
let ip = json["origin"].as_str().unwrap();`,

    fetch_html: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/html")?
    .header(ACCEPT, "text/html")
    .send().await?;
let html = resp.text()?;`,

    fetch_utf8: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/encoding/utf8")?
    .send().await?;
let text = resp.text()?; // UTF-8 decoded`,

    fetch_large_response: `let client = WasmClient::new();
let start = Instant::now();
let resp = client.get("https://httpbin.org/bytes/10000")?
    .send().await?;
let bytes = resp.bytes();
let elapsed = start.elapsed();
println!("{} bytes in {:?}", bytes.len(), elapsed);`,

    fetch_with_headers: `let client = WasmClient::builder()
    .user_agent("aioduct-demo/1.0")
    .build();
let resp = client.get("https://httpbin.org/headers")?
    .header(ACCEPT, "application/json")
    .send().await?;`,

    fetch_with_bearer: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/bearer")?
    .bearer_auth("demo-token-12345")
    .send().await?;
let data: serde_json::Value = resp.json()?;`,

    fetch_basic_auth: `let client = WasmClient::new();
let creds = format!("Basic {}", base64("demo:secret"));
let resp = client.get("https://httpbin.org/basic-auth/demo/secret")?
    .header(AUTHORIZATION, &creds)
    .send().await?;`,

    fetch_user_agent: `let client = WasmClient::builder()
    .user_agent("MyApp/2.0 (Rust; aioduct)")
    .build();
let resp = client.get("https://httpbin.org/user-agent")?
    .send().await?;`,

    fetch_cookies: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/anything")?
    .header(COOKIE, "session=abc123; theme=dark")
    .send().await?;
// Server echoes all received headers`,

    fetch_response_headers: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/response-headers?X-Custom=hello")?
    .send().await?;
for (name, value) in resp.headers() {
    println!("{name}: {}", value.to_str()?);
}`,

    post_json: `let client = WasmClient::new();
let resp = client.post("https://httpbin.org/post")?
    .json(&json!({"name": "aioduct", "version": "0.1.7"}))?
    .send().await?;
let data: serde_json::Value = resp.json()?;`,

    put_json: `let client = WasmClient::new();
let resp = client.put("https://httpbin.org/put")?
    .json(&json!({"id": 1, "name": "updated"}))?
    .send().await?;`,

    patch_json: `let client = WasmClient::new();
let resp = client.patch("https://httpbin.org/patch")?
    .json(&json!({"name": "patched"}))?
    .send().await?;`,

    delete_request: `let client = WasmClient::new();
let resp = client.delete("https://httpbin.org/delete")?
    .send().await?;
let body = resp.text()?;`,

    head_request: `let client = WasmClient::new();
let resp = client.head("https://httpbin.org/get")?
    .send().await?;
// HEAD returns headers only, no body
for (name, value) in resp.headers() {
    println!("{name}: {}", value.to_str()?);
}`,

    post_form: `let client = WasmClient::new();
let form = "username=rustacean&language=rust";
let resp = client.post("https://httpbin.org/post")?
    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
    .body(form)
    .send().await?;`,

    post_binary: `let client = WasmClient::new();
let binary: Vec<u8> = (0..256).map(|i| i as u8).collect();
let resp = client.post("https://httpbin.org/post")?
    .header(CONTENT_TYPE, "application/octet-stream")
    .body(binary)
    .send().await?;`,

    fetch_anything: `let client = WasmClient::new();
let resp = client.request(Method::OPTIONS, "https://httpbin.org/anything")?
    .send().await?;
// httpbin.org/anything accepts any method`,

    fetch_with_timeout: `let client = WasmClient::builder()
    .timeout(Duration::from_secs(3))
    .build();
// Endpoint delays 1s — well within our 3s limit
let resp = client.get("https://httpbin.org/delay/1")?
    .send().await?;`,

    fetch_timeout_fail: `let client = WasmClient::builder()
    .timeout(Duration::from_secs(3))
    .build();
// Endpoint delays 10s — exceeds our 3s limit!
let resp = client.get("https://httpbin.org/delay/10")?
    .send().await?;
// → Error::Timeout`,

    error_handling: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/status/404")?
    .send().await?;
match resp.error_for_status() {
    Ok(r) => println!("OK: {}", r.text()?),
    Err(e) => println!("Error: {e}"), // 404
}`,

    error_handling_500: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/status/500")?
    .send().await?;
match resp.error_for_status() {
    Ok(r) => println!("OK: {}", r.text()?),
    Err(e) => println!("Server error: {e}"), // 500
}`,

    fetch_redirect: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/redirect/3")?
    .send().await?;
// Browser follows redirects automatically
println!("Final URL: {}", resp.url());`,

    fetch_status_codes: `let client = WasmClient::new();
let base = "https://httpbin.org/status";
for code in [200, 201, 301, 400, 404, 500, 503] {
    let resp = client.get(&format!("{base}/{code}"))?
        .send().await?;
    let is_err = resp.error_for_status().is_err();
    println!("{code}: {}", if is_err { "error" } else { "ok" });
}`,

    fetch_conditional: `let client = WasmClient::new();
// Request 1: get the ETag
let resp = client.get("https://httpbin.org/etag/test123")?
    .send().await?;
let etag = resp.headers()["etag"].to_str()?;

// Request 2: conditional — If-None-Match
let resp = client.get("https://httpbin.org/etag/test123")?
    .header(IF_NONE_MATCH, etag)
    .send().await?;
// → 304 Not Modified (no body transferred!)`,

    fetch_cache_headers: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/cache/60")?
    .header(CACHE_CONTROL, "no-cache")
    .header("x-request-id", "demo-12345")
    .send().await?;
// Inspect cache-control, etag, expires headers`,

    fetch_gzip: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/gzip")?
    .header(ACCEPT_ENCODING, "gzip, deflate, br")
    .send().await?;
// Browser transparently decompresses`,

    fetch_content_negotiation: `let client = WasmClient::new();
let resp = client.get("https://httpbin.org/anything")?
    .header(ACCEPT, "application/xml")
    .send().await?;
// Server responds based on Accept header`,

    fetch_multiple: `let client = WasmClient::new();
let urls = ["https://httpbin.org/ip",
            "https://httpbin.org/get",
            "https://httpbin.org/user-agent"];
for url in urls {
    let resp = client.get(url)?.send().await?;
    println!("{}: {} bytes", url, resp.bytes().len());
}`,

	fetch_cookie_session: `let client = WasmClient::new();
	let jar = aioduct::CookieJar::new();

	// Step 1: Set-Cookie from server
	let resp = client.get("/cookies/set/session_id/demo12345")?
	    .send().await?;
	jar.store_from_response("httpbin.org", "/", resp.headers());

	// Step 2: Apply cookies to next request
	let mut headers = HeaderMap::new();
	jar.apply_to_request("httpbin.org", true, "/", None, &mut headers);
	let resp = client.get("/cookies")?
	    .headers(headers).send().await?;
	// Server now sees session_id cookie!`,

	fetch_link_pagination: `let resp = client.get(url)?.send().await?;
	let links = aioduct::link::parse_link_headers(resp.headers());
	for link in &links {
	    println!("URI: {}", link.uri());
	    if let Some(rel) = link.rel() { println!("Rel: {rel}"); }
	    if let Some(title) = link.title() { println!("Title: {title}"); }
	}`,

	fetch_problem_details: `let resp = client.get(url)?
	    .header(ACCEPT, "application/problem+json")
	    .send().await?;
	let problem: aioduct::ProblemDetails = resp.json()?;
	println!("Type: {:?}", problem.problem_type);
	println!("Title: {:?}", problem.title);
	println!("Status: {:?}", problem.status);
	// Parses RFC 9457 error responses`,
};

async function initWasm() {
    const status = document.getElementById('wasm-status');
    const runBtn = document.getElementById('run-btn');

    try {
        wasmModule = await import('./wasm/aioduct_wasm_demo.js');
        await wasmModule.default();
        status.textContent = 'Ready';
        status.className = 'demo-status ready';
        runBtn.disabled = false;
    } catch (e) {
        status.textContent = 'WASM unavailable';
        status.className = 'demo-status error';
        console.warn('WASM init failed:', e);
        const output = document.getElementById('demo-output');
        output.innerHTML = '<span class="placeholder">WebAssembly is not available in your browser. The demo shows aioduct making real HTTP requests from WASM compiled Rust code.</span>';
    }
}

async function runDemo() {
    if (!wasmModule) return;

    const output = document.getElementById('demo-output');
    const runBtn = document.getElementById('run-btn');
    const select = document.getElementById('demo-select');
    const selected = select.options[select.selectedIndex];
    const demoType = selected.value;
    const url = selected.dataset.url;

    output.className = 'demo-output loading';
    output.textContent = 'Fetching…';
    runBtn.disabled = true;

    try {
        let result;
        switch (demoType) {
            case 'fetch_json':
                result = await wasmModule.fetch_json(url);
                break;
            case 'fetch_url':
                result = await wasmModule.fetch_url(url);
                break;
            case 'fetch_ip_info':
                result = await wasmModule.fetch_ip_info();
                break;
            case 'fetch_html':
                result = await wasmModule.fetch_html(url);
                break;
            case 'fetch_utf8':
                result = await wasmModule.fetch_utf8(url);
                break;
            case 'fetch_large_response':
                result = await wasmModule.fetch_large_response(url);
                break;
            case 'fetch_with_headers':
                result = await wasmModule.fetch_with_headers(url);
                break;
            case 'fetch_with_bearer':
                result = await wasmModule.fetch_with_bearer_auth(url, 'demo-token-12345');
                break;
            case 'fetch_basic_auth':
                result = await wasmModule.fetch_basic_auth(url, 'demo', 'secret');
                break;
            case 'fetch_user_agent':
                result = await wasmModule.fetch_user_agent_echo('MyApp/2.0 (Rust; aioduct)');
                break;
            case 'fetch_cookies':
                result = await wasmModule.fetch_cookies(url);
                break;
            case 'fetch_response_headers':
                result = await wasmModule.fetch_response_headers(url);
                break;
            case 'post_json':
                result = await wasmModule.post_json(url, JSON.stringify({name: 'aioduct', version: '0.1.7'}));
                break;
            case 'put_json':
                result = await wasmModule.put_json(url, JSON.stringify({id: 1, name: 'updated'}));
                break;
            case 'patch_json':
                result = await wasmModule.patch_json(url, JSON.stringify({name: 'patched'}));
                break;
            case 'delete_request':
                result = await wasmModule.delete_request(url);
                break;
            case 'head_request':
                result = await wasmModule.head_request(url);
                break;
            case 'post_form':
                result = await wasmModule.post_form_urlencoded(url);
                break;
            case 'post_binary':
                result = await wasmModule.post_binary(url);
                break;
            case 'fetch_anything':
                result = await wasmModule.fetch_anything('OPTIONS', url);
                break;
            case 'fetch_with_timeout':
                result = await wasmModule.fetch_with_timeout(url, 3000);
                break;
            case 'fetch_timeout_fail':
                result = await wasmModule.fetch_with_timeout(url, 3000);
                break;
            case 'error_handling':
            case 'error_handling_500':
                result = await wasmModule.fetch_error_handling(url);
                break;
            case 'fetch_redirect':
                result = await wasmModule.fetch_redirect(url);
                break;
            case 'fetch_status_codes':
                result = await wasmModule.fetch_status_codes(url);
                break;
            case 'fetch_conditional':
                result = await wasmModule.fetch_conditional(url);
                break;
            case 'fetch_cache_headers':
                result = await wasmModule.fetch_cache_headers(url);
                break;
            case 'fetch_gzip':
                result = await wasmModule.fetch_gzip(url);
                break;
            case 'fetch_content_negotiation':
                result = await wasmModule.fetch_content_negotiation(url, 'application/xml');
                break;
            case 'fetch_multiple':
                result = await wasmModule.fetch_multiple_sequential(url);
                break;
            case 'fetch_query_params':
                result = await wasmModule.fetch_query_params(url, 'search', 'rust wasm', 'limit', '10');
                break;
            case 'fetch_typed_query':
                result = await wasmModule.fetch_typed_query(url);
                break;
            case 'fetch_streaming':
                result = await wasmModule.fetch_streaming(url);
                break;
            case 'fetch_cookie_session':
                result = await wasmModule.fetch_cookie_session(url);
                break;
            case 'fetch_link_pagination':
                result = await wasmModule.fetch_link_pagination(url);
                break;
            case 'fetch_problem_details':
                result = await wasmModule.fetch_problem_details(url);
                break;
            default:
                result = await wasmModule.fetch_json(url);
        }
        output.className = 'demo-output success';
        output.textContent = result;
    } catch (e) {
        output.className = 'demo-output error';
        if (String(e).includes('Timeout') || String(e).includes('abort')) {
            output.textContent = `Request timed out after 3 seconds.\n\nThis demonstrates aioduct's built-in timeout handling.\nThe endpoint has a 10s delay, but our client bailed after 3s.`;
        } else {
            output.textContent = `Network issue — ${e}\n\nThis can happen if httpbin.org is temporarily unavailable. Try again.`;
        }
    } finally {
        runBtn.disabled = false;
    }
}

function updateDemoCode() {
    const select = document.getElementById('demo-select');
    const selected = select.options[select.selectedIndex];
    const demoType = selected.value;
    const url = selected.dataset.url;
    const codeEl = document.getElementById('demo-code');

    let code = DEMO_CODE[demoType] || DEMO_CODE.fetch_json;
    if (url && !code.includes(url.split('?')[0])) {
        code = `// ${url}\n${code}`;
    }
    codeEl.textContent = code;
}

function initDemo() {
    document.getElementById('run-btn').addEventListener('click', runDemo);
    document.getElementById('demo-select').addEventListener('change', updateDemoCode);
    initWasm();
}

// --- Init ---

document.addEventListener('DOMContentLoaded', () => {
    initConfigurator();
    initTabs();
    initDemo();
});
