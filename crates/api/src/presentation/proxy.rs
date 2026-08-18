//! Dormant HTTP/1 proxy handler for presentation hosts.
//!
//! Axum has already parsed ordinary HTTP by the time this is called, so the
//! request and response bodies stay in Hyper. Only a successful Upgrade turns
//! back into two byte streams, which are then handed to `proxies::relay`.

use std::{convert::Infallible, net::SocketAddr, time::Duration};

use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode, Uri,
        header::{CONNECTION, HOST, UPGRADE},
    },
};
use hyper_util::rt::TokioIo;
use proxies::Dial;

use crate::{
    error::AppError,
    models::presentation::UpstreamHostMode,
    presentation::{TokenResolution, resolve_token, token_from_host},
    state::AppState,
};

use super::resolve;

const ROBOTS: &str = "noindex, nofollow, noarchive";

/// Complete dormant handler. Eventual host dispatch may call this directly;
/// the current application router deliberately does not.
pub async fn handle(
    state: AppState,
    peer: Option<SocketAddr>,
    request: Request<Body>,
) -> Response<Body> {
    let public_host = match request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
    {
        Some(host) => host.to_owned(),
        None => return response(StatusCode::NOT_FOUND, "not found"),
    };
    let Some(token) = token_from_host(&public_host, &state.config.preview_domain) else {
        return response(StatusCode::NOT_FOUND, "not found");
    };
    let resolved = match resolve_token(&state, token).await {
        Ok(TokenResolution::Active {
            presentation,
            binding,
        }) => (presentation, binding),
        Ok(TokenResolution::Gone) => return response(StatusCode::GONE, "presentation revoked"),
        Ok(TokenResolution::Unknown) => return response(StatusCode::NOT_FOUND, "not found"),
        Err(error) => return app_error(error),
    };
    let (presentation, binding) = resolved;
    let port = match u16::try_from(presentation.port) {
        Ok(port) if port != 0 => port,
        _ => return response(StatusCode::BAD_GATEWAY, "presentation has an invalid port"),
    };

    let endpoint = match connect(&state, &binding, port).await {
        Ok(endpoint) => endpoint,
        Err(error) => return app_error(error),
    };
    forward_connected(
        endpoint.0,
        &public_host,
        port,
        &presentation.upstream_host_mode,
        &state.config.preview_scheme,
        peer,
        state.config.preview_header_timeout,
        request,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn forward_connected(
    stream: Box<dyn proxies::Stream>,
    public_host: &str,
    port: u16,
    upstream_host_mode: &str,
    scheme: &str,
    peer: Option<SocketAddr>,
    header_timeout: Duration,
    mut request: Request<Body>,
) -> Response<Body> {
    let upgrade_requested = is_upgrade(request.headers());
    let downstream_upgrade = upgrade_requested.then(|| hyper::upgrade::on(&mut request));
    prepare_request(
        &mut request,
        &public_host,
        port,
        upstream_host_mode,
        scheme,
        peer,
        upgrade_requested,
    );

    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    *request.uri_mut() = path
        .parse::<Uri>()
        .unwrap_or_else(|_| Uri::from_static("/"));

    let (mut sender, connection) =
        match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
            Ok(parts) => parts,
            Err(error) => {
                return response(
                    StatusCode::BAD_GATEWAY,
                    &format!("upstream HTTP handshake failed: {error}"),
                );
            }
        };
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            tracing::debug!(error = %error, "preview upstream connection ended");
        }
    });

    let mut upstream =
        match tokio::time::timeout(header_timeout, sender.send_request(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                return response(
                    StatusCode::BAD_GATEWAY,
                    &format!("upstream request failed: {error}"),
                );
            }
            Err(_) => return response(StatusCode::BAD_GATEWAY, "upstream response timed out"),
        };

    let upgraded = upstream.status() == StatusCode::SWITCHING_PROTOCOLS;
    let upstream_upgrade = upgraded.then(|| hyper::upgrade::on(&mut upstream));
    sanitize_response(upstream.headers_mut(), upgraded);
    upstream.headers_mut().insert(
        HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static(ROBOTS),
    );

    if let (Some(downstream), Some(upstream_io)) = (downstream_upgrade, upstream_upgrade) {
        tokio::spawn(async move {
            match tokio::try_join!(downstream, upstream_io) {
                Ok((downstream, upstream)) => {
                    let mut downstream = TokioIo::new(downstream);
                    let mut upstream = TokioIo::new(upstream);
                    if let Err(error) = proxies::relay(&mut downstream, &mut upstream).await {
                        tracing::debug!(error = %error, "preview upgraded relay ended");
                    }
                }
                Err(error) => tracing::debug!(error = %error, "preview upgrade failed"),
            }
        });
    }

    let (parts, body) = upstream.into_parts();
    Response::from_parts(parts, Body::new(body))
}

