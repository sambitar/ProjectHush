//! A minimal SOCKS5 client, enough to dial an IRC server through Tor.
//!
//! The app already runs a Tor with a loopback SOCKS port (see `crate::tor`);
//! this dials through it. The destination is always sent as a *domain name*
//! (`ATYP 0x03`) so Tor resolves it — the local resolver never sees the
//! hostname, which is the point of routing IRC through Tor at all. `.onion`
//! hosts go end-to-end with no exit node.

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The largest hostname a SOCKS5 domain request can carry (one length byte).
const MAX_DOMAIN: usize = 255;

/// Encode a SOCKS5 `CONNECT` request for a domain destination.
pub fn encode_connect(host: &str, port: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(7 + host.len());
    out.push(0x05); // version
    out.push(0x01); // CONNECT
    out.push(0x00); // reserved
    out.push(0x03); // address type: domain name
    out.push(host.len() as u8);
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(&port.to_be_bytes());
    out
}

/// Open a TCP connection to `host:port` through the SOCKS5 proxy on
/// `127.0.0.1:socks_port`.
pub async fn connect(socks_port: u16, host: &str, port: u16) -> io::Result<TcpStream> {
    if host.is_empty() || host.len() > MAX_DOMAIN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hostname is empty or too long for SOCKS5",
        ));
    }

    let mut stream = TcpStream::connect(("127.0.0.1", socks_port)).await?;

    // Greeting: SOCKS5, one method, "no authentication".
    stream.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await?;
    if greeting != [0x05, 0x00] {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "the SOCKS proxy requires authentication we do not have",
        ));
    }

    stream.write_all(&encode_connect(host, port)).await?;

    // Reply: version, status, reserved, address type, address, port.
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("Tor refused the connection (SOCKS code {})", head[1]),
        ));
    }
    match head[3] {
        0x01 => {
            let mut addr = [0u8; 4];
            stream.read_exact(&mut addr).await?;
        }
        0x04 => {
            let mut addr = [0u8; 16];
            stream.read_exact(&mut addr).await?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            stream.read_exact(&mut name).await?;
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("unknown SOCKS address type {other}"),
            ))
        }
    }
    let mut bound_port = [0u8; 2];
    stream.read_exact(&mut bound_port).await?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_domain_connect_request() {
        let req = encode_connect("irc.libera.chat", 6697);
        assert_eq!(&req[..4], &[0x05, 0x01, 0x00, 0x03]);
        assert_eq!(req[4] as usize, "irc.libera.chat".len());
        assert_eq!(&req[5..5 + "irc.libera.chat".len()], b"irc.libera.chat");
        assert_eq!(&req[req.len() - 2..], &6697u16.to_be_bytes());
    }

    #[test]
    fn encodes_an_onion_address_as_a_domain() {
        let req = encode_connect("example.onion", 6667);
        assert_eq!(req[3], 0x03);
        assert_eq!(req[4] as usize, "example.onion".len());
    }

    #[tokio::test]
    async fn rejects_an_over_long_host_before_connecting() {
        let long = "a".repeat(300);
        let err = connect(1, &long, 6697).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
