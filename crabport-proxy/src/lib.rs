//! Proxy tunnel establishment — shared by SSH, Telnet, and any future
//! backend that needs to reach a `host:port` through a proxy.
//!
//! Returns a boxed stream (`AsyncRead + AsyncWrite + Unpin + Send`) that's
//! ready to be fed into whatever protocol runs on top (russh, telnet, …).
//!
//! Supported proxy protocols:
//!
//! - **SOCKS5** (RFC 1928 + RFC 1929 user/pass auth) — via `tokio-socks`.
//! - **HTTP CONNECT** — hand-rolled; the CONNECT request is trivial and
//!   avoids pulling in an HTTP client crate.
//! - **HTTPS CONNECT** — same as HTTP but the proxy stream is wrapped in
//!   TLS via `tokio-rustls`.
//!
//! When no proxy is configured (`ProxyKind::None` or empty host), this
//! falls back to a direct `TcpStream::connect`.

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crabport_core::credential::{ProxyConfig, ProxyKind};

/// A stream that's usable as a transport (russh, telnet, …).
///
/// We can't write `Box<dyn AsyncRead + AsyncWrite + Unpin + Send>` directly
/// because `AsyncRead` and `AsyncWrite` are non-auto traits — only one
/// non-auto trait is allowed in a trait object. So we declare a single
/// combining trait and box that instead.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + ?Sized> Stream for T {}

/// The boxed stream type returned by [`connect`].
pub type BoxStream = Box<dyn Stream>;

/// Establish a stream to `target_host:target_port`, either directly or
/// through the configured proxy.
///
/// - `proxy = None` or `is_enabled() == false` → direct TCP connect.
/// - `proxy = Some(Socks5)` → SOCKS5 tunnel.
/// - `proxy = Some(Http)` → HTTP CONNECT tunnel.
/// - `proxy = Some(Https)` → HTTPS CONNECT tunnel (TLS-wrapped).
pub async fn connect(
    proxy: &Option<ProxyConfig>,
    target_host: &str,
    target_port: u16,
) -> std::io::Result<BoxStream> {
    match proxy {
        Some(p) if p.is_enabled() => connect_via_proxy(p, target_host, target_port).await,
        _ => {
            let addr = format!("{target_host}:{target_port}");
            TcpStream::connect(&addr)
                .await
                .map(|s| Box::new(s) as BoxStream)
        }
    }
}

