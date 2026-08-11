//! 会话状态机骨架。
//!
//! 当前仅提供配置与统计结构；具体的 client/server 收发逻辑将在 P3-P6
//! 阶段逐步填入，以避免一次写太多未经验证的代码。

use crate::options::Negotiation;

/// 客户端配置。
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub blksize: u16,
    pub timeout_secs: u16,
    pub windowsize: u16,
    pub retries: u32,
    /// 断点续传：上次的 block 号，0 表示从头开始。
    pub resume_from_block: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            blksize: crate::DEFAULT_BLOCK_SIZE,
            timeout_secs: crate::DEFAULT_TIMEOUT_SECS,
            windowsize: crate::DEFAULT_WINDOW_SIZE,
            retries: 6,
            resume_from_block: 0,
        }
    }
}

/// 单次传输的统计。
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    pub bytes: u64,
    pub blocks: u64,
    /// 端到端总耗时（毫秒）。
    pub duration_ms: u64,
}

impl TransferStats {
    pub fn throughput_bps(&self) -> f64 {
        if self.duration_ms == 0 {
            0.0
        } else {
            (self.bytes as f64) * 8.0 * 1000.0 / (self.duration_ms as f64)
        }
    }
}

/// 应用协商后的最终参数。
pub fn apply_negotiation(cfg: &ClientConfig, neg: Negotiation) -> ClientConfig {
    let mut out = cfg.clone();
    out.blksize = neg.blksize;
    out.timeout_secs = neg.timeout;
    out.windowsize = neg.windowsize;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_throughput_calculation() {
        let s = TransferStats {
            bytes: 1_000_000,
            blocks: 1_000_000 / 512,
            duration_ms: 8_000,
        };
        // 1MB in 8s -> 1 Mbps
        assert!((s.throughput_bps() - 1_000_000.0).abs() < 1.0);
    }

    #[test]
    fn stats_zero_duration_safe() {
        let s = TransferStats::default();
        assert_eq!(s.throughput_bps(), 0.0);
    }

    #[test]
    fn apply_negotiation_overrides() {
        let cfg = ClientConfig::default();
        let neg = Negotiation {
            blksize: 1468,
            timeout: 3,
            windowsize: 8,
        };
        let out = apply_negotiation(&cfg, neg);
        assert_eq!(out.blksize, 1468);
        assert_eq!(out.timeout_secs, 3);
        assert_eq!(out.windowsize, 8);
    }
}