//! 各 CLI 子命令的实现。

use std::path::PathBuf;
use std::time::Instant;
use last_tftp_core::client::Client;
use last_tftp_core::session::ClientConfig;
use serde::Serialize;
use crate::progress::CliProgress;

#[derive(Debug, Serialize)]
struct Stats {
    bytes: u64,
    blocks: u64,
    duration_ms: u64,
    throughput_bps: f64,
}

fn build_client(
    blksize: u16,
    timeout: u16,
    window: u16,
    retries: u32,
    resume: bool,
    ipv6: bool,
) -> Client {
    let cfg = ClientConfig {
        blksize,
        timeout_secs: timeout,
        windowsize: window,
        retries,
        resume_from_block: if resume { 1 } else { 0 },
    };
    Client::new(cfg).prefer_ipv6(ipv6)
}

pub async fn get_run(
    target: String,
    output: PathBuf,
    port: u16,
    blksize: u16,
    timeout: u16,
    window: u16,
    retries: u32,
    resume: bool,
    progress: bool,
    json: bool,
    ipv6: bool,
) -> anyhow::Result<()> {
    let client = build_client(blksize, timeout, window, retries, resume, ipv6);
    let pb = if progress { Some(CliProgress::new_spinner(&target)) } else { None };
    let start = Instant::now();
    let stats = if let Some(pb) = pb {
        let pb = std::sync::Arc::new(pb);
        let cb_pb = pb.clone();
        let client = client.with_progress(move |p| cb_pb.update(p.bytes, None));
        client.get(&target, port, &output).await?
    } else {
        client.get(&target, port, &output).await?
    };
    let elapsed = start.elapsed();
    if json {
        let s = Stats {
            bytes: stats.bytes,
            blocks: stats.blocks,
            duration_ms: stats.duration_ms,
            throughput_bps: stats.throughput_bps(),
        };
        println!("{}", serde_json::to_string(&s)?);
    } else {
        println!(
            "downloaded {} bytes in {} ms ({:.2} Mbps)",
            stats.bytes,
            elapsed.as_millis(),
            stats.throughput_bps() / 1_000_000.0
        );
    }
    Ok(())
}

pub async fn put_run(
    local: PathBuf,
    target: String,
    port: u16,
    blksize: u16,
    timeout: u16,
    window: u16,
    retries: u32,
    progress: bool,
    json: bool,
    ipv6: bool,
) -> anyhow::Result<()> {
    let total = tokio::fs::metadata(&local).await?.len();
    let client = build_client(blksize, timeout, window, retries, false, ipv6);
    let pb = if progress { Some(CliProgress::new_bar(total, &target)) } else { None };
    let start = Instant::now();
    let stats = if let Some(pb) = pb {
        let pb = std::sync::Arc::new(pb);
        let cb_pb = pb.clone();
        let client = client.with_progress(move |p| cb_pb.update(p.bytes, p.total_bytes));
        client.put(&local, &target, port).await?
    } else {
        client.put(&local, &target, port).await?
    };
    let elapsed = start.elapsed();
    if json {
        let s = Stats {
            bytes: stats.bytes,
            blocks: stats.blocks,
            duration_ms: stats.duration_ms,
            throughput_bps: stats.throughput_bps(),
        };
        println!("{}", serde_json::to_string(&s)?);
    } else {
        println!(
            "uploaded {} bytes in {} ms ({:.2} Mbps)",
            stats.bytes,
            elapsed.as_millis(),
            stats.throughput_bps() / 1_000_000.0
        );
    }
    Ok(())
}

pub async fn probe_run(_target: String, _port: u16, _ipv6: bool) -> anyhow::Result<()> {
    anyhow::bail!("probe not yet implemented in this stage; use `get` to verify")
}

pub async fn server_run(root: PathBuf, port: u16, allow_write: bool, ipv6: bool) -> anyhow::Result<()> {
    let bind: std::net::SocketAddr = if ipv6 {
        format!("[::]:{port}").parse()?
    } else {
        format!("0.0.0.0:{port}").parse()?
    };
    let cfg = last_tftp_core::server::ServerConfig::new(root, allow_write);
    last_tftp_core::server::serve(bind, cfg).await?;
    Ok(())
}

pub async fn gui_run() -> anyhow::Result<()> {
    // 检查环境变量触发自动 Get（headless 验证用）
    let auto = std::env::var("LAST_TFTP_AUTO_GET").ok().and_then(|s| {
        // 格式: "host,port,file,local_path"
        let parts: Vec<&str> = s.splitn(4, ',').collect();
        if parts.len() == 4 {
            Some((parts[0].to_string(), parts[1].parse().ok()?, parts[2].to_string(), std::path::PathBuf::from(parts[3])))
        } else {
            None
        }
    });
    crate::gui::run_with_options(auto)
}