async fn connect_via_proxy(
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> std::io::Result<BoxStream> {
    match proxy.kind {
        ProxyKind::Socks5 => connect_socks5(proxy, target_host, target_port).await,
        ProxyKind::Http | ProxyKind::Https => {
            connect_http_connect(proxy, target_host, target_port).await
        }
        ProxyKind::None => {
            // `is_enabled()` already filtered this, but keep the arm for
            // exhaustiveness.
            let addr = format!("{target_host}:{target_port}");
            TcpStream::connect(&addr)
                .await
                .map(|s| Box::new(s) as BoxStream)
        }
    }
}

// ---------------------------------------------------------------------------
// SOCKS5
// ---------------------------------------------------------------------------

async fn connect_socks5(
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> std::io::Result<BoxStream> {
    use tokio_socks::tcp::Socks5Stream;

    let proxy_addr = format!("{}:{}", proxy.host, proxy.port);
    let target = format!("{target_host}:{target_port}");

    tracing::info!("SOCKS5: connecting via {proxy_addr} to {target}");

    let stream = match (proxy.username.as_deref(), proxy.password.as_deref()) {
        // Both username + password present → authenticate.
        (Some(user), Some(pass)) if !user.is_empty() && !pass.is_empty() => {
            Socks5Stream::connect_with_password(proxy_addr.as_str(), target.as_str(), user, pass)
                .await
                .map_err(io_err)?
        }
        // Otherwise → no auth (anonymous SOCKS5).
        _ => Socks5Stream::connect(proxy_addr.as_str(), target.as_str())
            .await
            .map_err(io_err)?,
    };

    Ok(Box::new(stream))
}

// ---------------------------------------------------------------------------
// HTTP / HTTPS CONNECT
// ---------------------------------------------------------------------------

/// HTTP CONNECT tunnel: send `CONNECT host:port HTTP/1.1` to the proxy,
/// wait for `HTTP/1.1 200`, then the stream is a raw tunnel to the target.
async fn connect_http_connect(
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
) -> std::io::Result<BoxStream> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let proxy_addr = format!("{}:{}", proxy.host, proxy.port);
    let target = format!("{target_host}:{target_port}");

    tracing::info!("{:?}: connecting via {proxy_addr} to {target}", proxy.kind);

    // Open TCP to the proxy server.
    let tcp = TcpStream::connect(&proxy_addr).await?;

    // For HTTPS proxies, wrap in TLS before sending the CONNECT request.
    // The TLS handshake is with the *proxy* server (not the SSH target).
    let mut stream: BoxStream = if proxy.kind == ProxyKind::Https {
        connect_tls_over(tcp, &proxy.host).await?
    } else {
        Box::new(tcp)
    };

    // Proxy-Authorization: Basic header (only when both credentials are set).
    let auth_header = match (proxy.username.as_deref(), proxy.password.as_deref()) {
        (Some(user), Some(pass)) if !user.is_empty() && !pass.is_empty() => {
            let credentials = format!("{user}:{pass}");
            let encoded = base64_encode(credentials.as_bytes());
            format!("\r\nProxy-Authorization: Basic {encoded}")
        }
        _ => String::new(),
    };

    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth_header}\r\n\r\n",);

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    // Read the response. We only need the status line — a 200 means the
    // tunnel is up and the rest of the stream is ours.
    let mut buf = [0u8; 1024];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let response = String::from_utf8_lossy(&buf[..n]);

    // First line looks like: `HTTP/1.1 200 Connection established\r\n...`
    let first_line = response.lines().next().unwrap_or("");
    if !first_line.contains(" 200 ") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!("proxy CONNECT failed: {first_line}"),
        ));
    }

    // If the 200 response included some tunnel bytes in the same read
    // buffer, wrap the stream so those bytes are yielded first.
    if let Some(idx) = response.find("\r\n\r\n") {
        let consumed = idx + 4;
        if consumed < n {
            let leftover = buf[consumed..n].to_vec();
            return Ok(Box::new(PrefixedRead::new(stream, leftover)));
        }
    }

    Ok(stream)
}

// ---------------------------------------------------------------------------
// TLS for HTTPS proxies
// ---------------------------------------------------------------------------

/// Wrap a TCP stream in TLS using rustls. This is for the proxy connection
/// only — the protocol running on top (SSH, telnet) does its own crypto.
async fn connect_tls_over(tcp: TcpStream, server_name: &str) -> std::io::Result<BoxStream> {
    use rustls_pki_types::ServerName;
    use rustls_platform_verifier::BuilderVerifierExt;
    use std::sync::Arc;
    use tokio_rustls::{TlsConnector, rustls};

    // Use the OS-native trust store / verification logic instead of
    // shipping our own webpki root list — this matches what browsers do.
    let config = rustls::ClientConfig::builder()
        .with_platform_verifier()
        .with_no_client_auth();
    let config = Arc::new(config);
    let connector = TlsConnector::from(config);

    let server_name = ServerName::try_from(server_name.to_string())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    connector
        .connect(server_name, tcp)
        .await
        .map(|s| Box::new(s) as BoxStream)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}

// ---------------------------------------------------------------------------
// PrefixedRead — prepend already-read bytes to a stream
// ---------------------------------------------------------------------------

