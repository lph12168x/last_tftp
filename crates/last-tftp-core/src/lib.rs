//! last-tftp-core: TFTP 协议核心库（无 IO 之外依赖）。
//!
//! 分层：
//! - [`packet`]   报文编解码
//! - [`opcode`]   操作码常量
//! - [`options`]  RFC 2347/2348/2349/7440 选项协商
//! - [`error`]    TFTP 错误码
//! - [`session`]  客户端/服务端会话状态机

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod client;
pub mod error;
pub mod opcode;
pub mod options;
pub mod packet;
pub mod server;
pub mod session;
pub mod transfer;

pub use error::TftpError;
pub use opcode::Opcode;
pub use options::Negotiation;
pub use packet::{Packet, ParseError};
pub use session::{ClientConfig, TransferStats};

/// 协议规范块大小（RFC 1350 默认值）。
pub const DEFAULT_BLOCK_SIZE: u16 = 512;
/// 最大协商块大小（RFC 2348）。
pub const MAX_BLOCK_SIZE: u16 = 65464;
/// 默认超时秒数。
pub const DEFAULT_TIMEOUT_SECS: u16 = 5;
/// 默认窗口大小（RFC 7440）。
pub const DEFAULT_WINDOW_SIZE: u16 = 1;
/// 最大窗口大小（RFC 7440 上限为 65535，但实际可用受单端口缓存约束）。
pub const MAX_WINDOW_SIZE: u16 = 65535;

/// TFTP 传输模式：仅实现 octet。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Octet,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Octet => "octet",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "octet" => Some(Mode::Octet),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse_roundtrip() {
        assert_eq!(Mode::parse("octet"), Some(Mode::Octet));
        assert_eq!(Mode::parse("OCTET"), Some(Mode::Octet));
        assert_eq!(Mode::parse("netascii"), None);
        assert_eq!(Mode::Octet.as_str(), "octet");
    }

    #[test]
    fn const_values_match_rfc() {
        assert_eq!(DEFAULT_BLOCK_SIZE, 512);
        assert_eq!(MAX_BLOCK_SIZE, 65464);
    }
}