async fn connect(
    state: &AppState,
    binding: &crate::models::session::SessionEnvironment,
    port: u16,
) -> Result<(Box<dyn proxies::Stream>, String), AppError> {
    let first = resolve::endpoint(state, binding, port, false).await?;
    match tokio::time::timeout(
        state.config.preview_connect_timeout,
        first.dialer.dial(&first.address),
    )
    .await
    {
        Ok(Ok(stream)) => Ok((stream, first.address)),
        first_error => {
            let Some((container_id, network)) = first.container_cache else {
                return Err(dial_error(first_error));
            };
            state
                .presentation_addresses
                .invalidate(container_id, network.as_deref())
                .await;
            let retried = resolve::endpoint(state, binding, port, true).await?;
            tokio::time::timeout(
                state.config.preview_connect_timeout,
                retried.dialer.dial(&retried.address),
            )
            .await
            .map_err(|_| AppError::BadGateway("upstream connection timed out".into()))?
            .map(|stream| (stream, retried.address))
            .map_err(|error| AppError::BadGateway(error.to_string()))
        }
    }
}

fn dial_error(
    result: Result<Result<Box<dyn proxies::Stream>, proxies::Error>, tokio::time::error::Elapsed>,
) -> AppError {
    match result {
        Ok(Err(error)) => AppError::BadGateway(error.to_string()),
        Err(_) => AppError::BadGateway("upstream connection timed out".into()),
        Ok(Ok(_)) => unreachable!("successful connections return before error mapping"),
    }
}

fn prepare_request(
    request: &mut Request<Body>,
    public_host: &str,
    port: u16,
    mode: &str,
    scheme: &str,
    peer: Option<SocketAddr>,
    upgrade: bool,
) {
    sanitize_hop_headers(request.headers_mut(), upgrade);
    let upstream_host = if mode == UpstreamHostMode::Preserve.as_str() {
        public_host.to_owned()
    } else {
        format!("localhost:{port}")
    };
    if let Ok(value) = HeaderValue::from_str(&upstream_host) {
        request.headers_mut().insert(HOST, value);
    }
    // These describe this proxy hop. Do not let a browser-supplied chain make
    // the presented application believe a different public host or client.
    for name in [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
    ] {
        request.headers_mut().remove(name);
    }
    insert_header(request.headers_mut(), "x-forwarded-host", public_host);
    insert_header(request.headers_mut(), "x-forwarded-proto", scheme);
    if let Some(peer) = peer {
        insert_header(
            request.headers_mut(),
            "x-forwarded-for",
            &peer.ip().to_string(),
        );
    }
    let forwarded = match peer {
        Some(peer) => format!(
            "for=\"{}\";host=\"{}\";proto={}",
            peer.ip(),
            public_host,
            scheme
        ),
        None => format!("host=\"{}\";proto={}", public_host, scheme),
    };
    insert_header(request.headers_mut(), "forwarded", &forwarded);
}

fn sanitize_response(headers: &mut HeaderMap, upgrade: bool) {
    sanitize_hop_headers(headers, upgrade);
}

