//! 内置 TFTP 服务器。

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
use crate::session::TransferStats;
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
            if let Err(e) = handle_one(peer, pkt, cfg, None).await {
                error!(peer=%peer, error=%e, "transfer failed");
            }
        });
    }
}

pub async fn handle_one(
    peer: SocketAddr,
    pkt: Packet,
    cfg: ServerConfig,
    progress_tx: Option<std::sync::mpsc::Sender<u64>>,
) -> Result<TransferStats, TftpError> {
    let bind_addr = if peer.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(bind_addr).await.map_err(TftpError::Io)?;
    sock.connect(peer).await.map_err(TftpError::Io)?;
    let sock = Arc::new(sock);
    match pkt {
        Packet::Rrq { filename, mode: _, options } => {
            debug!(%peer, file=%filename, "RRQ");
            let path = sanitize_path(&cfg.root, &filename)?;
            serve_read(sock, peer, &path, options, progress_tx).await
        }
        Packet::Wrq { filename, mode: _, options } => {
            if !cfg.allow_write {
                let err = Packet::Error {
                    code: crate::error::TftpErrorCode::AccessViolation,
                    message: "writes disabled".into(),
                };
                let _ = send_packet(&sock, &err, peer).await;
                return Ok(TransferStats::default());
            }
            debug!(%peer, file=%filename, "WRQ");
            let path = sanitize_path(&cfg.root, &filename)?;
            serve_write(sock, peer, &path, options, progress_tx).await
        }
        other => Err(TftpError::Protocol(format!("expected RRQ/WRQ, got {other:?}"))),
    }
}

