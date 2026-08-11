//! TFTP 错误码定义（RFC 1350 §5 + 私有扩展）。

use thiserror::Error;

/// 协议级错误（可在 ERROR 报文中传递）。
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TftpErrorCode {
    NotDefined = 0,
    FileNotFound = 1,
    AccessViolation = 2,
    DiskFull = 3,
    IllegalOperation = 4,
    UnknownTransferId = 5,
    FileAlreadyExists = 6,
    NoSuchUser = 7,
    /// RFC 2347：选项协商失败。
    OptionNegotiation = 8,
}

impl TftpErrorCode {
    pub fn from_u16(v: u16) -> Self {
        match v {
            1 => Self::FileNotFound,
            2 => Self::AccessViolation,
            3 => Self::DiskFull,
            4 => Self::IllegalOperation,
            5 => Self::UnknownTransferId,
            6 => Self::FileAlreadyExists,
            7 => Self::NoSuchUser,
            8 => Self::OptionNegotiation,
            _ => Self::NotDefined,
        }
    }

    pub fn as_u16(self) -> u16 {
        self as u16
    }

    pub fn default_message(self) -> &'static str {
        match self {
            Self::NotDefined => "Not defined",
            Self::FileNotFound => "File not found",
            Self::AccessViolation => "Access violation",
            Self::DiskFull => "Disk full or allocation exceeded",
            Self::IllegalOperation => "Illegal TFTP operation",
            Self::UnknownTransferId => "Unknown transfer ID",
            Self::FileAlreadyExists => "File already exists",
            Self::NoSuchUser => "No such user",
            Self::OptionNegotiation => "Option negotiation failed",
        }
    }
}

/// 本地运行时错误（不会下发到 TFTP 报文）。
#[derive(Debug, Error)]
pub enum TftpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid packet: {0}")]
    Parse(String),

    #[error("remote sent ERROR {code:?}: {message}")]
    Remote { code: TftpErrorCode, message: String },

    #[error("timeout after {retries} retries")]
    Timeout { retries: u32 },

    #[error("transfer aborted by user")]
    Aborted,

    #[error("unsupported option: {0}")]
    UnsupportedOption(String),

    #[error("invalid block size: {0}")]
    InvalidBlockSize(u16),

    #[error("protocol violation: {0}")]
    Protocol(String),
}