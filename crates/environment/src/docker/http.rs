//! Just enough HTTP/1.1 to speak to a container daemon.
//!
//! Hand-rolled rather than pulled in. The Engine API needs five calls, two of
//! which are unusual in the same way: `exec/start` hijacks the connection and
//! hands back a raw bidirectional stream, and the archive endpoints move tar
//! bodies. A general client's abstractions are mostly in the way of the first
//! of those — the upgrade dance is three lines here and a typed ceremony
//! elsewhere — and its connector is exactly the piece that would have to be
//! replaced to reach a daemon through an SSH channel.
//!
//! What it does not do: no keep-alive, no redirects, no compression, no TLS. A
//! connection serves one request. Nothing here is a general-purpose client and
//! it should not grow into one.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::docker::daemon::Conn;
use crate::fault::Fault;

/// A parsed response head. The body, if any, is still on the wire.
pub struct Head {
    pub status: u16,
    headers: Vec<(String, String)>,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// One connection, one request, and whatever is left buffered after the head.
pub struct Wire {
    io: Conn,
    /// Bytes read past the end of the head. Belongs to the body, or to the
    /// hijacked stream — reading them and then dropping them is the classic
    /// way to lose the first frame of an exec.
    spare: Vec<u8>,
}

impl Wire {
    pub fn new(io: Conn) -> Self {
        Wire {
            io,
            spare: Vec::new(),
        }
    }

    /// Sends a request and reads back the response head.
    ///
    /// `upgrade` asks the daemon to hijack the connection, which it answers
    /// with `101` and then says nothing further in HTTP.
    pub async fn request(
        &mut self,
        method: &str,
        target: &str,
        body: Option<&[u8]>,
        upgrade: bool,
    ) -> Result<Head, Fault> {
        let mut head = format!("{method} {target} HTTP/1.1\r\nHost: docker\r\n");
        if upgrade {
            head.push_str("Upgrade: tcp\r\nConnection: Upgrade\r\n");
        } else {
            head.push_str("Connection: close\r\n");
        }
        match body {
            Some(body) => {
                head.push_str("Content-Type: application/json\r\n");
                head.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
            // A PUT with no body still needs a length, or the daemon waits.
            None if method != "GET" && method != "HEAD" => {
                head.push_str("Content-Length: 0\r\n");
            }
            None => {}
        }
        head.push_str("\r\n");

        self.io
            .write_all(head.as_bytes())
            .await
            .map_err(transport)?;
        if let Some(body) = body {
            self.io.write_all(body).await.map_err(transport)?;
        }
        self.io.flush().await.map_err(transport)?;

        self.read_head().await
    }

    /// Sends a request whose body is arbitrary bytes rather than JSON — the
    /// archive endpoints take a tar.
    pub async fn request_tar(&mut self, target: &str, tar: &[u8]) -> Result<Head, Fault> {
        let head = format!(
            "PUT {target} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\
             Content-Type: application/x-tar\r\nContent-Length: {}\r\n\r\n",
            tar.len()
        );
        self.io
            .write_all(head.as_bytes())
            .await
            .map_err(transport)?;
        self.io.write_all(tar).await.map_err(transport)?;
        self.io.flush().await.map_err(transport)?;
        self.read_head().await
    }

    async fn read_head(&mut self) -> Result<Head, Fault> {
        let mut buffer: Vec<u8> = Vec::with_capacity(1024);
        let end = loop {
            if let Some(at) = find_blank_line(&buffer) {
                break at;
            }
            let mut chunk = [0u8; 1024];
            let read = self.io.read(&mut chunk).await.map_err(transport)?;
            if read == 0 {
                return Err(Fault::Unreachable(
                    "the docker daemon closed the connection before answering".to_owned(),
                ));
            }
            buffer.extend_from_slice(&chunk[..read]);
        };

        self.spare = buffer[end..].to_vec();
        let text = String::from_utf8_lossy(&buffer[..end]).into_owned();
        let mut lines = text.split("\r\n");

        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| {
                Fault::Unreachable("the docker daemon sent an unreadable status line".to_owned())
            })?;

        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
            .collect();

        Ok(Head { status, headers })
    }

    /// Reads the whole body, honoring the framing the head asked for.
    pub async fn body(&mut self, head: &Head) -> Result<Vec<u8>, Fault> {
        if head
            .header("transfer-encoding")
            .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
        {
            return self.chunked().await;
        }

        match head.header("content-length").and_then(|n| n.parse().ok()) {
            Some(length) => self.exactly(length).await,
            // No framing at all means read until close, which is what
            // `Connection: close` asked for.
            None => {
                let mut body = std::mem::take(&mut self.spare);
                self.io.read_to_end(&mut body).await.map_err(transport)?;
                Ok(body)
            }
        }
    }

    async fn exactly(&mut self, length: usize) -> Result<Vec<u8>, Fault> {
        let mut body = std::mem::take(&mut self.spare);
        while body.len() < length {
            let mut chunk = vec![0u8; (length - body.len()).min(64 * 1024)];
            let read = self.io.read(&mut chunk).await.map_err(transport)?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        body.truncate(length);
        Ok(body)
    }

    async fn chunked(&mut self) -> Result<Vec<u8>, Fault> {
        let mut raw = std::mem::take(&mut self.spare);
        let mut body = Vec::new();
        let mut at = 0usize;

        loop {
            let line = loop {
                if let Some(end) = find_crlf(&raw[at..]) {
                    break at + end;
                }
                if !self.fill(&mut raw).await? {
                    return Err(Fault::Unreachable(
                        "the docker daemon truncated a chunked body".to_owned(),
                    ));
                }
            };

            let size = usize::from_str_radix(
                String::from_utf8_lossy(&raw[at..line])
                    .trim()
                    .split(';')
                    .next()
                    .unwrap_or(""),
                16,
            )
            .map_err(|_| Fault::Unreachable("unreadable chunk size".to_owned()))?;
            at = line + 2;

            if size == 0 {
                break;
            }
            while raw.len() < at + size + 2 {
                if !self.fill(&mut raw).await? {
                    return Err(Fault::Unreachable(
                        "the docker daemon truncated a chunked body".to_owned(),
                    ));
                }
            }
            body.extend_from_slice(&raw[at..at + size]);
            at += size + 2;
        }

        Ok(body)
    }

    async fn fill(&mut self, into: &mut Vec<u8>) -> Result<bool, Fault> {
        let mut chunk = [0u8; 8192];
        let read = self.io.read(&mut chunk).await.map_err(transport)?;
        into.extend_from_slice(&chunk[..read]);
        Ok(read > 0)
    }

    /// Gives up the connection after an upgrade, with anything already
    /// buffered put back in front of it.
    pub fn hijack(self) -> Conn {
        let (reader, writer) = tokio::io::split(self.io);
        let reader = std::io::Cursor::new(self.spare).chain(reader);
        Box::new(tokio::io::join(reader, writer))
    }
}

fn find_blank_line(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| at + 4)
}

fn find_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|w| w == b"\r\n")
}

fn transport(error: std::io::Error) -> Fault {
    Fault::Unreachable(error.to_string())
}
