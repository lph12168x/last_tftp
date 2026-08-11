//! TFTP 客户端：异步 GET / PUT。

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::net::lookup_host;
use tokio::net::UdpSocket;

use crate::error::TftpError;
use crate::options::{Negotiation, Options};
use crate::packet::Packet;
use crate::session::{ClientConfig, TransferStats};
use crate::transfer::{parse_target, recv_packet, send_packet, with_retry};

async fn resolve(host: &str, port: u16, prefer_v6: bool) -> Result<SocketAddr, TftpError> {
    if let Ok(addr) = host.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let addrs: Vec<_> = lookup_host((host, port))
        .await
        .map_err(|e| TftpError::Protocol(format!("dns resolve {host}: {e}")))?
        .collect();
    if addrs.is_empty() {
        return Err(TftpError::Protocol(format!("no addresses for {host}")));
    }
    let pick = if prefer_v6 {
        addrs.iter().find(|a| a.is_ipv6()).or_else(|| addrs.first())
    } else {
        addrs.iter().find(|a| a.is_ipv4()).or_else(|| addrs.first())
    };
    Ok(*pick.unwrap())
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub bytes: u64,
    pub blocks: u64,
    pub total_bytes: Option<u64>,
    pub instantaneous_bps: f64,
}

pub struct Client {
    cfg: ClientConfig,
    prefer_v6: bool,
    progress: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("cfg", &self.cfg)
            .field("prefer_v6", &self.prefer_v6)
            .field("progress", &self.progress.as_ref().map(|_| "<cb>"))
            .finish()
    }
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            cfg: self.cfg.clone(),
            prefer_v6: self.prefer_v6,
            progress: self.progress.clone(),
        }
    }
}

impl Client {
    pub fn new(cfg: ClientConfig) -> Self {
        Self { cfg, prefer_v6: false, progress: None }
    }

    pub fn prefer_ipv6(mut self, v: bool) -> Self {
        self.prefer_v6 = v;
        self
    }

    pub fn with_progress<F>(mut self, cb: F) -> Self
    where
        F: Fn(Progress) + Send + Sync + 'static,
    {
        self.progress = Some(Arc::new(cb));
        self
    }

    pub fn config(&self) -> &ClientConfig {
        &self.cfg
    }

    pub async fn get(
        &self,
        target: &str,
        port: u16,
        dest: &Path,
    ) -> Result<TransferStats, TftpError> {
        let (host, file) = parse_target(target)?;
        let server = resolve(&host, port, self.prefer_v6).await?;
        let bind_addr = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };

        let sock = UdpSocket::bind(bind_addr).await?;

        let mut req_opts = Options::new();
        req_opts.set_blksize(self.cfg.blksize);
        req_opts.set_timeout(self.cfg.timeout_secs);
        req_opts.set_windowsize(self.cfg.windowsize);
        let rrq = Packet::Rrq {
            filename: file.clone(),
            mode: "octet".into(),
            options: req_opts,
        };

        let (neg, mut server) = self.negotiate_get(&sock, server, &rrq).await?;

        let mut out = if self.cfg.resume_from_block > 0 {
            let mut f = File::create(dest).await?;
            f.seek(SeekFrom::Start(self.cfg.resume_from_block * u64::from(neg.blksize))).await?;
            f
        } else {
            File::create(dest).await?
        };

