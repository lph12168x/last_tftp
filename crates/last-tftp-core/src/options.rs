//! RFC 2347/2348/2349/7440 选项协商。
//!
//! 编码：`name\0value\0 name\0value\0 ...`。
//! 协商规则：客户端请求 → 服务端 OACK 应答，仅接受服务端裁定的值。

use std::collections::BTreeMap;

/// 选项名字符串常量。
pub const BLKSIZE: &str = "blksize";
pub const TIMEOUT: &str = "timeout";
pub const TSIZE: &str = "tsize";
pub const WINDOWSIZE: &str = "windowsize";

/// TFTP 选项集合。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    inner: BTreeMap<String, String>,
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.inner.get(name).map(String::as_str)
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.inner.insert(name.into(), value.into());
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    // ---- 类型化便捷接口 ----

    pub fn blksize(&self) -> Option<u16> {
        self.get(BLKSIZE).and_then(|s| s.parse().ok())
    }

    pub fn set_blksize(&mut self, v: u16) {
        self.insert(BLKSIZE, v.to_string());
    }

    pub fn timeout(&self) -> Option<u16> {
        self.get(TIMEOUT).and_then(|s| s.parse().ok())
    }

    pub fn set_timeout(&mut self, v: u16) {
        self.insert(TIMEOUT, v.to_string());
    }

    pub fn tsize(&self) -> Option<u64> {
        self.get(TSIZE).and_then(|s| s.parse().ok())
    }

    pub fn set_tsize(&mut self, v: u64) {
        self.insert(TSIZE, v.to_string());
    }

    pub fn windowsize(&self) -> Option<u16> {
        self.get(WINDOWSIZE).and_then(|s| s.parse().ok())
    }

    pub fn set_windowsize(&mut self, v: u16) {
        self.insert(WINDOWSIZE, v.to_string());
    }

    /// 从一段已剥离 opcode 的载荷尾部解析。
    pub(crate) fn decode_from(mut buf: &[u8]) -> Result<Self, crate::packet::ParseError> {
        use crate::packet::ParseError;
        let mut out = Options::default();
        while !buf.is_empty() {
            let (name, rest) = take_cstr(buf)?;
            if rest.is_empty() {
                // RFC 2347：选项必须成对 name\0value\0。
                return Err(ParseError::UnterminatedString);
            }
            let (value, rest) = take_cstr(rest)?;
            out.inner.insert(name, value);
            buf = rest;
        }
        Ok(out)
    }

    pub(crate) fn encode_into(&self, buf: &mut bytes::BytesMut) {
        use bytes::BufMut;
        for (k, v) in &self.inner {
            buf.put_slice(k.as_bytes());
            buf.put_u8(0);
            buf.put_slice(v.as_bytes());
            buf.put_u8(0);
        }
    }
}

fn take_cstr(buf: &[u8]) -> Result<(String, &[u8]), crate::packet::ParseError> {
    use crate::packet::ParseError;
    let pos = buf.iter().position(|&b| b == 0).ok_or(ParseError::UnterminatedString)?;
    let s = std::str::from_utf8(&buf[..pos]).map_err(|_| ParseError::InvalidUtf8)?;
    Ok((s.to_string(), &buf[pos + 1..]))
}

/// 选项协商结果：经过服务端确认后的最终参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Negotiation {
    pub blksize: u16,
    pub timeout: u16,
    pub windowsize: u16,
    /// RRQ 时 server 回 OACK 带的文件大小，client GET 据此算进度百分比。
    pub tsize: Option<u64>,
}

impl Negotiation {
    pub const fn defaults() -> Self {
        Self {
            blksize: crate::DEFAULT_BLOCK_SIZE,
            timeout: crate::DEFAULT_TIMEOUT_SECS,
            windowsize: crate::DEFAULT_WINDOW_SIZE,
            tsize: None,
        }
    }

    /// 应用服务端 OACK 的裁决结果。
    pub fn apply_oack(&self, oack: &Options) -> Self {
        let mut out = *self;
        if let Some(v) = oack.blksize() {
            out.blksize = v;
        }
        if let Some(v) = oack.timeout() {
            out.timeout = v;
        }
        if let Some(v) = oack.windowsize() {
            out.windowsize = v;
        }
        if let Some(v) = oack.tsize() {
            out.tsize = Some(v);
        }
        out
    }
}

/// 校验客户端请求的合法性，返回建议裁定的参数。
pub fn validate_request(req: &Options) -> Result<Negotiation, crate::TftpError> {
    let mut n = Negotiation::defaults();
    if let Some(v) = req.blksize() {
        if !(8..=crate::MAX_BLOCK_SIZE).contains(&v) {
            return Err(crate::TftpError::InvalidBlockSize(v));
        }
        n.blksize = v;
    }
    if let Some(v) = req.timeout() {
        if v == 0 || v > 255 {
            return Err(crate::TftpError::Protocol(format!("invalid timeout {v}")));
        }
        n.timeout = v;
    }
    if let Some(v) = req.windowsize() {
        if v == 0 {
            return Err(crate::TftpError::Protocol("windowsize must be > 0".into()));
        }
        n.windowsize = v.min(crate::MAX_WINDOW_SIZE);
    }
    n.tsize = req.tsize();
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let mut opts = Options::new();
        opts.set_blksize(1468);
        opts.set_timeout(3);
        opts.set_windowsize(8);

        let mut buf = bytes::BytesMut::new();
        opts.encode_into(&mut buf);
        let parsed = Options::decode_from(&buf).unwrap();
        assert_eq!(opts, parsed);
    }

    #[test]
    fn empty_options_encode_to_nothing() {
        let opts = Options::new();
        let mut buf = bytes::BytesMut::new();
        opts.encode_into(&mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn negotiation_defaults() {
        let n = Negotiation::defaults();
        assert_eq!(n.blksize, 512);
        assert_eq!(n.timeout, 5);
        assert_eq!(n.windowsize, 1);
        assert_eq!(n.tsize, None);
    }

    #[test]
    fn apply_oack_overrides_only_present() {
        let n = Negotiation::defaults();
        let mut oack = Options::new();
        oack.set_blksize(8192);
        let applied = n.apply_oack(&oack);
        assert_eq!(applied.blksize, 8192);
        assert_eq!(applied.timeout, 5);
        assert_eq!(applied.windowsize, 1);
        assert_eq!(applied.tsize, None);
    }

    #[test]
    fn apply_oack_preserves_tsize_from_oack() {
        let n = Negotiation::defaults();
        let mut oack = Options::new();
        oack.set_tsize(102400);
        let applied = n.apply_oack(&oack);
        assert_eq!(applied.tsize, Some(102400));
    }

    #[test]
    fn validate_request_blksize_bounds() {
        let mut req = Options::new();
        req.set_blksize(7);
        assert!(matches!(validate_request(&req), Err(crate::TftpError::InvalidBlockSize(7))));

        req.set_blksize(65465);
        assert!(matches!(
            validate_request(&req),
            Err(crate::TftpError::InvalidBlockSize(65465))
        ));

        req.set_blksize(1468);
        let n = validate_request(&req).unwrap();
        assert_eq!(n.blksize, 1468);
    }

    #[test]
    fn validate_request_timeout_bounds() {
        let mut req = Options::new();
        req.set_timeout(0);
        assert!(validate_request(&req).is_err());
        req.set_timeout(256);
        assert!(validate_request(&req).is_err());
        req.set_timeout(60);
        assert_eq!(validate_request(&req).unwrap().timeout, 60);
    }
}
