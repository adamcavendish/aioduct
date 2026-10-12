#[path = "../../../common/observability/mod.rs"]
mod observability;

use std::time::Duration;

use aioduct::{CompioClient, RetryConfig};
use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt as _, StreamBody};
use hyper::body::Frame;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = observability::fixture::FixtureServer::start()?;

    compio_runtime::Runtime::new()?.block_on(async {
        let trace = std::env::args().any(|arg| arg == "--trace");
        let observer = observability::ReferenceObserver::new("compio", trace);
        observer.heading("complete callback reference");

        let client = CompioClient::builder()
            .observer(observer)
            .timeout(Duration::from_secs(10))
            .retry(
                RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::from_millis(25))
                    .max_backoff(Duration::from_millis(25)),
            )
            .build_local()?;

        observer.section(1, "ordinary GET · start, headers, body, finish");
        let body = client
            .get_local(&fixture.url("/ok"))?
            .observation_context(1)
            .send()
            .await?
            .bytes()
            .await?;
        observer.application_line(
            "HTTP",
            format!(
                "body {} · {}",
                body.len(),
                observability::text_preview(&body)
            ),
        );

        observer.section(2, "pool hit · reuse without a new physical connection");
        let body = client
            .get_local(&fixture.url("/ok"))?
            .observation_context(2)
            .send()
            .await?
            .text()
            .await?;
        observer.application_line(
            "HTTP",
            format!("body · {}", observability::text_preview(body.as_bytes())),
        );

        observer.section(3, "redirect · one logical request, multiple attempts");
        let response = client
            .get_local(&fixture.url("/redirect"))?
            .observation_context(3)
            .send()
            .await?;
        observer.application_line("HTTP", format!("final status={}", response.status()));
        let _ = response.bytes().await?;

        observer.section(4, "retry · 503, cleanup, backoff, then 200");
        let response = client
            .get_local(&fixture.url("/retry"))?
            .observation_context(4)
            .send()
            .await?;
        observer.application_line("HTTP", format!("final status={}", response.status()));
        let _ = response.bytes().await?;

        observer.section(5, "upload · request-body terminal observation");
        let response = client
            .post_local(&fixture.url("/upload"))?
            .observation_context(5)
            .body("request body observed once")
            .send()
            .await?;
        observer.application_line("HTTP", format!("body · {}", response.text().await?));

        observer.section(6, "failed upload · body error before response selection");
        let request_body = StreamBody::new(stream::iter(vec![Err::<Frame<Bytes>, _>(
            aioduct::Error::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "intentional upload fixture failure",
            )),
        )]))
        .boxed_unsync();
        let result = client
            .post_local(&fixture.url("/upload"))?
            .observation_context(6)
            .body_stream(request_body)
            .send()
            .await;
        match result {
            Ok(_) => observer.application_line("HTTP", "unexpected upload success"),
            Err(error) => observer.application_line(
                "HTTP",
                format!("expected error · {}", observability::compact_display(error)),
            ),
        }

        observer.section(7, "body failure · selected response fails while reading");
        let body_error_client = CompioClient::builder()
            .observer(observer)
            .retry(RetryConfig::default().max_retries(0))
            .build_local()?;
        let response = body_error_client
            .get_local(&fixture.url("/body-error"))?
            .observation_context(7)
            .send()
            .await?;
        match response.bytes().await {
            Ok(body) => observer.application_line(
                "HTTP",
                format!(
                    "unexpected body success · {}",
                    observability::text_preview(&body)
                ),
            ),
            Err(error) => observer.application_line(
                "HTTP",
                format!(
                    "expected body error · {}",
                    observability::error_text(&error)
                ),
            ),
        }

        observer.section(8, "SSE · lifecycle and selected statistics");
        let response = client
            .get_local(&fixture.url("/sse"))?
            .observation_context(8)
            .header(
                http::header::ACCEPT,
                http::HeaderValue::from_static("text/event-stream"),
            )
            .send()
            .await?;
        let mut events = response.into_sse_stream();
        while let Some(event) = events.next().await {
            observer.print_sse_application_event(&event);
        }

        observer.section(
            9,
            "SSE abandonment · HTTP and SSE lifetimes stay independent",
        );
        let response = client
            .get_local(&fixture.url("/sse"))?
            .observation_context(9)
            .send()
            .await?;
        let mut events = response.into_sse_stream();
        let _ = events.next().await;
        drop(events);

        observer.section(10, "SSE decoder error · original error is reported");
        let response = client
            .get_local(&fixture.url("/sse"))?
            .observation_context(10)
            .send()
            .await?;
        let mut events = response.into_sse_stream().with_max_payload_size(8);
        while let Some(event) = events.next().await {
            let failed = event.is_err();
            observer.print_sse_application_event(&event);
            if failed {
                break;
            }
        }
        drop(events);

        observer.section(11, "cancelled execution · no response is selected");
        let _ = compio_runtime::time::timeout(Duration::from_millis(20), async {
            if let Ok(request) = client.get_local(&fixture.url("/hang")) {
                let _ = request.observation_context(11).send().await;
            }
        })
        .await;
        observer.application_line("HTTP", "expected timeout");

        observer.section(12, "per-request opt-out · begin is not called");
        let response = client
            .get_local(&fixture.url("/status/204"))?
            .disable_observation()
            .send()
            .await?;
        observer.application_line("HTTP", format!("status={}", response.status()));
        let _ = response.bytes().await?;

        observer.section(
            13,
            "HTTP/1 Upgrade · response handoff and channel lifecycle",
        );
        let response = client
            .get_local(&fixture.url("/upgrade"))?
            .observation_context(13)
            .upgrade()
            .send()
            .await?;
        let mut channel = response.upgrade().await?;
        observability::channel_write_all(&mut channel, b"upgrade payload").await?;
        observability::channel_shutdown(&mut channel).await?;
        let echoed = observability::channel_read_to_end(&mut channel).await?;
        observer.application_line(
            "CHAN",
            format!("echo · {}", observability::text_preview(&echoed)),
        );

        observer.section(14, "channel detachment · local owner dropped");
        let response = client
            .get_local(&fixture.url("/upgrade"))?
            .observation_context(14)
            .upgrade()
            .send()
            .await?;
        let channel = response.upgrade().await?;
        drop(channel);

        observer.section(15, "HTTP/1 CONNECT · distinct handoff mode");
        let response = client
            .request_local(http::Method::CONNECT, &fixture.url(""))?
            .observation_context(15)
            .send()
            .await?;
        observer.application_line("HTTP", format!("status={}", response.status()));
        let mut channel = response.upgrade().await?;
        observability::channel_write_all(&mut channel, b"connect payload").await?;
        observability::channel_shutdown(&mut channel).await?;
        let echoed = observability::channel_read_to_end(&mut channel).await?;
        observer.application_line(
            "CHAN",
            format!("echo · {}", observability::text_preview(&echoed)),
        );

        drop(body_error_client);
        drop(client);
        observer.complete();
        Ok(())
    })
}
