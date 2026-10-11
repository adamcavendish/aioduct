use super::*;
use crate::traits::ByteStreamExt;
use http_body::{Body, Frame};
use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

enum Step {
    Frame(Frame<Bytes>),
    Error(Error),
    Pending,
}
struct Script {
    steps: VecDeque<Step>,
    polls: Arc<AtomicUsize>,
}
impl Body for Script {
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        self.polls.fetch_add(1, Ordering::Relaxed);
        match self.steps.pop_front() {
            Some(Step::Frame(frame)) => Poll::Ready(Some(Ok(frame))),
            Some(Step::Error(error)) => Poll::Ready(Some(Err(error))),
            Some(Step::Pending) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            None => Poll::Ready(None),
        }
    }
}
fn script(steps: impl IntoIterator<Item = Step>) -> (Script, Arc<AtomicUsize>) {
    let polls = Arc::new(AtomicUsize::new(0));
    (
        Script {
            steps: steps.into_iter().collect(),
            polls: polls.clone(),
        },
        polls,
    )
}
fn run<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..100 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
    panic!("script did not complete");
}
async fn collect<S: ByteStreamExt>(mut stream: S) -> Result<Vec<Bytes>, Error> {
    let mut chunks = Vec::new();
    while let Some(chunk) = ByteStreamExt::next(&mut stream).await {
        chunks.push(chunk?);
    }
    Ok(chunks)
}
fn collect_send<S>(stream: S) -> impl Future<Output = Result<Vec<Bytes>, Error>> + Send
where
    S: ByteStreamExt + Send,
    for<'a> S::Next<'a>: Send,
{
    collect(stream)
}
fn trailer(name: &'static str, value: &'static str) -> Step {
    let mut headers = http::HeaderMap::new();
    headers.insert(name, http::HeaderValue::from_static(value));
    Step::Frame(Frame::trailers(headers))
}
fn data(bytes: &'static [u8]) -> Step {
    Step::Frame(Frame::data(Bytes::from_static(bytes)))
}

#[test]
fn send_trait_next_keeps_empty_frame_and_trailer_contract() {
    let (body, _) = script([
        data(b""),
        trailer("x-first", "one"),
        data(b"hello"),
        trailer("x-last", "two"),
        data(b""),
    ]);
    let mut stream = BodyStreamSend::new(body.boxed_unsync());
    let chunk = run(ByteStreamExt::next(&mut stream)).unwrap().unwrap();
    assert_eq!(chunk.as_ref(), b"hello");
    assert!(run(ByteStreamExt::next(&mut stream)).is_none());
    assert!(run(ByteStreamExt::next(&mut stream)).is_none());
    assert_eq!(stream.trailers().unwrap()["x-first"], "one");
    assert_eq!(stream.trailers().unwrap()["x-last"], "two");
}

#[test]
fn cancelling_send_next_keeps_consumed_trailers_and_remaining_data() {
    let (body, polls) = script([
        data(b""),
        trailer("x-before", "kept"),
        Step::Pending,
        data(b"after"),
    ]);
    let mut stream = BodyStreamSend::new(body.boxed_unsync());
    drop(ByteStreamExt::next(&mut stream));
    assert_eq!(polls.load(Ordering::Relaxed), 0);
    {
        let mut next = ByteStreamExt::next(&mut stream);
        assert!(
            Pin::new(&mut next)
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
    }
    assert_eq!(polls.load(Ordering::Relaxed), 3);
    assert_eq!(stream.trailers().unwrap()["x-before"], "kept");
    assert_eq!(
        run(ByteStreamExt::next(&mut stream))
            .unwrap()
            .unwrap()
            .as_ref(),
        b"after"
    );
    assert!(run(ByteStreamExt::next(&mut stream)).is_none());
}

struct LocalScript(Script, Rc<()>);
impl Body for LocalScript {
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let this = self.get_mut();
        let _ = &this.1; // Deliberately keep the underlying source !Send.
        Pin::new(&mut this.0).poll_frame(cx)
    }
}
#[test]
fn local_trait_next_preserves_rc_source_trailers_and_cancellation() {
    let (body, polls) = script([
        trailer("x-local", "kept"),
        Step::Pending,
        data(b"local"),
        data(b""),
    ]);
    let body = ResponseBodyLocal::from_inner(Box::pin(LocalScript(body, Rc::new(()))));
    let mut stream = BodyStreamLocal::from_response_body(body);
    drop(ByteStreamExt::next(&mut stream));
    assert_eq!(polls.load(Ordering::Relaxed), 0);
    {
        let mut next = ByteStreamExt::next(&mut stream);
        assert!(
            Pin::new(&mut next)
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
    }
    assert_eq!(stream.trailers().unwrap()["x-local"], "kept");
    assert_eq!(
        run(collect(stream)).unwrap(),
        [Bytes::from_static(b"local")]
    );
}

#[derive(Debug)]
struct IdentityError(u8);
impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "identity error {}", self.0)
    }
}
impl std::error::Error for IdentityError {}
fn check_error<S: ByteStreamExt>(mut stream: S, identity: usize, polls: &AtomicUsize) {
    let Error::Other(error) = run(ByteStreamExt::next(&mut stream)).unwrap().unwrap_err() else {
        panic!()
    };
    assert_eq!(&*error as *const _ as *const () as usize, identity);
    let before = polls.load(Ordering::Relaxed);
    assert!(run(ByteStreamExt::next(&mut stream)).is_none());
    assert!(run(ByteStreamExt::next(&mut stream)).is_none());
    assert_eq!(polls.load(Ordering::Relaxed), before);
}
#[test]
fn error_is_moved_unchanged_and_both_native_streams_remain_fused() {
    for local in [false, true] {
        let error = Box::new(IdentityError(1));
        let identity = &*error as *const _ as usize;
        let (body, polls) = script([Step::Error(Error::Other(error)), data(b"must not be read")]);
        if local {
            check_error(
                BodyStreamLocal::from_response_body(ResponseBodyLocal::from_inner(Box::pin(
                    LocalScript(body, Rc::new(())),
                ))),
                identity,
                &polls,
            );
        } else {
            check_error(BodyStreamSend::new(body.boxed_unsync()), identity, &polls);
        }
    }
}

#[test]
fn generic_send_future_moves_to_64_concurrent_threads() {
    let barrier = Arc::new(Barrier::new(64));
    let threads: Vec<_> = (0..64)
        .map(|_| {
            let (body, _) = script([data(b"hello"), Step::Pending, data(b"world")]);
            let future = collect_send(BodyStreamSend::new(body.boxed_unsync()));
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                assert_eq!(
                    run(future).unwrap(),
                    [Bytes::from_static(b"hello"), Bytes::from_static(b"world")]
                );
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}
