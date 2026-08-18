//! Transport-agnostic network proxying.
//!
//! This crate deliberately does not know HTTP, user identity, bindings, or
//! execution environments. A caller authorizes a request and resolves its
//! destination, then gives this crate an already-accepted client stream and a
//! dialer. Keeping that boundary narrow makes the same relay work for local
//! TCP, SSH `direct-tcpip`, and any future transport that yields an async
//! stream.
//!
//! A proxy connection is streamed in both directions. In particular, nothing
//! is buffered as a complete request or response, so long-lived protocols
//! such as server-sent events and upgraded WebSocket connections retain their
//! normal streaming behaviour.

use std::io;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

/// An asynchronous bidirectional byte stream suitable for relaying.
///
/// This is intentionally protocol-neutral. An HTTP layer may hand it an
/// upgraded connection, while an SSH transport may hand it a `direct-tcpip`
/// channel.
pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T> Stream for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

/// The number of bytes that crossed each direction of a completed relay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Relayed {
    /// Bytes read from the client and written to the destination.
    pub client_to_destination: u64,
    /// Bytes read from the destination and written to the client.
    pub destination_to_client: u64,
}

/// Opens the destination side of a proxy connection.
///
/// Implementations are where a transport belongs: `TcpDialer` opens a local
/// TCP connection; an SSH-backed implementation can open a `direct-tcpip`
/// channel. The caller, not a dialer, is responsible for deciding whether an
/// address is permitted.
#[async_trait]
pub trait Dial: Send + Sync {
    /// The stream returned for a successfully opened destination.
    type Connection: Stream + 'static;

    /// Open `address` on behalf of the caller.
    async fn dial(&self, address: &str) -> Result<Self::Connection, Error>;
}

/// The standard TCP dialer.
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpDialer;

#[async_trait]
impl Dial for TcpDialer {
    type Connection = TcpStream;

    async fn dial(&self, address: &str) -> Result<Self::Connection, Error> {
        TcpStream::connect(address)
            .await
            .map_err(|source| Error::Dial {
                address: address.to_owned(),
                source,
            })
    }
}

/// Failures while opening or relaying a proxy connection.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The selected destination could not be opened.
    #[error("could not dial `{address}`: {source}")]
    Dial {
        address: String,
        #[source]
        source: io::Error,
    },

    /// A connected peer failed while bytes were being relayed.
    #[error("proxy relay failed: {0}")]
    Relay(#[source] io::Error),
}

/// Relay bytes until both sides have cleanly finished, reporting both byte
/// counts. Half-closes are preserved by [`tokio::io::copy_bidirectional`].
pub async fn relay<Client, Destination>(
    client: &mut Client,
    destination: &mut Destination,
) -> Result<Relayed, Error>
where
    Client: Stream,
    Destination: Stream,
{
    let (client_to_destination, destination_to_client) =
        tokio::io::copy_bidirectional(client, destination)
            .await
            .map_err(Error::Relay)?;
    Ok(Relayed {
        client_to_destination,
        destination_to_client,
    })
}

/// Open a destination with `dialer` and relay a client connection to it.
///
/// This is the usual one-call proxy path. It intentionally accepts the
/// destination as an argument rather than discovering one from ambient
/// configuration, because selecting a destination is an authorization
/// decision owned by the caller.
pub async fn proxy<Client, D>(
    client: &mut Client,
    dialer: &D,
    address: &str,
) -> Result<Relayed, Error>
where
    Client: Stream,
    D: Dial,
{
    let mut destination = dialer.dial(address).await?;
    relay(client, &mut destination).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::Mutex,
        time::timeout,
    };

    use super::*;

    #[tokio::test]
    async fn proxies_bytes_in_both_directions() {
        let (mut destination_peer, destination_proxy) = tokio::io::duplex(64);
        let destination = tokio::spawn(async move {
            let mut request = [0; 5];
            destination_peer.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"hello");
            destination_peer.write_all(b"world").await.unwrap();
            destination_peer.shutdown().await.unwrap();
        });

        let (mut client, mut proxy_client) = tokio::io::duplex(64);
        let dialer = OneConnection::new(destination_proxy);
        let relay = tokio::spawn(async move {
            proxy(&mut proxy_client, &dialer, "authorised.example:8080")
                .await
                .unwrap()
        });

        client.write_all(b"hello").await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        timeout(Duration::from_secs(1), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response, b"world");

        let relayed = relay.await.unwrap();
        assert_eq!(relayed.client_to_destination, 5);
        assert_eq!(relayed.destination_to_client, 5);
        destination.await.unwrap();
    }

    /// A test-only transport that proves `proxy` accepts streams from a
    /// caller-owned dialer without requiring a real TCP listener.
    struct OneConnection {
        connection: Mutex<Option<tokio::io::DuplexStream>>,
    }

    impl OneConnection {
        fn new(connection: tokio::io::DuplexStream) -> Self {
            Self {
                connection: Mutex::new(Some(connection)),
            }
        }
    }

    #[async_trait]
    impl Dial for OneConnection {
        type Connection = tokio::io::DuplexStream;

        async fn dial(&self, _address: &str) -> Result<Self::Connection, Error> {
            self.connection
                .lock()
                .await
                .take()
                .ok_or_else(|| Error::Dial {
                    address: "already connected".to_owned(),
                    source: io::Error::other("test dialer has one connection"),
                })
        }
    }

    #[tokio::test]
    async fn dial_errors_keep_the_destination_in_the_error() {
        let error = TcpDialer.dial("127.0.0.1:0").await.unwrap_err();
        assert!(matches!(error, Error::Dial { address, .. } if address == "127.0.0.1:0"));
    }
}
