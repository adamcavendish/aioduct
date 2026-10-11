#![cfg(feature = "tokio")]

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::h1::h1_server;

#[path = "support/observation.rs"]
mod observation_support;
use observation_support::TestObserver;

#[tokio::test]
async fn typed_observer_reports_selected_headers_and_one_body_finish() {
    let (addr, _) = h1_server().await;
    let observer = TestObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(observer.clone())
        .build()
        .unwrap();
    // The response can be held in another structure without its Observer type.
    struct Holder {
        response: Option<aioduct::Response>,
    }
    let mut holder = Holder {
        response: Some(
            client
                .get(&format!("http://{addr}/"))
                .unwrap()
                .send()
                .await
                .unwrap(),
        ),
    };
    let before = observer.phases();
    assert_eq!(
        before
            .iter()
            .filter(|fact| **fact == "RequestStart")
            .count(),
        1
    );
    assert_eq!(
        before
            .iter()
            .filter(|fact| **fact == "ResponseHeaders")
            .count(),
        1
    );
    assert!(!before.iter().any(|fact| fact.starts_with("RequestFinish")));
    assert_eq!(
        holder.response.take().unwrap().text().await.unwrap(),
        "hello aioduct"
    );
    let after = observer.phases();
    assert_eq!(
        after
            .iter()
            .filter(|fact| **fact == "ResponseBodyEnded")
            .count(),
        1
    );
    assert_eq!(
        after
            .iter()
            .filter(|fact| **fact == "RequestFinish(Ended)")
            .count(),
        1
    );
}

#[tokio::test]
async fn pool_stats_report_hit_miss_and_inventory() {
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    let initial = client.pool_stats();
    assert_eq!(initial.checkout_hits, 0);
    assert_eq!(initial.checkout_misses, 0);
    assert_eq!(initial.idle_pool_entries, 0);
    assert_eq!(initial.checked_out_pool_handles, 0);

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    let after_first = client.pool_stats();
    assert_eq!(after_first.checkout_misses, 1);
    assert_eq!(after_first.checkout_hits, 0);
    assert_eq!(after_first.idle_pool_entries, 1);
    assert_eq!(after_first.checked_out_pool_handles, 0);
    assert_eq!(after_first.hosts.len(), 1);
    assert_eq!(after_first.hosts[0].scheme, "http");
    assert_eq!(after_first.hosts[0].authority, addr.to_string());
    assert_eq!(after_first.hosts[0].route, "direct");
    assert_eq!(after_first.hosts[0].idle, 1);
    assert_eq!(after_first.hosts[0].active, 0);

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    let after_second = client.pool_stats();
    assert_eq!(after_second.checkout_misses, 1);
    assert_eq!(after_second.checkout_hits, 1);
    assert_eq!(after_second.idle_pool_entries, 1);
    assert_eq!(after_second.checked_out_pool_handles, 0);
}

#[tokio::test]
async fn observer_no_events_when_not_configured() {
    let (addr, _counter) = h1_server().await;

    // Client without observer — should not panic or error
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