/// Wraps a stream, yielding `prefix` bytes first, then delegating to the
/// inner stream. Used when the HTTP CONNECT proxy response included some
/// tunnel data in the same read buffer.
struct PrefixedRead<S> {
    prefix: Vec<u8>,
    pos: usize,
    inner: S,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> PrefixedRead<S> {
    fn new(inner: S, prefix: Vec<u8>) -> Self {
        Self {
            prefix,
            pos: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin + Send> AsyncRead for PrefixedRead<S> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();

        // Serve from prefix first.
        if this.pos < this.prefix.len() {
            let remaining = &this.prefix[this.pos..];
            let space = buf.remaining();
            let n = remaining.len().min(space);
            buf.put_slice(&remaining[..n]);
            this.pos += n;
            return std::task::Poll::Ready(Ok(()));
        }

        // Prefix exhausted — delegate to inner.
        std::pin::Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin + Send> AsyncWrite for PrefixedRead<S> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

/// Minimal base64 encoder (avoids pulling in the `base64` crate for one
/// tiny use). RFC 4648 standard alphabet with padding.
pub(crate) fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[((b0 & 0x03) << 4 | b1 >> 4) as usize] as char);

        if chunk.len() > 1 {
            out.push(ALPHABET[((b1 & 0x0f) << 2 | b2 >> 6) as usize] as char);
        } else {
            out.push('=');
        }

        if chunk.len() > 2 {
            out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests — pure helpers + loopback fake-proxy handshakes (no real network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crabport_core::credential::{ProxyConfig, ProxyKind};

    // ---- base64 ----

    /// RFC 4648 §10 test vectors.
    #[test]
    fn base64_rfc4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), expected, "input {input:?}");
        }
    }

    // ---- PrefixedRead ----

    /// The prefix is yielded before any inner-stream data, and writes pass
    /// straight through to the inner stream.
    #[tokio::test]
    async fn prefixed_read_serves_prefix_then_inner() {
        let (client, mut server) = tokio::io::duplex(64);
        let mut wrapped = PrefixedRead::new(client, b"EARLY".to_vec());

        server.write_all(b"-LATE").await.unwrap();

        let mut out = Vec::new();
        let mut buf = [0u8; 16];
        while out.len() < 10 {
            let n = wrapped.read(&mut buf).await.unwrap();
            assert!(n > 0, "unexpected EOF after {:?}", out);
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(out, b"EARLY-LATE");

        // Writes bypass the prefix and reach the peer.
        wrapped.write_all(b"up").await.unwrap();
        let mut got = [0u8; 2];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"up");
    }

