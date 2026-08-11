//! TFTP 操作码常量（RFC 1350 + RFC 2347）。

use serde::{Deserialize, Serialize};

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Opcode {
    Rrq = 1,
    Wrq = 2,
    Data = 3,
    Ack = 4,
    Error = 5,
    Oack = 6,
}

impl Opcode {
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::Rrq),
            2 => Some(Self::Wrq),
            3 => Some(Self::Data),
            4 => Some(Self::Ack),
            5 => Some(Self::Error),
            6 => Some(Self::Oack),
            _ => None,
        }
    }

    pub fn as_u16(self) -> u16 {
        self as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_roundtrip() {
        for op in [
            Opcode::Rrq,
            Opcode::Wrq,
            Opcode::Data,
            Opcode::Ack,
            Opcode::Error,
            Opcode::Oack,
        ] {
            assert_eq!(Opcode::from_u16(op.as_u16()), Some(op));
        }
    }

    #[test]
    fn unknown_opcode_rejected() {
        assert_eq!(Opcode::from_u16(0), None);
        assert_eq!(Opcode::from_u16(7), None);
        assert_eq!(Opcode::from_u16(u16::MAX), None);
    }
}