//! TFTP 报文编解码（RFC 1350 + RFC 2347）。
//!
//! 所有报文均以 2 字节大端 opcode 开头；字符串字段以 `\0` 结尾。
//! 多个字符串/选项连续出现在载荷中，无显式分隔符。

use crate::error::{TftpError, TftpErrorCode};
use crate::opcode::Opcode;
use crate::options::Options;
use bytes::BufMut;

/// 报文解析错误（属于协议层）。
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("buffer too short: need {need}, got {got}")]
    Truncated { need: usize, got: usize },

    #[error("unknown opcode: {0}")]
    UnknownOpcode(u16),

    #[error("missing NUL terminator in string field")]
    UnterminatedString,

    #[error("invalid UTF-8 in string field")]
    InvalidUtf8,

    #[error("DATA block number mismatch: expected {expected}, got {got}")]
    BlockMismatch { expected: u16, got: u16 },

    #[error("DATA block number wraps from 65535 to 0 not allowed in single transfer")]
    BlockWrap,
}

impl From<ParseError> for TftpError {
    fn from(e: ParseError) -> Self {
        TftpError::Parse(e.to_string())
    }
}

/// 解码后的报文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Rrq {
        filename: String,
        mode: String,
        options: Options,
    },
    Wrq {
        filename: String,
        mode: String,
        options: Options,
    },
    Data {
        block: u16,
        /// 不超过协商后 blksize，最小 0（最后一次小于完整块）。
        data: bytes::Bytes,
    },
    Ack {
        block: u16,
    },
    Error {
        code: TftpErrorCode,
        message: String,
    },
    Oack {
        options: Options,
    },
}

impl Packet {
    pub fn opcode(&self) -> Opcode {
        match self {
            Self::Rrq { .. } => Opcode::Rrq,
            Self::Wrq { .. } => Opcode::Wrq,
            Self::Data { .. } => Opcode::Data,
            Self::Ack { .. } => Opcode::Ack,
            Self::Error { .. } => Opcode::Error,
            Self::Oack { .. } => Opcode::Oack,
        }
    }

    /// 序列化为字节。用于 wire 传输。
    pub fn encode(&self) -> bytes::BytesMut {
        use bytes::BufMut;
        let mut buf = bytes::BytesMut::new();
        match self {
            Self::Rrq { filename, mode, options } | Self::Wrq { filename, mode, options } => {
                buf.put_u16(self.opcode().as_u16());
                put_cstr(&mut buf, filename);
                put_cstr(&mut buf, mode);
                options.encode_into(&mut buf);
            }
            Self::Data { block, data } => {
                buf.put_u16(Opcode::Data.as_u16());
                buf.put_u16(*block);
                buf.put_slice(data);
            }
            Self::Ack { block } => {
                buf.put_u16(Opcode::Ack.as_u16());
                buf.put_u16(*block);
            }
            Self::Error { code, message } => {
                buf.put_u16(Opcode::Error.as_u16());
                buf.put_u16(code.as_u16());
                put_cstr(&mut buf, message);
            }
            Self::Oack { options } => {
                buf.put_u16(Opcode::Oack.as_u16());
                options.encode_into(&mut buf);
            }
        }
        buf
    }

