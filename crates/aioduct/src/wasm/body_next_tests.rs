use super::*;
use crate::traits::ByteStreamExt;
use std::task::Waker;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = "
export function fixture() {
    let controller;
    const stream = new ReadableStream({start(c) { controller = c; }});
    return { reader: stream.getReader(), controller };
}
export function push(f, text) { f.controller.enqueue(new TextEncoder().encode(text)); }
export function close(f) { f.controller.close(); }
export function fail(f) { f.controller.error(new Error('body read failure')); }
")]
extern "C" {
    fn fixture() -> JsValue;
    fn push(f: &JsValue, text: &str);
    fn close(f: &JsValue);
    fn fail(f: &JsValue);
}
fn body(f: &JsValue) -> WasmBodyStream {
    WasmBodyStream {
        reader: Some(
            js_sys::Reflect::get(f, &"reader".into())
                .unwrap()
                .unchecked_into(),
        ),
        done: false,
        observation: None,
    }
}
async fn generic_next<S: ByteStreamExt>(stream: &mut S) -> Option<Result<Bytes, Error>> {
    ByteStreamExt::next(stream).await
}

#[wasm_bindgen_test(async)]
async fn named_body_next_is_lazy_and_preserves_empty_chunks_and_eof() {
    let f = fixture();
    let mut stream = body(&f);
    drop(ByteStreamExt::next(&mut stream));
    push(&f, "");
    push(&f, "body");
    close(&f);
    // Unlike native BodyStream, the host byte stream already delivers empty chunks.
    assert!(generic_next(&mut stream).await.unwrap().unwrap().is_empty());
    assert_eq!(
        generic_next(&mut stream).await.unwrap().unwrap().as_ref(),
        b"body"
    );
    assert!(generic_next(&mut stream).await.is_none());
    assert!(generic_next(&mut stream).await.is_none());
}

#[wasm_bindgen_test(async)]
async fn repoll_reuses_one_read_and_cancel_keeps_existing_promise_behavior() {
    let f = fixture();
    let mut stream = body(&f);
    {
        let mut next = ByteStreamExt::next(&mut stream);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(Pin::new(&mut next).poll(&mut cx).is_pending());
        assert!(Pin::new(&mut next).poll(&mut cx).is_pending());
    }
    let mut next = ByteStreamExt::next(&mut stream);
    assert!(
        Pin::new(&mut next)
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    push(&f, "consumed by dropped read");
    push(&f, "delivered");
    close(&f);
    assert_eq!(next.await.unwrap().unwrap().as_ref(), b"delivered");
    assert!(generic_next(&mut stream).await.is_none());
}

#[wasm_bindgen_test(async)]
async fn js_read_rejection_is_returned_once() {
    let f = fixture();
    let mut stream = body(&f);
    fail(&f);
    let error = generic_next(&mut stream).await.unwrap().unwrap_err();
    assert!(matches!(error, Error::Other(_)));
    assert!(generic_next(&mut stream).await.is_none());
}

#[wasm_bindgen_test(async)]
async fn missing_reader_is_immediate_eof() {
    let mut stream = WasmBodyStream {
        reader: None,
        done: false,
        observation: None,
    };
    assert!(generic_next(&mut stream).await.is_none());
    assert!(generic_next(&mut stream).await.is_none());
}

fn sse(f: &JsValue) -> WasmSseStream {
    WasmSseStream {
        body: body(f),
        buf: BytesMut::new(),
        decoder: SseDecoder::new(),
        done: false,
        observation: None,
    }
}

#[wasm_bindgen_test(async)]
async fn named_sse_next_parses_protocol_events_via_generic_trait() {
    use crate::traits::SseStreamExt;
    let f = fixture();
    let mut stream = sse(&f);
    drop(SseStreamExt::next(&mut stream));
    push(
        &f,
        ":comment\nretry: 3\n\nevent: custom\ndata: first\ndata: second\n\ndata: tail",
    );
    close(&f);
    assert_eq!(
        SseStreamExt::next(&mut stream).await.unwrap().unwrap(),
        SseEvent::Retry(3)
    );
    let SseEvent::Message(message) = SseStreamExt::next(&mut stream).await.unwrap().unwrap() else {
        panic!("expected message");
    };
    assert_eq!(message.event, "custom");
    assert_eq!(message.data, "first\nsecond");
    assert!(SseStreamExt::next(&mut stream).await.is_none());
    assert!(SseStreamExt::next(&mut stream).await.is_none());
}

#[wasm_bindgen_test(async)]
async fn named_sse_next_cancel_preserves_existing_js_promise_behavior() {
    use crate::traits::SseStreamExt;
    let f = fixture();
    let mut stream = sse(&f);
    {
        let mut next = SseStreamExt::next(&mut stream);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(Pin::new(&mut next).poll(&mut cx).is_pending());
        assert!(Pin::new(&mut next).poll(&mut cx).is_pending());
    }
    let mut next = SseStreamExt::next(&mut stream);
    assert!(
        Pin::new(&mut next)
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    push(&f, "data: consumed by dropped read\n\n");
    push(&f, "data: delivered\n\n");
    close(&f);
    let SseEvent::Message(message) = next.await.unwrap().unwrap() else {
        panic!("expected message");
    };
    assert_eq!(message.data, "delivered");
    assert!(SseStreamExt::next(&mut stream).await.is_none());
}

#[wasm_bindgen_test(async)]
async fn named_sse_next_returns_recoverable_limit_error_and_later_message() {
    use crate::traits::SseStreamExt;
    let f = fixture();
    let mut stream = SseStreamExt::with_max_payload_size(sse(&f), 12);
    push(&f, "data: too-large-payload\n\ndata: ok\n\n");
    close(&f);
    assert!(matches!(
        SseStreamExt::next(&mut stream).await.unwrap().unwrap_err(),
        Error::SseEventTooLarge { limit_bytes: 12 }
    ));
    assert!(matches!(
        SseStreamExt::next(&mut stream).await.unwrap().unwrap(),
        SseEvent::Message(_)
    ));
    assert!(SseStreamExt::next(&mut stream).await.is_none());
}

#[wasm_bindgen_test(async)]
async fn named_sse_next_returns_js_rejection_without_hiding_error() {
    use crate::traits::SseStreamExt;
    let f = fixture();
    let mut stream = sse(&f);
    fail(&f);
    assert!(matches!(
        SseStreamExt::next(&mut stream).await.unwrap().unwrap_err(),
        Error::Other(_)
    ));
    assert!(SseStreamExt::next(&mut stream).await.is_none());
}