        let timeout = Duration::from_secs(u64::from(neg.timeout));
        let mut stats = TransferStats::default();
        let mut expected_block: u64 = self.cfg.resume_from_block + 1;
        let mut last_acked_block: u16 = self.cfg.resume_from_block as u16;
        // 主循环：单包超时累计 retries 次后放弃，每次超时重发上一个 ACK
        // 触发 server 端重传当前 block（RFC 1350 ACK 语义）。
        let max_retries = self.cfg.retries.max(1);
        let mut retries_left: u32 = max_retries;
        loop {
            // recv_packet 内部已有 tokio::time::timeout，这里只需处理 Result。
            let (pkt, from) = match recv_packet(&sock, timeout).await {
                Ok(v) => v,
                Err(e) => {
                    if retries_left == 0 {
                        return Err(TftpError::Timeout { retries: max_retries });
                    }
                    retries_left -= 1;
                    // 单包超时：重 ACK 最近一个 block，触发 server 重传当前 block。
                    let _ = send_packet(
                        &sock,
                        &Packet::Ack { block: last_acked_block },
                        server,
                    )
                    .await;
                    continue;
                }
            };
            retries_left = max_retries;
            if from.port() != 0 {
                server.set_port(from.port());
            }
            match pkt {
                Packet::Error { code, message } => {
                    return Err(TftpError::Remote { code, message });
                }
                Packet::Data { block, data } => {
                    let block_u64 = u64::from(block);
                    if block_u64 < u64::from(last_acked_block).max(expected_block) {
                        // 旧包重传：再 ACK 一次。
                        send_packet(&sock, &Packet::Ack { block }, server).await?;
                        last_acked_block = block;
                        continue;
                    }
                    if block_u64 > expected_block {
                        // 跳号：重 ACK 最近一个，触发 server 重传。
                        let prev = u16::try_from(expected_block - 1).unwrap_or(block);
                        send_packet(&sock, &Packet::Ack { block: prev }, server).await?;
                        last_acked_block = prev;
                        continue;
                    }
                    out.write_all(&data).await?;
                    stats.bytes += data.len() as u64;
                    stats.blocks += 1;
                    expected_block += 1;
                    last_acked_block = block;
                    if data.len() < neg.blksize as usize {
                        let _ = send_packet(&sock, &Packet::Ack { block }, server).await;
                        return Ok(stats);
                    }
                    if neg.windowsize <= 1 || stats.blocks % u64::from(neg.windowsize) == 0 {
                        let ack = Packet::Ack { block };
                        send_packet(&sock, &ack, server).await?;
                    }
                }
                other => {
                    return Err(TftpError::Protocol(format!(
                        "unexpected packet during DATA: {other:?}"
                    )));
                }
            }
        }
    }

    async fn negotiate_get(
        &self,
        sock: &UdpSocket,
        server: SocketAddr,
        rrq: &Packet,
    ) -> Result<(Negotiation, SocketAddr), TftpError> {
        let timeout = Duration::from_secs(u64::from(self.cfg.timeout_secs));
        let initial_neg = Negotiation {
            blksize: self.cfg.blksize,
            timeout: self.cfg.timeout_secs,
            windowsize: self.cfg.windowsize,
        };

        let sock_ref = sock;
        let rrq_owned = rrq.clone();
        let op = move |d: Duration| {
            let req = rrq_owned.clone();
            let srv = server;
            async move {
                send_packet(sock_ref, &req, srv).await.map_err(TftpError::Io)?;
                let r = tokio::time::timeout(d, recv_packet(sock_ref, d)).await;
                match r {
                    Ok(Ok(v)) => Ok(v),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(TftpError::Protocol("negotiate timeout".into())),
                }
            }
        };
        let (pkt, from): (Packet, SocketAddr) = with_retry(timeout, timeout * 8, self.cfg.retries, op).await?;

        match pkt {
            Packet::Oack { options } => {
                send_packet(sock_ref, &Packet::Ack { block: 0 }, from)
                    .await
                    .map_err(TftpError::Io)?;
                Ok((initial_neg.apply_oack(&options), from))
            }
            Packet::Data { block, .. } if block == 1 => Ok((initial_neg, from)),
            Packet::Error { code, message } => Err(TftpError::Remote { code, message }),
            other => Err(TftpError::Protocol(format!("unexpected reply: {other:?}"))),
        }
    }

    pub async fn put(
        &self,
        src: &Path,
        target: &str,
        port: u16,
    ) -> Result<TransferStats, TftpError> {
        let (host, file) = parse_target(target)?;
        let server = resolve(&host, port, self.prefer_v6).await?;
        let bind_addr = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
        let sock = UdpSocket::bind(bind_addr).await?;
        let meta = tokio::fs::metadata(src).await?;
        let total_bytes = meta.len();
        let mut req_opts = Options::new();

        req_opts.set_blksize(self.cfg.blksize);
        req_opts.set_timeout(self.cfg.timeout_secs);
        req_opts.set_windowsize(self.cfg.windowsize);
        req_opts.set_tsize(total_bytes);
        let wrq = Packet::Wrq {
            filename: file.clone(),
            mode: "octet".into(),
            options: req_opts,
        };

        let (neg, mut server) = self.negotiate_put(&sock, server, &wrq).await?;
        let timeout = Duration::from_secs(u64::from(neg.timeout));
        let mut buf = vec![0u8; neg.blksize as usize];
        let mut stats = TransferStats::default();
        let mut f = File::open(src).await?;
        let mut next_block: u16 = 1;

        loop {
            let n = f.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let chunk = bytes::Bytes::copy_from_slice(&buf[..n]);
            let pkt = Packet::Data { block: next_block, data: chunk };
            let local_block = next_block;
            let pkt_owned = pkt.clone();

            let sock_ref = &sock;
            let srv = server;
            let op = move |d: Duration| {
                let pkt = pkt_owned.clone();
                let srv = srv;
                async move {
                    send_packet(sock_ref, &pkt, srv).await.map_err(TftpError::Io)?;
                    let r = tokio::time::timeout(d, recv_packet(sock_ref, d)).await;
                    match r {
                        Ok(Ok((Packet::Ack { block: ack_block }, from))) if ack_block == local_block => Ok(from),
                        Ok(Ok((Packet::Error { code, message }, _))) => Err(TftpError::Remote { code, message }),
                        Ok(Ok(_)) => Err(TftpError::Protocol("unexpected ack".into())),
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err(TftpError::Protocol("ack timeout".into())),
                    }
                }
            };
            let from = with_retry(timeout, timeout * 8, self.cfg.retries, op).await?;
            if from.port() != 0 {
                server.set_port(from.port());
            }

            stats.bytes += n as u64;
            stats.blocks += 1;

            if n < neg.blksize as usize {
                return Ok(stats);
            }
            next_block = next_block.wrapping_add(1);
            if next_block == 0 {
                return Err(TftpError::Protocol("block counter wrap".into()));
            }
        }
        Ok(stats)
    }

    async fn negotiate_put(
        &self,
        sock: &UdpSocket,
        server: SocketAddr,
        wrq: &Packet,
    ) -> Result<(Negotiation, SocketAddr), TftpError> {
        let timeout = Duration::from_secs(u64::from(self.cfg.timeout_secs));
        let initial_neg = Negotiation {
            blksize: self.cfg.blksize,
            timeout: self.cfg.timeout_secs,
            windowsize: self.cfg.windowsize,
        };

        let sock_ref = sock;
        let wrq_owned = wrq.clone();
        let op = move |d: Duration| {
            let req = wrq_owned.clone();
            let srv = server;
            async move {
                send_packet(sock_ref, &req, srv).await.map_err(TftpError::Io)?;
                let r = tokio::time::timeout(d, recv_packet(sock_ref, d)).await;
                match r {
                    Ok(Ok(v)) => Ok(v),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(TftpError::Protocol("negotiate timeout".into())),
                }
            }
        };
        let (pkt, from): (Packet, SocketAddr) = with_retry(timeout, timeout * 8, self.cfg.retries, op).await?;

        match pkt {
            Packet::Oack { options } => Ok((initial_neg.apply_oack(&options), from)),
            Packet::Ack { block } if block == 0 => Ok((initial_neg, from)),
            Packet::Error { code, message } => Err(TftpError::Remote { code, message }),
            other => Err(TftpError::Protocol(format!("unexpected reply: {other:?}"))),
        }
    }
}