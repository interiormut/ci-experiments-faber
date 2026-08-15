//! The outbound leg: WebSocket over TLS on 443, through whatever proxy the
//! machine this daemon runs on is configured to use (X40).
//!
//! Dialing this way, deliberately, is why this daemon exists: an egress
//! proxy that passes ordinary HTTPS traffic carries a WebSocket upgrade
//! without special-casing it, where a raw TLS socket on a nonstandard port
//! is exactly what such a proxy exists to block. Honoring the *ambient*
//! `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY` here does not conflict with R11 —
//! this daemon is the caller, running on the caller's own infrastructure,
//! so reading its own environment is the trust decision R11 reserves to
//! whoever owns it.

use std::sync::Arc;
use std::time::Duration;

use http::header::AUTHORIZATION;
use rand::RngExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::config::Config;
use crate::ws_stream::WsStream;

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Reconnects forever, jittered backoff between attempts. There is
/// deliberately no supervisor logic beyond this (X40): a dropped connection
/// — a blip, an API restart, R16 preemption by a newer connection — is
/// handled the same way every time, by reconnecting.
pub async fn run_forever(config: &Config) -> ! {
    let mut backoff = MIN_BACKOFF;
    loop {
        match connect_and_serve(config).await {
            Ok(()) => {
                tracing::info!("connection ended; reconnecting");
                backoff = MIN_BACKOFF;
            }
            Err(error) => {
                tracing::warn!(%error, "connect attempt failed");
            }
        }

        let jitter = rand::rng().random_range(0.5..1.5);
        let wait = backoff.mul_f64(jitter);
        tokio::time::sleep(wait).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn connect_and_serve(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let target = parse_api_url(&config.api)?;
    let tcp = dial_tcp(&target.host, target.port).await?;

    let stream: MaybeTls = if target.tls {
        let connector = tls_connector();
        let server_name = rustls_pki_types::ServerName::try_from(target.host.clone())?;
        MaybeTls::Tls(Box::new(connector.connect(server_name, tcp).await?))
    } else {
        // http:// only ever happens against a local dev instance — X40's
        // "TLS on 443" is the deliberate production shape, not a default
        // this branch second-guesses.
        MaybeTls::Plain(tcp)
    };

    let path = "/api/agent/connect";
    let mut request = format!("{}://{}{path}", target.ws_scheme(), target.authority())
        .into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        format!("Bearer {}", config.credential).parse()?,
    );

    let (ws, response) = tokio_tungstenite::client_async(request, stream).await?;
    tracing::info!(status = %response.status(), "connected to faber");

    let stream = WsStream::new(ws);

    let server_config = Arc::new(russh::server::Config {
        keys: vec![russh::keys::PrivateKey::from_openssh(
            &config.host_private_key,
        )?],
        ..Default::default()
    });

    let handler = crate::handler::Handler::default();
    // `run_stream` returns once key exchange completes, handing back a
    // `RunningSession` that is itself the future to wait on for the
    // connection's actual lifetime — awaiting only the outer call, as if it
    // were the whole connection, ends this function the moment kex finishes
    // and leaves the session running orphaned in the background.
    russh::server::run_stream(server_config, stream, handler)
        .await?
        .await?;
    Ok(())
}

/// Either a bare TCP stream or one wrapped in TLS, so `client_async` below
/// has one concrete type to hand off to regardless of which the target
/// needed. Boxing the TLS side keeps this enum from ballooning to the size
/// of a `rustls` client-connection struct just to sit next to a `TcpStream`.
enum MaybeTls {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl tokio::io::AsyncRead for MaybeTls {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            MaybeTls::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for MaybeTls {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            MaybeTls::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            MaybeTls::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            MaybeTls::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

fn tls_connector() -> tokio_rustls::TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(client_config))
}

struct Target {
    host: String,
    port: u16,
    tls: bool,
}

impl Target {
    fn ws_scheme(&self) -> &'static str {
        if self.tls { "wss" } else { "ws" }
    }

    fn authority(&self) -> String {
        let default_port = if self.tls { 443 } else { 80 };
        if self.port == default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn parse_api_url(api: &str) -> Result<Target, Box<dyn std::error::Error>> {
    let (scheme, rest) = api
        .split_once("://")
        .ok_or("api url must include a scheme, e.g. https://faber.example.com")?;
    let tls = match scheme {
        "https" => true,
        "http" => false,
        other => return Err(format!("unsupported api scheme '{other}'").into()),
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host.to_owned(), port.parse()?),
        None => (authority.to_owned(), if tls { 443 } else { 80 }),
    };
    Ok(Target { host, port, tls })
}

/// TCP-connects to `host:port`, through `HTTPS_PROXY`/`HTTP_PROXY` if one is
/// configured and `NO_PROXY` doesn't exclude this host.
async fn dial_tcp(host: &str, port: u16) -> Result<TcpStream, Box<dyn std::error::Error>> {
    if no_proxy_matches(host) {
        return Ok(TcpStream::connect((host, port)).await?);
    }
    let Some(proxy) = env_proxy() else {
        return Ok(TcpStream::connect((host, port)).await?);
    };
    connect_via_proxy(&proxy, host, port).await
}

fn env_var_ci(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| std::env::var(name.to_uppercase()).ok())
}

fn env_proxy() -> Option<String> {
    env_var_ci("https_proxy").or_else(|| env_var_ci("http_proxy"))
}

fn no_proxy_matches(host: &str) -> bool {
    let Some(raw) = env_var_ci("no_proxy") else {
        return false;
    };
    if raw.trim() == "*" {
        return true;
    }
    raw.split(',').map(str::trim).filter(|s| !s.is_empty()).any(|pattern| {
        let pattern = pattern.trim_start_matches('.');
        host == pattern || host.ends_with(&format!(".{pattern}"))
    })
}

/// `CONNECT`s through an HTTP(S) proxy and hands back the tunnel — the TCP
/// stream past this point speaks straight to `host:port`, same as if there
/// were no proxy at all. TLS to Faber rides inside the tunnel, same as a
/// browser's HTTPS-over-proxy.
async fn connect_via_proxy(
    proxy: &str,
    host: &str,
    port: u16,
) -> Result<TcpStream, Box<dyn std::error::Error>> {
    let without_scheme = proxy.split_once("://").map_or(proxy, |(_, rest)| rest);
    let without_auth = without_scheme.rsplit('@').next().unwrap_or(without_scheme);
    let authority = without_auth.trim_end_matches('/');
    let (proxy_host, proxy_port) = authority
        .rsplit_once(':')
        .ok_or("proxy url must include a port")?;
    let proxy_port: u16 = proxy_port.parse()?;

    let mut stream = TcpStream::connect((proxy_host, proxy_port)).await?;
    let request =
        format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;

    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
        if response.ends_with(b"\r\n\r\n") {
            break;
        }
        if response.len() > 8192 {
            return Err("proxy CONNECT response too large".into());
        }
    }

    let status_line = String::from_utf8_lossy(&response);
    let status_line = status_line.lines().next().unwrap_or("");
    if !status_line.contains(" 200") {
        return Err(format!("proxy CONNECT to {host}:{port} failed: {status_line}").into());
    }

    Ok(stream)
}