fn sanitize_path(root: &Path, filename: &str) -> Result<PathBuf, TftpError> {
    let cleaned = filename.replace('\\', "/");
    if cleaned.starts_with('/') || cleaned.contains(':') {
        return Err(TftpError::Protocol("path escape denied".into()));
    }
    let p = root.join(&cleaned);

    fn strip_verbatim(p: &Path) -> std::path::PathBuf {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            if let Some(unced) = rest.strip_prefix("UNC\\") {
                std::path::PathBuf::from(format!("\\\\{unced}"))
            } else {
                std::path::PathBuf::from(rest)
            }
        } else {
            p.to_path_buf()
        }
    }

    let canon_p = std::fs::canonicalize(&p);
    let resolved = match canon_p {
        Ok(cp) => cp,
        Err(_) => {
            let mut acc = std::path::PathBuf::new();
            for comp in p.components() {
                if let std::path::Component::ParentDir = comp {
                    acc.pop();
                } else {
                    acc.push(comp);
                }
            }
            let parent = acc.parent().unwrap_or(&acc);
            let canon_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
            canon_parent.join(acc.file_name().unwrap_or_default())
        }
    };
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let root_norm = strip_verbatim(&canon_root);
    let p_norm = strip_verbatim(&resolved);
    if !p_norm.starts_with(&root_norm) {
        return Err(TftpError::Protocol("path escape denied".into()));
    }
    Ok(if resolved.exists() { resolved } else { p })
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
    progress_tx: Option<std::sync::mpsc::Sender<u64>>,
) -> Result<TransferStats, TftpError> {
    let neg = clamp_negotiation(Negotiation::defaults().apply_oack(&req_opts));

    let mut oack_opts = Options::new();
    oack_opts.set_blksize(neg.blksize);
    oack_opts.set_timeout(neg.timeout);
    if neg.windowsize > 1 {
        oack_opts.set_windowsize(neg.windowsize);
    }
    let oack = Packet::Oack { options: oack_opts };
    send_packet(&sock, &oack, peer).await.map_err(TftpError::Io)?;

    let mut f = match File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            let code = if e.kind() == std::io::ErrorKind::NotFound {
                crate::error::TftpErrorCode::FileNotFound
            } else {
                crate::error::TftpErrorCode::AccessViolation
            };
            let _ = send_packet(
                &sock,
                &Packet::Error {
                    code,
                    message: format!("cannot open file: {e}"),
                },
                peer,
            )
            .await;
            return Ok(TransferStats::default());
        }
    };
    let mut block: u16 = 1;
    let mut buf = vec![0u8; neg.blksize as usize];
    let file_len = f.metadata().await.map_err(TftpError::Io)?.len();
    let mut bytes_sent: u64 = 0;
    if file_len == 0 {
        let data = Packet::Data {
            block,
            data: bytes::Bytes::new(),
        };
        send_packet(&sock, &data, peer).await.map_err(TftpError::Io)?;
        let _ = wait_for(
            &sock,
            peer,
            Duration::from_secs(u64::from(neg.timeout)),
            |p| matches!(p, Packet::Ack { .. } | Packet::Error { .. }),
        )
        .await;
        return Ok(TransferStats::default());
    }
    let mut emitted_empty_eof = false;
    loop {
        let n = match f.read(&mut buf).await {
            Ok(0) => {
                if emitted_empty_eof {
                    break;
                }
                emitted_empty_eof = true;
                let data = Packet::Data {
                    block,
                    data: bytes::Bytes::new(),
                };
                send_packet(&sock, &data, peer).await.map_err(TftpError::Io)?;
                wait_for(
                    &sock,
                    peer,
                    Duration::from_secs(u64::from(neg.timeout)),
                    |p| matches!(p, Packet::Ack { .. } | Packet::Error { .. }),
                )
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
        bytes_sent += n as u64;
        debug!(%peer, block, n, "send DATA");
        if let Some(tx) = &progress_tx {
            let _ = tx.send(bytes_sent);
        }
        let is_last_short = (n as u16) < neg.blksize;

        let wait_threshold = if neg.windowsize <= 1 {
            1
        } else {
            neg.windowsize
        };
        if block % wait_threshold == 0 || is_last_short || block == 1 {
            let mut waited_block: u16 = 0;
            while waited_block < block {
                let got = wait_for(
                    &sock,
                    peer,
                    Duration::from_secs(u64::from(neg.timeout)),
                    |p| matches!(p, Packet::Ack { .. } | Packet::Error { .. }),
                )
                .await?;
                match got {
                    Packet::Ack { block: b } => {
                        if u32::from(b) >= u32::from(block) {
                            waited_block = b;
                        } else {
                            debug!(%peer, want=block, got=b, "skip stale ACK");
                        }
                    }
                    Packet::Error { code, message } => {
                        return Err(TftpError::Remote { code, message });
                    }
                    _ => unreachable!(),
                }
            }
        }

        if is_last_short {
            break;
        }
        // TFTP block numbers are u16 and wrap naturally per RFC 1350.
        // Do NOT error on wrap — large files (>4GB with blksize=512,
        // >900MB with blksize=1468) require wrapping past block 65535.
        block = block.wrapping_add(1);
    }
    info!(%peer, "RRQ done");
    Ok(TransferStats {
        bytes: file_len,
        blocks: bytes_sent / u64::from(neg.blksize),
        total_bytes: Some(file_len),
        duration_ms: 0,
    })
}

async fn serve_write(
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
    path: &Path,
    req_opts: Options,
    progress_tx: Option<std::sync::mpsc::Sender<u64>>,
) -> Result<TransferStats, TftpError> {
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
        send_packet(&sock, &Packet::Ack { block: 0 }, peer)
            .await
            .map_err(TftpError::Io)?;
    }

    let mut received: u64 = 0;
    let mut f = match File::create(path).await {
        Ok(f) => f,
        Err(e) => {
            let _ = send_packet(
                &sock,
                &Packet::Error {
                    code: crate::error::TftpErrorCode::AccessViolation,
                    message: format!("cannot create file: {e}"),
                },
                peer,
            )
            .await;
            return Ok(TransferStats::default());
        }
    };
    let mut expected_block: u16 = 1;
    loop {
        let pkt = wait_for(
            &sock,
            peer,
            Duration::from_secs(u64::from(neg.timeout)),
            |p| matches!(p, Packet::Data { .. } | Packet::Error { .. }),
        )
        .await?;
        match pkt {
            Packet::Data { block, data } => {
                if block != expected_block {
                    send_packet(
                        &sock,
                        &Packet::Ack {
                            block: expected_block.wrapping_sub(1),
                        },
                        peer,
                    )
                    .await
                    .map_err(TftpError::Io)?;
                    continue;
                }
                received += data.len() as u64;
                f.write_all(&data).await.map_err(TftpError::Io)?;
                let last = (data.len() as u16) < neg.blksize;
                send_packet(&sock, &Packet::Ack { block }, peer)
                    .await
                    .map_err(TftpError::Io)?;
                if let Some(tx) = &progress_tx {
                    let _ = tx.send(received);
                }
                if last {
                    break;
                }
                // TFTP block numbers wrap naturally per RFC 1350.
                expected_block = expected_block.wrapping_add(1);
            }
            Packet::Error { code, message } => return Err(TftpError::Remote { code, message }),
            _ => unreachable!(),
        }
    }
    Ok(TransferStats {
        bytes: received,
        blocks: received / u64::from(neg.blksize),
        total_bytes: None,
        duration_ms: 0,
    })
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
        SocketAddr::V4(v4) => SocketAddr::V6(std::net::SocketAddrV6::new(
            v4.ip().to_ipv6_mapped(),
            v4.port(),
            0,
            0,
        )),
        other => other,
    };
    norm(a) == norm(b)
}