fn sanitize_hop_headers(headers: &mut HeaderMap, keep_upgrade: bool) {
    let upgrade_value = keep_upgrade
        .then(|| headers.get(UPGRADE).cloned())
        .flatten();
    let named = headers
        .get(CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for name in named {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
    if let Some(value) = upgrade_value {
        headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(UPGRADE, value);
    }
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers.get(UPGRADE).is_some()
        && headers
            .get(CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
}

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(HeaderName::from_static(name), value);
    }
}

fn app_error(error: AppError) -> Response<Body> {
    match error {
        AppError::BadGateway(message) | AppError::ServiceUnavailable(message) => {
            response(StatusCode::BAD_GATEWAY, &message)
        }
        other => {
            tracing::error!(error = %other, "preview request failed");
            response(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

fn response(status: StatusCode, message: &str) -> Response<Body> {
    let mut response = Response::new(Body::from(message.to_owned()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static(ROBOTS),
    );
    response
}

#[allow(dead_code)]
fn _body_is_infallible(_: Infallible) {}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn hop_headers_are_removed_but_a_real_upgrade_is_rebuilt() {
        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION, HeaderValue::from_static("keep-alive, Upgrade"));
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        sanitize_hop_headers(&mut headers, true);
        assert_eq!(headers.get(CONNECTION).unwrap(), "upgrade");
        assert_eq!(headers.get(UPGRADE).unwrap(), "websocket");
        assert!(!headers.contains_key("keep-alive"));
    }

    #[test]
    fn every_local_error_discourages_indexing() {
        let response = response(StatusCode::NOT_FOUND, "not found");
        assert_eq!(response.headers()["x-robots-tag"], ROBOTS);
    }

    #[test]
    fn host_modes_and_forwarding_headers_are_explicit() {
        let mut loopback = Request::builder()
            .uri("/asset.js")
            .body(Body::empty())
            .unwrap();
        prepare_request(
            &mut loopback,
            "p-token.preview.test",
            5173,
            "loopback",
            "https",
            Some("192.0.2.1:1234".parse().unwrap()),
            false,
        );
        assert_eq!(loopback.headers()[HOST], "localhost:5173");
        assert_eq!(
            loopback.headers()["x-forwarded-host"],
            "p-token.preview.test"
        );
        assert_eq!(loopback.headers()["x-forwarded-proto"], "https");

        prepare_request(
            &mut loopback,
            "p-token.preview.test",
            5173,
            "preserve",
            "https",
            None,
            false,
        );
        assert_eq!(loopback.headers()[HOST], "p-token.preview.test");
    }

    #[tokio::test]
    async fn connected_proxy_passes_redirects_through_and_sets_robots_policy() {
        let (proxy, mut server) = tokio::io::duplex(8 * 1024);
        let upstream = tokio::spawn(async move {
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                server.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /old HTTP/1.1\r\n"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("\r\nhost: localhost:5173\r\n")
            );
            server
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let request = Request::builder()
            .uri("/old")
            .header(HOST, "p-token.preview.test")
            .body(Body::empty())
            .unwrap();
        let response = forward_connected(
            Box::new(proxy),
            "p-token.preview.test",
            5173,
            "loopback",
            "https",
            None,
            Duration::from_secs(1),
            request,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers()["location"], "/next");
        assert_eq!(response.headers()["x-robots-tag"], ROBOTS);
        upstream.await.unwrap();
    }

    #[tokio::test]
    async fn connected_proxy_streams_a_chunked_response_body() {
        let (proxy, mut server) = tokio::io::duplex(8 * 1024);
        tokio::spawn(async move {
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                server.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            server
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n")
                .await
                .unwrap();
            tokio::task::yield_now().await;
            server.write_all(b"5\r\nworld\r\n0\r\n\r\n").await.unwrap();
        });
        let request = Request::builder()
            .uri("/events")
            .header(HOST, "p-token.preview.test")
            .body(Body::empty())
            .unwrap();
        let response = forward_connected(
            Box::new(proxy),
            "p-token.preview.test",
            5173,
            "preserve",
            "https",
            None,
            Duration::from_secs(1),
            request,
        )
        .await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"helloworld");
    }

    #[tokio::test]
    async fn connected_proxy_streams_a_request_body_upstream() {
        let (proxy, mut server) = tokio::io::duplex(8 * 1024);
        let payload = vec![b'x'; 64 * 1024];
        let expected = payload.clone();
        let upstream = tokio::spawn(async move {
            let mut head = Vec::new();
            let mut byte = [0_u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                server.read_exact(&mut byte).await.unwrap();
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).unwrap();
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap();
            let mut received = vec![0_u8; length];
            server.read_exact(&mut received).await.unwrap();
            assert_eq!(received, expected);
            server
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let request = Request::builder()
            .method("POST")
            .uri("/upload")
            .header(HOST, "p-token.preview.test")
            .body(Body::from(payload))
            .unwrap();
        let response = forward_connected(
            Box::new(proxy),
            "p-token.preview.test",
            5173,
            "loopback",
            "https",
            None,
            Duration::from_secs(1),
            request,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        upstream.await.unwrap();
    }

    #[tokio::test]
    async fn connected_proxy_preserves_the_websocket_upgrade_handshake() {
        let (proxy, mut server) = tokio::io::duplex(8 * 1024);
        let upstream = tokio::spawn(async move {
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                server.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            assert!(request.contains("\r\nconnection: upgrade\r\n"));
            assert!(request.contains("\r\nupgrade: websocket\r\n"));
            server
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let request = Request::builder()
            .uri("/hmr")
            .header(HOST, "p-token.preview.test")
            .header(CONNECTION, "upgrade")
            .header(UPGRADE, "websocket")
            .body(Body::empty())
            .unwrap();
        let response = forward_connected(
            Box::new(proxy),
            "p-token.preview.test",
            5173,
            "preserve",
            "https",
            None,
            Duration::from_secs(1),
            request,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(response.headers()[CONNECTION], "upgrade");
        assert_eq!(response.headers()[UPGRADE], "websocket");
        upstream.await.unwrap();
    }
}