    /// A read buffer smaller than the prefix drains it across multiple reads.
    #[tokio::test]
    async fn prefixed_read_partial_reads() {
        let (client, _server) = tokio::io::duplex(64);
        let mut wrapped = PrefixedRead::new(client, b"abcdef".to_vec());
        let mut buf = [0u8; 4];
        let n = wrapped.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"abcd");
        let n = wrapped.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ef");
    }

    // ---- direct connect ----

    /// `proxy = None` falls back to a plain TCP connection.
    #[tokio::test]
    async fn direct_connect_when_no_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            sock.write_all(b"hello").await.unwrap();
        });

        let mut stream = connect(&None, "127.0.0.1", port).await.unwrap();
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    }

    /// A configured-but-disabled proxy (kind None / empty host) also goes
    /// direct instead of failing.
    #[tokio::test]
    async fn disabled_proxy_goes_direct() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            sock.write_all(b"ok").await.unwrap();
        });

        let disabled = ProxyConfig {
            kind: ProxyKind::Socks5,
            host: String::new(), // empty host → is_enabled() == false
            port: 1,
            username: None,
            password: None,
        };
        let mut stream = connect(&Some(disabled), "127.0.0.1", port).await.unwrap();
        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ok");
    }

    // ---- HTTP CONNECT ----

    /// Read from `sock` until the header terminator, returning the request.
    async fn read_http_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut req = Vec::new();
        let mut buf = [0u8; 512];
        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut buf).await.unwrap();
            assert!(n > 0, "client closed before finishing request");
            req.extend_from_slice(&buf[..n]);
        }
        String::from_utf8(req).unwrap()
    }

    /// Happy path: CONNECT line + Basic auth header are sent, a 200 reply
    /// opens the tunnel, and bytes that arrived in the same read as the
    /// response are not lost.
    #[tokio::test]
    async fn http_connect_sends_auth_and_keeps_leftover_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let req = read_http_request(&mut sock).await;
            // Tunnel bytes ride along in the same segment as the response.
            sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\nEARLY")
                .await
                .unwrap();
            sock.write_all(b"-DATA").await.unwrap();
            req
        });

        let proxy = ProxyConfig {
            kind: ProxyKind::Http,
            host: "127.0.0.1".into(),
            port,
            username: Some("u".into()),
            password: Some("p".into()),
        };
        let mut stream = connect(&Some(proxy), "target.example", 22).await.unwrap();

        let mut out = Vec::new();
        let mut buf = [0u8; 32];
        while out.len() < 10 {
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "unexpected EOF after {:?}", out);
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(out, b"EARLY-DATA");

        let req = server.await.unwrap();
        assert!(
            req.starts_with("CONNECT target.example:22 HTTP/1.1\r\n"),
            "bad request line: {req}"
        );
        // base64("u:p") == "dTpw"
        assert!(
            req.contains("Proxy-Authorization: Basic dTpw"),
            "missing auth header: {req}"
        );
    }

    /// A non-200 response fails the connect with an error naming the status
    /// line instead of handing back a dead stream.
    #[tokio::test]
    async fn http_connect_non_200_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = read_http_request(&mut sock).await;
            sock.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });

        let proxy = ProxyConfig {
            kind: ProxyKind::Http,
            host: "127.0.0.1".into(),
            port,
            username: None,
            password: None,
        };
        let err = match connect(&Some(proxy), "target.example", 22).await {
            Ok(_) => panic!("407 must fail"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("407"), "error was: {err}");
    }

    // ---- SOCKS5 ----

    /// Minimal RFC 1928 server: no-auth greeting, accept any CONNECT,
    /// then push `payload` through the tunnel. Returns the target the
    /// client asked for.
    async fn fake_socks5(listener: TcpListener, payload: &'static [u8]) -> (String, u16) {
        let (mut sock, _) = listener.accept().await.unwrap();

        // Greeting: VER NMETHODS METHODS...
        let mut head = [0u8; 2];
        sock.read_exact(&mut head).await.unwrap();
        assert_eq!(head[0], 0x05, "not SOCKS5");
        let mut methods = vec![0u8; head[1] as usize];
        sock.read_exact(&mut methods).await.unwrap();
        // Select "no authentication required".
        sock.write_all(&[0x05, 0x00]).await.unwrap();

        // Request: VER CMD RSV ATYP ...
        let mut req = [0u8; 4];
        sock.read_exact(&mut req).await.unwrap();
        assert_eq!(req[1], 0x01, "expected CONNECT");
        let target = match req[3] {
            0x01 => {
                let mut ip = [0u8; 4];
                sock.read_exact(&mut ip).await.unwrap();
                format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
            }
            0x03 => {
                let mut len = [0u8; 1];
                sock.read_exact(&mut len).await.unwrap();
                let mut name = vec![0u8; len[0] as usize];
                sock.read_exact(&mut name).await.unwrap();
                String::from_utf8(name).unwrap()
            }
            other => panic!("unexpected ATYP {other}"),
        };
        let mut port = [0u8; 2];
        sock.read_exact(&mut port).await.unwrap();

        // Success reply, bound to 0.0.0.0:0.
        sock.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        sock.write_all(payload).await.unwrap();

        (target, u16::from_be_bytes(port))
    }

    /// Anonymous SOCKS5 handshake reaches the requested target and the
    /// returned stream carries tunnel data.
    #[tokio::test]
    async fn socks5_no_auth_tunnel() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(fake_socks5(listener, b"SOCKS-OK"));

        let proxy = ProxyConfig {
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".into(),
            port,
            username: None,
            password: None,
        };
        let mut stream = connect(&Some(proxy), "target.example", 2222).await.unwrap();
        let mut buf = [0u8; 8];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"SOCKS-OK");

        let (target, target_port) = server.await.unwrap();
        assert_eq!(target, "target.example");
        assert_eq!(target_port, 2222);
    }
}
