//! last-tftp 二进制入口。
//!
//! 双击（无子命令）默认进 GUI，不弹 console 窗口；CLI 子命令从 cmd 启动时
//! 自动 attach 父 console，stdout/stderr 正常输出。
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use last_tftp_core::client::Client;
use last_tftp_core::session::ClientConfig;

mod commands;
mod gui;
mod progress;
#[derive(Debug, Parser)]
#[command(version, about = "last-tftp: modern cross-platform TFTP tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// 启用 IPv6（默认 IPv4）
    #[arg(long, global = true)]
    ipv6: bool,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// 从 TFTP 服务器下载文件
    Get {
        /// 形如 HOST:FILE
        target: String,
        /// 本地保存路径
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// TFTP 端口
        #[arg(long, default_value_t = 69)]
        port: u16,
        #[arg(long, default_value_t = 1468)]
        blksize: u16,
        #[arg(long, default_value_t = 5)]
        timeout: u16,
        #[arg(long, default_value_t = 1)]
        window: u16,
        #[arg(long, default_value_t = 6)]
        retries: u32,
        #[arg(long)]
        resume: bool,
        #[arg(long)]
        no_progress: bool,
        /// 输出 JSON 统计到 stdout
        #[arg(long)]
        json: bool,
    },
    /// 上传本地文件到 TFTP 服务器
    Put {
        #[arg(short = 'l', long)]
        local: PathBuf,
        /// 形如 HOST:FILE
        target: String,
        #[arg(long, default_value_t = 69)]
        port: u16,
        #[arg(long, default_value_t = 1468)]
        blksize: u16,
        #[arg(long, default_value_t = 5)]
        timeout: u16,
        #[arg(long, default_value_t = 1)]
        window: u16,
        #[arg(long, default_value_t = 6)]
        retries: u32,
        #[arg(long)]
        no_progress: bool,
        #[arg(long)]
        json: bool,
    },
    /// 运行内置 TFTP 服务器（同时支持 IPv4 和 IPv6）
    Server {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, default_value_t = 6969)]
        port: u16,
        #[arg(long)]
        allow_write: bool,
    },
    /// 仅做一次 OACK 协商探测，输出对端能力
    Probe {
        target: String,
        #[arg(long, default_value_t = 69)]
        port: u16,
    },
    /// 启动 GUI 桌面应用
    Gui,
}

fn main() -> anyhow::Result<()> {
    attach_console_if_available();
    init_tracing();
    let cli = Cli::parse();
    // 双击启动默认走 GUI，且完全跳过 tokio 顶层 runtime 构造，
    // 避免 Windows 上 GUI 启动时弹 console 窗口。
    if matches!(cli.cmd, None | Some(Cmd::Gui)) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        return rt.block_on(commands::gui_run());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let cmd = cli.cmd.unwrap();
        match cmd {
            Cmd::Get {
                target,
                output,
                port,
                blksize,
                timeout,
                window,
                retries,
                resume,
                no_progress,
                json,
            } => commands::get_run(target, output, port, blksize, timeout, window, retries, resume, !no_progress, json, cli.ipv6).await,
            Cmd::Put {
                local,
                target,
                port,
                blksize,
                timeout,
                window,
                retries,
                no_progress,
                json,
            } => commands::put_run(local, target, port, blksize, timeout, window, retries, !no_progress, json, cli.ipv6).await,
            Cmd::Server { root, port, allow_write } => commands::server_run(root, port, allow_write, cli.ipv6).await,
            Cmd::Probe { target, port } => commands::probe_run(target, port, cli.ipv6).await,
            Cmd::Gui => unreachable!(),
        }
    })
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[allow(dead_code)]
fn _silence_unused(_: Client, _: ClientConfig) {}
#[cfg(target_os = "windows")]
fn attach_console_if_available() {
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(dwProcessId: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(target_os = "windows"))]
fn attach_console_if_available() {}
