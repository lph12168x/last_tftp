//! 异步 UDP 收发原语：tokio UdpSocket 上的 TFTP 报文收发，含超时重传。
//!
//! 这一层只负责"在规定超时内等到想要的报文"。语义层面的 client/server
//! 状态机放在 `client` / `server` 模块里。

use std::future::Future;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::sleep;
use crate::error::TftpError;
use crate::packet::Packet;
/// 通用发送：返回字节数与对端地址。
pub async fn send_packet(
    sock: &UdpSocket,
    packet: &Packet,
    dest: std::net::SocketAddr,
) -> std::io::Result<usize> {
    let bytes = packet.encode();
    sock.send_to(&bytes, dest).await
}

/// 接收单个报文（在 `timeout` 内）。
pub async fn recv_packet(sock: &UdpSocket, timeout: Duration) -> Result<(Packet, std::net::SocketAddr), TftpError> {
    let mut buf = vec![0u8; 65536];
    let recv = tokio::time::timeout(timeout, sock.recv_from(&mut buf)).await;
    match recv {
        Ok(Ok((n, from))) => {
            let pkt = Packet::parse(&buf[..n]).map_err(TftpError::from)?;
            Ok((pkt, from))
        }
        Ok(Err(e)) => Err(TftpError::Io(e)),
        Err(_) => Err(TftpError::Protocol("recv timeout".into())),
    }
}

/// 指数退避重试：按 `initial` 起步，乘 2，封顶 `max`，最多 `max_retries` 次。
pub async fn with_retry<T, F, Fut>(
    initial: Duration,
    max: Duration,
    max_retries: u32,
    mut op: F,
) -> Result<T, TftpError>
where
    F: FnMut(Duration) -> Fut,
    Fut: Future<Output = Result<T, TftpError>>,
{
    let mut delay = initial;
    let mut last_err: Option<TftpError> = None;
    let mut attempt = 0;
    loop {
        if attempt >= max_retries {
            break;
        }
        match op(delay).await {
            Ok(v) => return Ok(v),
            Err(e @ TftpError::Timeout { .. }) | Err(e @ TftpError::Protocol(_)) => {
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
        if attempt + 1 < max_retries {
            sleep(delay).await;
            delay = (delay * 2).min(max);
        }
        attempt += 1;
    }
    Err(last_err.unwrap_or(TftpError::Timeout { retries: max_retries }))
}

/// 解析对端 `HOST:FILE` 形式的字符串。HOST 可以是 IPv4/IPv6/主机名。
pub fn parse_target(target: &str) -> Result<(String, String), TftpError> {
    // IPv6 字面量形如 `[::1]:path`
    if let Some(rest) = target.strip_prefix('[') {
        let end = rest.find(']').ok_or_else(|| TftpError::Protocol("invalid IPv6 host".into()))?;
        let host = &rest[..end];
        if host.is_empty() {
            return Err(TftpError::Protocol("empty IPv6 host".into()));
        }
        let after = &rest[end + 1..];
        let path = after.strip_prefix(':').ok_or_else(|| TftpError::Protocol("missing file path".into()))?;
        return Ok((host.to_string(), path.to_string()));
    }
    let (host, path) = target
        .split_once(':')
        .ok_or_else(|| TftpError::Protocol("target must be HOST:FILE".into()))?;
    if host.is_empty() {
        return Err(TftpError::Protocol("empty host".into()));
    }
    Ok((host.to_string(), path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_ipv4() {
        let (h, p) = parse_target("192.168.1.1:kernel.bin").unwrap();
        assert_eq!(h, "192.168.1.1");
        assert_eq!(p, "kernel.bin");
    }

    #[test]
    fn parse_target_ipv6() {
        let (h, p) = parse_target("[fe80::1]:fw.img").unwrap();
        assert_eq!(h, "fe80::1");
        assert_eq!(p, "fw.img");
    }

    #[test]
    fn parse_target_hostname() {
        let (h, p) = parse_target("router.lan:config").unwrap();
        assert_eq!(h, "router.lan");
        assert_eq!(p, "config");
    }

    #[test]
    fn parse_target_invalid() {
        assert!(parse_target("nocolon").is_err());
        assert!(parse_target("[fe80::1]fw.img").is_err());
        assert!(parse_target("[]:path").is_err());
    }
}