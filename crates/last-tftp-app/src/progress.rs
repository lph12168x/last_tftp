//! CLI 进度条：基于 indicatif。
//!
//! 提供两种：
//! - `new_spinner` GET 模式（未知大小）
//! - `new_bar` PUT 模式（已知大小）

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

pub struct CliProgress {
    bar: ProgressBar,
}
impl CliProgress {
    pub fn new_spinner(target: &str) -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_draw_target(ProgressDrawTarget::stderr());
        pb.set_style(
            ProgressStyle::with_template("{spinner:.green} {msg} {bytes} {bytes_per_sec}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
        );
        pb.set_message(format!("GET {target}"));
        Self { bar: pb }
    }

    pub fn new_bar(total: u64, target: &str) -> Self {
        let pb = ProgressBar::new(total);
        pb.set_draw_target(ProgressDrawTarget::stderr());
        pb.set_style(
            ProgressStyle::with_template(
                "[{elapsed_precise}] {bar:40.cyan/blue} {bytes}/{total_bytes} {bytes_per_sec} ETA {eta}",
            )
            .unwrap()
            .progress_chars("##-"),
        );
        pb.set_message(format!("PUT {target}"));
        Self { bar: pb }
    }

    pub fn update(&self, bytes: u64, _total: Option<u64>) {
        // 简单节流：每次都更新，indicatif 内部会限流
        self.bar.set_position(bytes);
    }
}