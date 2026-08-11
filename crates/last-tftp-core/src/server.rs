//! 内置 TFTP 服务器。
//!
//! 经典多 socket 设计：监听 socket 只接收 RRQ/WRQ；每个 transfer
//! 独立 bind 新 socket 与 client 通信，避免主循环与 task 抢包。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tracing::{debug, error, info, warn};

use crate::error::TftpError;
use crate::options::{Negotiation, Options};
use crate::packet::Packet;
use crate::transfer::{recv_packet, send_packet};

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub root: PathBuf,
    pub allow_write: bool,
}

impl ServerConfig {
    pub fn new(root: impl Into<PathBuf>, allow_write: bool) -> Self {
        Self { root: root.into(), allow_write }
    }
}

pub async fn serve(bind_addr: SocketAddr, cfg: ServerConfig) -> std::io::Result<()> {
    let sock = Arc::new(UdpSocket::bind(bind_addr).await?);
    let local = sock.local_addr()?;
    info!(addr = %local, "TFTP server listening");

    let mut buf = vec![0u8; 65536];
    loop {
        let (n, peer) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                error!(error=%e, "recv_from failed");
                continue;
            }
        };
        let pkt = match Packet::parse(&buf[..n]) {
            Ok(p) => p,
            Err(e) => {
                warn!(error=%e, "drop invalid packet");
                continue;
            }
        };
        let cfg = cfg.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_one(peer, pkt, cfg).await {
                error!(peer=%peer, error=%e, "transfer failed");
            }
        });
    }
}

pub async fn handle_one(
    peer: SocketAddr,
    pkt: Packet,
    cfg: ServerConfig,
) -> Result<(), TftpError> {
    // 按 peer 地址族选 socket：macOS 拒绝 IPv6 socket 发往 IPv4 peer（EINVAL）。

    let bind_addr = if peer.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(bind_addr).await.map_err(TftpError::Io)?;
    sock.connect(peer).await.map_err(TftpError::Io)?;
    let sock = Arc::new(sock);

    match pkt {
        Packet::Rrq { filename, mode: _, options } => {
            debug!(%peer, file=%filename, "RRQ");
            let path = sanitize_path(&cfg.root, &filename)?;
            serve_read(sock, peer, &path, options).await
        }
        Packet::Wrq { filename, mode: _, options } => {
            if !cfg.allow_write {
                let err = Packet::Error {
                    code: crate::error::TftpErrorCode::AccessViolation,
                    message: "writes disabled".into(),
                };
                let _ = send_packet(&sock, &err, peer).await;
                return Ok(());
            }
            debug!(%peer, file=%filename, "WRQ");
            let path = sanitize_path(&cfg.root, &filename)?;
            serve_write(sock, peer, &path, options).await
        }
        other => Err(TftpError::Protocol(format!("expected RRQ/WRQ, got {other:?}"))),
    }
}

fn sanitize_path(root: &Path, filename: &str) -> Result<PathBuf, TftpError> {
    let cleaned = filename.replace('\\', "/");
    let p = root.join(&cleaned);
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canon_p = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
    if !canon_p.starts_with(&canon_root) {
        return Err(TftpError::Protocol("path escape denied".into()));
    }
    Ok(canon_p)
}

fn clamp_negotiation(mut n: Negotiation) -> Negotiation {
    if !(8..=crate::MAX_BLOCK_SIZE).contains(&n.blksize) {
        n.blksize = crate::DEFAULT_BLOCK_SIZE;
    }
    if n.timeout == 0 || n.timeout > 255 {
        n.timeout = crate::DEFAULT_TIMEOUT_SECS;
    }
    if n.windowsize == 0 {
        n.windowsize = crate::DEFAULT_WINDOW_SIZE;
    }
    n
}

