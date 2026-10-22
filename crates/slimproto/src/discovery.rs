//! Finding Lyrion servers on the LAN (`docs/slimproto.md` §1): a UDP broadcast of `e` plus the TLV
//! tags we want, answered by `E` plus TLVs. The server's address is the reply's source; slimproto
//! is always on port 3483.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::net::UdpSocket;

use crate::messages::PORT;

/// A server that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// Where to connect for slimproto (the reply's source, port 3483).
    pub addr: SocketAddrV4,
    /// `NAME`, e.g. "lyrion".
    pub name: String,
    /// `JSON`: the HTTP / JSON-RPC port, usually 9000.
    pub http_port: Option<u16>,
    /// `VERS`, e.g. "9.0.2".
    pub version: String,
}

/// The request: `e` and the tags we want, each with length 0.
const REQUEST: &[u8] = b"eNAME\0JSON\0VERS\0";

/// Broadcasts a request and collects every answer that arrives within `wait`.
pub async fn discover(wait: Duration) -> std::io::Result<Vec<Server>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_broadcast(true)?;
    socket.send_to(REQUEST, (Ipv4Addr::BROADCAST, PORT)).await?;
    let mut servers: Vec<Server> = Vec::new();
    let mut buf = [0u8; 1500];
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(received) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
        let (len, from) = received?;
        let SocketAddr::V4(from) = from else { continue };
        if let Some(server) = parse_reply(&buf[..len], *from.ip())
            && !servers.iter().any(|s| s.addr == server.addr)
        {
            servers.push(server);
        }
    }
    Ok(servers)
}

/// Parses an `E` reply; `None` for anything else (our own broadcast echoes back as `e`).
#[must_use]
pub fn parse_reply(datagram: &[u8], from: Ipv4Addr) -> Option<Server> {
    let mut rest = datagram.strip_prefix(b"E")?;
    let mut server = Server {
        addr: SocketAddrV4::new(from, PORT),
        name: String::new(),
        http_port: None,
        version: String::new(),
    };
    while rest.len() >= 5 {
        let (tag, len) = (&rest[..4], usize::from(rest[4]));
        let value = rest.get(5..5 + len)?;
        let text = String::from_utf8_lossy(value).into_owned();
        match tag {
            b"NAME" => server.name = text,
            b"JSON" => server.http_port = text.trim().parse().ok(),
            b"VERS" => server.version = text,
            _ => {}
        }
        rest = &rest[5 + len..];
    }
    Some(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tlv_replies() {
        let reply = b"ENAME\x06lyrionJSON\x049000VERS\x059.0.2UUID\x03abc";
        let server = parse_reply(reply, Ipv4Addr::new(192, 168, 1, 2)).unwrap();
        assert_eq!(
            server.addr,
            SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 2), 3483)
        );
        assert_eq!(server.name, "lyrion");
        assert_eq!(server.http_port, Some(9000));
        assert_eq!(server.version, "9.0.2");
    }

    #[test]
    fn ignores_requests_and_truncated_replies() {
        assert_eq!(
            parse_reply(REQUEST, Ipv4Addr::LOCALHOST),
            None,
            "our own broadcast"
        );
        assert_eq!(parse_reply(b"ENAME\x09lyr", Ipv4Addr::LOCALHOST), None);
        let bare = parse_reply(b"E", Ipv4Addr::LOCALHOST).unwrap();
        assert_eq!(
            bare.http_port, None,
            "an old server that sends no TLVs is still a server"
        );
    }
}
