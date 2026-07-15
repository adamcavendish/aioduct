use http::header::HOST;

use super::*;

const MALFORMED_AUTHORITIES: [&str; 4] = [
    "example.test:not-a-port",
    "example.test:99999",
    "[2001:db8::1]:not-a-port",
    "[2001:db8::1]:99999",
];

const EMPTY_PORT_AUTHORITIES: [&str; 2] = ["example.test:", "[2001:db8::1]:"];

fn origin_parts(version: http::Version, host: &str) -> http::request::Parts {
    let mut request = http::Request::builder()
        .version(version)
        .uri("/upload")
        .body(())
        .unwrap();
    request
        .headers_mut()
        .insert(HOST, http::HeaderValue::from_str(host).unwrap());
    request.into_parts().0
}

fn absolute_parts(version: http::Version, authority: &str) -> http::request::Parts {
    let uri = http::Uri::builder()
        .scheme("https")
        .authority(authority)
        .path_and_query("/upload")
        .build()
        .unwrap();
    http::Request::builder()
        .version(version)
        .uri(uri)
        .body(())
        .unwrap()
        .into_parts()
        .0
}

#[test]
fn h1_host_rejects_malformed_explicit_dns_and_ipv6_ports() {
    for authority in MALFORMED_AUTHORITIES {
        let error = InboundRequestTarget::capture(&origin_parts(http::Version::HTTP_11, authority))
            .unwrap_err();
        assert!(
            matches!(error, Error::InvalidHeader(ref message) if message.contains("invalid explicit port")),
            "unexpected error for {authority}: {error}"
        );
    }
}

#[test]
fn h1_absolute_form_rejects_malformed_explicit_dns_and_ipv6_ports() {
    for authority in MALFORMED_AUTHORITIES {
        let mut parts = absolute_parts(http::Version::HTTP_11, authority);
        parts
            .headers
            .insert(HOST, http::HeaderValue::from_static("downstream.example"));
        assert!(
            InboundRequestTarget::capture(&parts).is_err(),
            "accepted {authority}"
        );
    }
}

#[test]
fn h2_and_h3_reject_malformed_uri_and_host_authority_ports() {
    for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
        for authority in MALFORMED_AUTHORITIES {
            let uri_error =
                InboundRequestTarget::capture(&absolute_parts(version, authority)).unwrap_err();
            assert!(
                matches!(uri_error, Error::InvalidHeader(ref message) if message.contains("invalid explicit port")),
                "unexpected URI authority error for {version:?} {authority}: {uri_error}"
            );

            let mut host_parts = absolute_parts(version, "example.test");
            host_parts
                .headers
                .insert(HOST, http::HeaderValue::from_str(authority).unwrap());
            let host_error = InboundRequestTarget::capture(&host_parts).unwrap_err();
            assert!(
                matches!(host_error, Error::InvalidHeader(ref message) if message.contains("invalid explicit port")),
                "unexpected Host authority error for {version:?} {authority}: {host_error}"
            );
        }
    }
}

#[test]
fn omitted_dns_and_ipv6_ports_remain_valid_for_h1_h2_and_h3() {
    for authority in ["example.test", "[2001:db8::1]"] {
        assert!(
            InboundRequestTarget::capture(&origin_parts(http::Version::HTTP_11, authority)).is_ok(),
            "HTTP/1.1 rejected {authority}"
        );
        for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
            assert!(
                InboundRequestTarget::capture(&absolute_parts(version, authority)).is_ok(),
                "{version:?} rejected {authority}"
            );
        }
    }
}

#[test]
fn empty_dns_and_ipv6_ports_remain_valid_outside_connect_targets() {
    for authority in EMPTY_PORT_AUTHORITIES {
        assert!(
            InboundRequestTarget::capture(&origin_parts(http::Version::HTTP_11, authority)).is_ok(),
            "HTTP/1.1 rejected {authority}"
        );
        for version in [
            http::Version::HTTP_11,
            http::Version::HTTP_2,
            http::Version::HTTP_3,
        ] {
            let mut parts = absolute_parts(version, authority);
            if version == http::Version::HTTP_11 {
                parts
                    .headers
                    .insert(HOST, http::HeaderValue::from_static("downstream.example"));
            }
            assert!(
                InboundRequestTarget::capture(&parts).is_ok(),
                "{version:?} rejected {authority}"
            );
        }
    }
}

#[test]
fn empty_ports_are_rejected_in_ordinary_connect_targets() {
    for authority in EMPTY_PORT_AUTHORITIES {
        let parts = http::Request::builder()
            .method(http::Method::CONNECT)
            .version(http::Version::HTTP_11)
            .uri(http::Uri::builder().authority(authority).build().unwrap())
            .header(HOST, authority)
            .body(())
            .unwrap()
            .into_parts()
            .0;
        assert!(
            InboundRequestTarget::capture(&parts).is_err(),
            "accepted CONNECT target {authority}"
        );
    }
}

#[test]
fn empty_and_omitted_connect_host_ports_are_rejected() {
    for (target, empty_host, omitted_host) in [
        ("example.test:80", "example.test:", "example.test"),
        ("[2001:db8::1]:443", "[2001:db8::1]:", "[2001:db8::1]"),
    ] {
        let connect_parts = |host: &str| {
            http::Request::builder()
                .method(http::Method::CONNECT)
                .version(http::Version::HTTP_11)
                .uri(http::Uri::builder().authority(target).build().unwrap())
                .header(HOST, host)
                .body(())
                .unwrap()
                .into_parts()
                .0
        };

        assert!(
            InboundRequestTarget::capture(&connect_parts(empty_host)).is_err(),
            "accepted empty CONNECT Host port {empty_host}"
        );
        assert!(
            InboundRequestTarget::capture(&connect_parts(omitted_host)).is_err(),
            "accepted omitted CONNECT Host port {omitted_host}"
        );
    }
}