async fn serve_read(
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
    path: &Path,
    req_opts: Options,
) -> Result<(), TftpError> {
    let neg = clamp_negotiation(Negotiation::defaults().apply_oack(&req_opts));

    let mut oack_opts = Options::new();
    oack_opts.set_blksize(neg.blksize);
    oack_opts.set_timeout(neg.timeout);
    if neg.windowsize > 1 {
        oack_opts.set_windowsize(neg.windowsize);
    }
    let oack = Packet::Oack { options: oack_opts };
    send_packet(&sock, &oack, peer).await.map_err(TftpError::Io)?;

    let mut f = File::open(path).await.map_err(|_| TftpError::Parse("cannot open file".into()))?;
    let mut block: u16 = 1;
    let mut buf = vec![0u8; neg.blksize as usize];
    loop {
        let n = match f.read(&mut buf).await {
            Ok(0) => {
                // 文件恰好是 blksize 整数倍：TFTP 要求发一个空 DATA 表示结束
                let data = Packet::Data {
                    block,
                    data: bytes::Bytes::new(),
                };
                send_packet(&sock, &data, peer).await.map_err(TftpError::Io)?;
                wait_for(&sock, peer, Duration::from_secs(u64::from(neg.timeout)), |p| {
                    matches!(p, Packet::Ack { .. } | Packet::Error { .. })
                })
                .await?;
                break;
            }
            Ok(v) => v,
            Err(e) => return Err(TftpError::Io(e)),
        };
        let data = Packet::Data {
            block,
            data: bytes::Bytes::copy_from_slice(&buf[..n]),
        };
        send_packet(&sock, &data, peer).await.map_err(TftpError::Io)?;
        debug!(%peer, block, n, "send DATA");
        if (n as u16) < neg.blksize {
            break;
        }
        block = block.wrapping_add(1);
        if block == 0 {
            return Err(TftpError::Protocol("block wrap".into()));
        }
    }
    info!(%peer, "RRQ done");
    Ok(())
}

async fn serve_write(
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
    path: &Path,
    req_opts: Options,
) -> Result<(), TftpError> {
    let neg = clamp_negotiation(Negotiation::defaults().apply_oack(&req_opts));

    let mut oack_opts = Options::new();
    oack_opts.set_blksize(neg.blksize);
    oack_opts.set_timeout(neg.timeout);
    if neg.windowsize > 1 {
        oack_opts.set_windowsize(neg.windowsize);
    }
    let has_options = !oack_opts.is_empty();
    if has_options {
        let oack = Packet::Oack { options: oack_opts };
        send_packet(&sock, &oack, peer).await.map_err(TftpError::Io)?;
    } else {
        send_packet(&sock, &Packet::Ack { block: 0 }, peer).await.map_err(TftpError::Io)?;
    }

    let mut f = File::create(path).await.map_err(|_| TftpError::Parse("cannot create file".into()))?;
    let mut expected_block: u16 = 1;
    loop {
        let pkt = wait_for(&sock, peer, Duration::from_secs(u64::from(neg.timeout)), |p| {
            matches!(p, Packet::Data { .. } | Packet::Error { .. })
        })
        .await?;
        match pkt {
            Packet::Data { block, data } => {
                if block != expected_block {
                    send_packet(&sock, &Packet::Ack { block: expected_block.wrapping_sub(1) }, peer)
                        .await
                        .map_err(TftpError::Io)?;
                    continue;
                }
                f.write_all(&data).await.map_err(TftpError::Io)?;
                let last = (data.len() as u16) < neg.blksize;
                send_packet(&sock, &Packet::Ack { block }, peer).await.map_err(TftpError::Io)?;
                if last {
                    break;
                }
                expected_block = expected_block.wrapping_add(1);
                if expected_block == 0 {
                    return Err(TftpError::Protocol("block wrap".into()));
                }
            }
            Packet::Error { code, message } => return Err(TftpError::Remote { code, message }),
            _ => unreachable!(),
        }
    }
    info!(%peer, "WRQ done");
    Ok(())
}

async fn wait_for<F>(
    sock: &UdpSocket,
    expected_peer: SocketAddr,
    timeout: Duration,
    pred: F,
) -> Result<Packet, TftpError>
where
    F: Fn(&Packet) -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        if remain.is_zero() {
            return Err(TftpError::Protocol("timeout".into()));
        }
        let r = tokio::time::timeout(remain, recv_packet(sock, remain)).await;
        match r {
            Ok(Ok((pkt, from))) => {
                if !peers_match(from, expected_peer) {
                    debug!(?from, ?expected_peer, "ignoring packet from other peer");
                    continue;
                }
                if pred(&pkt) {
                    return Ok(pkt);
                }
                debug!(?pkt, "ignoring non-matching packet");
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(TftpError::Protocol("wait_for timeout".into())),
        }
    }
}

fn peers_match(a: SocketAddr, b: SocketAddr) -> bool {
    if a == b {
        return true;
    }
    let norm = |s: SocketAddr| match s {
        SocketAddr::V4(v4) => SocketAddr::V6(
            std::net::SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0),
        ),
        other => other,
    };
    norm(a) == norm(b)
}
