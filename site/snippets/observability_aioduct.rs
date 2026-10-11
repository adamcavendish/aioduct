// features: tokio,tracing
// runtime: tokio
use aioduct::{
    Interest, NoopConnectionObservation, Observer, RequestFinish, RequestInfo,
    RequestObservation, ResponseHeaders, TokioClient,
};

struct Events;
impl Observer for Events {
    type Context = ();
    type Observation = Events;
    type ConnectionObservation = NoopConnectionObservation;

    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Events) {
        (
            Interest::RESPONSE_HEADERS | Interest::REQUEST_FINISH,
            Events,
        )
    }

    fn begin_connection(&self) -> (Interest, NoopConnectionObservation) {
        (Interest::EMPTY, NoopConnectionObservation)
    }
}

impl RequestObservation for Events {
    fn on_response_headers(&self, event: &ResponseHeaders) {
        println!("response status: {}", event.response.status);
    }

    fn on_request_finish(&self, event: &RequestFinish<'_>) {
        println!("request finished: {:?}", event.reason);
    }
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder().observer(Events).build()?;
    let resp = client.get("https://httpbin.org/get")?.send().await?;
    println!("Status: {}", resp.status());
    let _body = resp.bytes().await?;
    Ok(())
}
