//! A minimal SOCKS5 client: enough to reach a relay through Tor.
//!
//! Hand-rolled rather than pulled in, because the whole of what Tor egress needs
//! is the CONNECT command with no authentication and a **domain-name** address.
//! Sending the name rather than a resolved address is the point: `socks5h` means
//! the proxy resolves, so no DNS lookup happens on this machine and the exit
//! cannot be correlated by a local resolver log. Keeping it here means the one
//! place this crate's bytes leave for a public network is small enough to read.

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Tor's usual SOCKS port, used when the proxy address names none.
const DEFAULT_PROXY_PORT: u16 = 9050;

/// Dial `host:port` through the SOCKS5 proxy at `proxy`.
///
/// `proxy` is `host:port`, optionally with a `socks5h://` / `socks5://` scheme.
pub async fn connect(proxy: &str, host: &str, port: u16) -> io::Result<TcpStream> {
    let (proxy_host, proxy_port) = split_authority(proxy)?;
    let mut stream = TcpStream::connect((proxy_host.as_str(), proxy_port)).await?;

    // Greeting: SOCKS5, one method offered, no authentication.
    stream.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut chosen = [0u8; 2];
    stream.read_exact(&mut chosen).await?;
    if chosen != [0x05, 0x00] {
        return Err(refused(format!(
            "the proxy did not accept no-auth SOCKS5 (reply {chosen:?})"
        )));
    }

    // CONNECT, address type 0x03: a domain name for the proxy to resolve.
    let name = host.as_bytes();
    let len = u8::try_from(name.len())
        .map_err(|_| refused(format!("`{host}` is too long for a SOCKS5 domain name")))?;
    let mut request = Vec::with_capacity(7 + name.len());
    request.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, len]);
    request.extend_from_slice(name);
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;

    // Reply: version, status, reserved, address type, bound address, bound port.
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(refused(format!("not a SOCKS5 reply (version {})", head[0])));
    }
    if head[1] != 0x00 {
        return Err(refused(format!(
            "the proxy refused the connection ({})",
            reason(head[1])
        )));
    }
    match head[3] {
        0x01 => {
            stream.read_exact(&mut [0u8; 4]).await?;
        }
        0x04 => {
            stream.read_exact(&mut [0u8; 16]).await?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut bound = vec![0u8; len[0] as usize];
            stream.read_exact(&mut bound).await?;
        }
        other => return Err(refused(format!("unknown SOCKS5 address type {other}"))),
    }
    stream.read_exact(&mut [0u8; 2]).await?; // the bound port

    Ok(stream)
}

/// The host and port of a proxy address, with or without a scheme.
fn split_authority(proxy: &str) -> io::Result<(String, u16)> {
    let trimmed = proxy.trim();
    let without_scheme = trimmed
        .strip_prefix("socks5h://")
        .or_else(|| trimmed.strip_prefix("socks5://"))
        .or_else(|| trimmed.strip_prefix("socks://"))
        .unwrap_or(trimmed)
        .trim_end_matches('/');
    if without_scheme.is_empty() {
        return Err(refused("the proxy address is empty"));
    }
    match without_scheme.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => Ok((
            host.to_string(),
            port.parse()
                .map_err(|_| refused(format!("`{proxy}` has no usable port")))?,
        )),
        Some(_) => Err(refused(format!("`{proxy}` has no host"))),
        None => Ok((without_scheme.to_string(), DEFAULT_PROXY_PORT)),
    }
}

/// The RFC 1928 reply codes this client can meet.
fn reason(code: u8) -> &'static str {
    match code {
        0x01 => "general failure",
        0x02 => "connection not allowed by the proxy",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "unknown reason",
    }
}

fn refused(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A proxy that completes the handshake, checking that the target arrived as
    /// a domain name rather than a resolved address.
    #[tokio::test]
    async fn the_handshake_asks_the_proxy_to_resolve_the_name() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [0x05, 0x01, 0x00]);
            stream.write_all(&[0x05, 0x00]).await.unwrap();

            let mut head = [0u8; 5];
            stream.read_exact(&mut head).await.unwrap();
            assert_eq!(
                &head[..4],
                &[0x05, 0x01, 0x00, 0x03],
                "CONNECT with a domain-name address"
            );
            let mut host = vec![0u8; head[4] as usize];
            stream.read_exact(&mut host).await.unwrap();
            assert_eq!(host, b"relay.example");
            let mut port = [0u8; 2];
            stream.read_exact(&mut port).await.unwrap();
            assert_eq!(u16::from_be_bytes(port), 443);

            stream
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            stream
        });

        let stream = connect(&format!("socks5h://{addr}"), "relay.example", 443)
            .await
            .expect("the handshake completes");
        drop(stream);
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_connect_is_an_error_not_a_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            stream.write_all(&[0x05, 0x00]).await.unwrap();
            let mut head = [0u8; 5];
            stream.read_exact(&mut head).await.unwrap();
            let mut host = vec![0u8; head[4] as usize];
            stream.read_exact(&mut host).await.unwrap();
            let mut port = [0u8; 2];
            stream.read_exact(&mut port).await.unwrap();
            stream.write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap();
        });

        let error = connect(&format!("{addr}"), "relay.example", 443)
            .await
            .expect_err("a refused connect does not hand back a socket");
        assert!(
            error.to_string().contains("connection refused"),
            "{error}"
        );
        proxy.await.unwrap();
    }

    #[test]
    fn a_proxy_address_is_read_with_or_without_a_scheme() {
        assert_eq!(
            split_authority("socks5h://127.0.0.1:9050").unwrap(),
            ("127.0.0.1".to_string(), 9050)
        );
        assert_eq!(
            split_authority("127.0.0.1:9150").unwrap(),
            ("127.0.0.1".to_string(), 9150)
        );
        assert_eq!(
            split_authority("127.0.0.1").unwrap(),
            ("127.0.0.1".to_string(), DEFAULT_PROXY_PORT)
        );
        assert!(split_authority("").is_err());
        assert!(split_authority(":9050").is_err());
    }
}
