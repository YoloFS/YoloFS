// yolo CLI — perm.rs
//
// The `Perm` permission type — the single rule type shared across config,
// kernel ioctls, and journal ask decisions. Ask decisions are a subset:
// allow/write-ask/read-only/deny. `ask` and `hide` are rule-only.

use crate::ioctl;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Perm {
    Ask,
    Allow,
    WriteAsk,
    ReadOnly,
    Deny,
    Hide,
}

impl Perm {
    pub fn to_ioctl(self) -> u8 {
        match self {
            Perm::Ask => ioctl::YOLO_PERM_ASK,
            Perm::Allow => ioctl::YOLO_PERM_ALLOW,
            Perm::WriteAsk => ioctl::YOLO_PERM_WRITE_ASK,
            Perm::ReadOnly => ioctl::YOLO_PERM_READ_ONLY,
            Perm::Deny => ioctl::YOLO_PERM_DENY,
            Perm::Hide => ioctl::YOLO_PERM_HIDE,
        }
    }

    /// Inverse of [`to_ioctl`]. `None` for `UNSET` or any unknown value.
    pub fn from_ioctl(v: u8) -> Option<Self> {
        match v {
            ioctl::YOLO_PERM_ASK => Some(Perm::Ask),
            ioctl::YOLO_PERM_ALLOW => Some(Perm::Allow),
            ioctl::YOLO_PERM_WRITE_ASK => Some(Perm::WriteAsk),
            ioctl::YOLO_PERM_READ_ONLY => Some(Perm::ReadOnly),
            ioctl::YOLO_PERM_DENY => Some(Perm::Deny),
            ioctl::YOLO_PERM_HIDE => Some(Perm::Hide),
            _ => None,
        }
    }

    /// The journal's single-letter code for an ask decision. `ask` and `hide`
    /// are rule-only and cannot be written as A-record decisions.
    pub fn to_decision_letter(self) -> Option<char> {
        match self {
            Perm::Allow => Some('y'),
            Perm::WriteAsk => Some('w'),
            Perm::ReadOnly => Some('r'),
            Perm::Deny => Some('d'),
            Perm::Ask | Perm::Hide => None,
        }
    }

    /// Inverse of [`to_decision_letter`].
    pub fn from_decision_letter(b: u8) -> Option<Self> {
        match b {
            b'y' => Some(Perm::Allow),
            b'w' => Some(Perm::WriteAsk),
            b'r' => Some(Perm::ReadOnly),
            b'd' => Some(Perm::Deny),
            _ => None,
        }
    }
}

impl fmt::Display for Perm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Perm::Ask => "ask",
            Perm::Allow => "allow",
            Perm::WriteAsk => "write-ask",
            Perm::ReadOnly => "read-only",
            Perm::Deny => "deny",
            Perm::Hide => "hide",
        })
    }
}

impl FromStr for Perm {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "ask" => Ok(Perm::Ask),
            "allow" => Ok(Perm::Allow),
            "write-ask" => Ok(Perm::WriteAsk),
            "read-only" => Ok(Perm::ReadOnly),
            "deny" => Ok(Perm::Deny),
            "hide" => Ok(Perm::Hide),
            _ => anyhow::bail!("unknown permission: {s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Perm; 6] = [
        Perm::Ask,
        Perm::Allow,
        Perm::WriteAsk,
        Perm::ReadOnly,
        Perm::Deny,
        Perm::Hide,
    ];

    #[test]
    fn decision_letter_roundtrips() {
        let decisions = [Perm::Allow, Perm::WriteAsk, Perm::ReadOnly, Perm::Deny];
        for p in decisions {
            assert_eq!(
                Perm::from_decision_letter(p.to_decision_letter().unwrap() as u8),
                Some(p)
            );
        }
        assert_eq!(Perm::Allow.to_decision_letter(), Some('y'));
        assert_eq!(Perm::WriteAsk.to_decision_letter(), Some('w'));
        assert_eq!(Perm::ReadOnly.to_decision_letter(), Some('r'));
        assert_eq!(Perm::Deny.to_decision_letter(), Some('d'));
        assert_eq!(Perm::Ask.to_decision_letter(), None);
        assert_eq!(Perm::Hide.to_decision_letter(), None);
        assert_eq!(Perm::from_decision_letter(b'a'), None);
        assert_eq!(Perm::from_decision_letter(b'h'), None);
        assert_eq!(Perm::from_decision_letter(b'?'), None);
    }

    #[test]
    fn ioctl_roundtrips() {
        for p in ALL {
            assert_eq!(Perm::from_ioctl(p.to_ioctl()), Some(p));
        }
        assert_eq!(Perm::from_ioctl(ioctl::YOLO_PERM_UNSET), None);
    }
}
