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

    #[cfg(debug_assertions)]
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

    #[cfg(debug_assertions)]
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
fn base64_encode(input: &[u8]) -> String {
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