    /// 从字节流解析。不会消费多余字节（用于 UDP 一次一报文场景）。
    pub fn parse(buf: &[u8]) -> Result<Self, ParseError> {
        if buf.len() < 2 {
            return Err(ParseError::Truncated { need: 2, got: buf.len() });
        }
        let op = u16::from_be_bytes([buf[0], buf[1]]);
        let rest = &buf[2..];
        match Opcode::from_u16(op) {
            Some(Opcode::Rrq) | Some(Opcode::Wrq) => {
                let (filename, rest) = take_cstr(rest)?;
                let (mode, rest) = take_cstr(rest)?;
                let options = Options::decode_from(rest)?;
                Ok(if op == Opcode::Rrq.as_u16() {
                    Self::Rrq { filename, mode, options }
                } else {
                    Self::Wrq { filename, mode, options }
                })
            }
            Some(Opcode::Data) => {
                if rest.len() < 2 {
                    return Err(ParseError::Truncated { need: 2, got: rest.len() });
                }
                let block = u16::from_be_bytes([rest[0], rest[1]]);
                let data = bytes::Bytes::copy_from_slice(&rest[2..]);
                Ok(Self::Data { block, data })
            }
            Some(Opcode::Ack) => {
                if rest.len() < 2 {
                    return Err(ParseError::Truncated { need: 2, got: rest.len() });
                }
                Ok(Self::Ack { block: u16::from_be_bytes([rest[0], rest[1]]) })
            }
            Some(Opcode::Error) => {
                if rest.len() < 2 {
                    return Err(ParseError::Truncated { need: 2, got: rest.len() });
                }
                let code = u16::from_be_bytes([rest[0], rest[1]]);
                let (message, _) = take_cstr(&rest[2..])?;
                Ok(Self::Error {
                    code: TftpErrorCode::from_u16(code),
                    message,
                })
            }
            Some(Opcode::Oack) => {
                let options = Options::decode_from(rest)?;
                Ok(Self::Oack { options })
            }
            None => Err(ParseError::UnknownOpcode(op)),
        }
    }
}

fn put_cstr(buf: &mut bytes::BytesMut, s: &str) {
    buf.put_slice(s.as_bytes());
    buf.put_u8(0);
}

fn take_cstr(buf: &[u8]) -> Result<(String, &[u8]), ParseError> {
    let pos = buf.iter().position(|&b| b == 0).ok_or(ParseError::UnterminatedString)?;
    let s = std::str::from_utf8(&buf[..pos]).map_err(|_| ParseError::InvalidUtf8)?;
    Ok((s.to_string(), &buf[pos + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_rrq_no_options() {
        let p = Packet::Rrq {
            filename: "kernel.bin".into(),
            mode: "octet".into(),
            options: Options::default(),
        };
        let bytes = p.encode();
        assert_eq!(bytes[0..2], [0, 1]); // opcode RRQ
        let parsed = Packet::parse(&bytes).unwrap();
        assert_eq!(parsed, p);
    }

    #[test]
    fn encode_decode_wrq_with_options() {
        let mut opts = Options::default();
        opts.set_blksize(1468);
        opts.set_timeout(3);
        let p = Packet::Wrq {
            filename: "fw.img".into(),
            mode: "octet".into(),
            options: opts.clone(),
        };
        let parsed = Packet::parse(&p.encode()).unwrap();
        assert_eq!(parsed, p);
    }

    #[test]
    fn encode_decode_data_and_ack() {
        let data = Packet::Data {
            block: 42,
            data: bytes::Bytes::from_static(b"hello"),
        };
        assert_eq!(Packet::parse(&data.encode()).unwrap(), data);

        let ack = Packet::Ack { block: 42 };
        assert_eq!(Packet::parse(&ack.encode()).unwrap(), ack);
    }

    #[test]
    fn encode_decode_error() {
        let err = Packet::Error {
            code: TftpErrorCode::FileNotFound,
            message: "no such file".into(),
        };
        assert_eq!(Packet::parse(&err.encode()).unwrap(), err);
    }

    #[test]
    fn encode_decode_oack() {
        let mut opts = Options::default();
        opts.set_blksize(1468);
        opts.set_windowsize(8);
        let oack = Packet::Oack { options: opts.clone() };
        assert_eq!(Packet::parse(&oack.encode()).unwrap(), oack);
    }

    #[test]
    fn truncated_buffer_rejected() {
        let buf = [0u8, 1]; // only opcode, missing NUL-terminated filename
        let err = Packet::parse(&buf).unwrap_err();
        assert!(matches!(err, ParseError::UnterminatedString));
    }

    #[test]
    fn unknown_opcode_rejected() {
        let buf = [0u8, 99, 0, 0];
        let err = Packet::parse(&buf).unwrap_err();
        assert!(matches!(err, ParseError::UnknownOpcode(99)));
    }
}