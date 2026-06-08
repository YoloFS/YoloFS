// yolo CLI — perm.rs
//
// Permission rule modes and one-shot ask decisions.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Deny,
    Allow,
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
}

impl Decision {
    pub fn to_ioctl(self) -> u8 {
        match self {
            Decision::Deny => ioctl::YOLO_DECISION_DENY,
            Decision::Allow => ioctl::YOLO_DECISION_ALLOW,
        }
    }

    /// The journal's single-letter code for an ask decision.
    pub fn to_letter(self) -> char {
        match self {
            Decision::Allow => 'y',
            Decision::Deny => 'd',
        }
    }

    /// Inverse of [`to_letter`].
    pub fn from_letter(b: u8) -> Option<Self> {
        match b {
            b'y' => Some(Decision::Allow),
            b'd' => Some(Decision::Deny),
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

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
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
        let decisions = [Decision::Allow, Decision::Deny];
        for p in decisions {
            assert_eq!(Decision::from_letter(p.to_letter() as u8), Some(p));
        }
        assert_eq!(Decision::Allow.to_letter(), 'y');
        assert_eq!(Decision::Deny.to_letter(), 'd');
        assert_eq!(Decision::from_letter(b'a'), None);
        assert_eq!(Decision::from_letter(b'w'), None);
        assert_eq!(Decision::from_letter(b'r'), None);
        assert_eq!(Decision::from_letter(b'h'), None);
        assert_eq!(Decision::from_letter(b'?'), None);
    }

    #[test]
    fn decision_ioctl_values() {
        assert_eq!(Decision::Deny.to_ioctl(), ioctl::YOLO_DECISION_DENY);
        assert_eq!(Decision::Allow.to_ioctl(), ioctl::YOLO_DECISION_ALLOW);
    }

    #[test]
    fn ioctl_roundtrips() {
        for p in ALL {
            assert_eq!(Perm::from_ioctl(p.to_ioctl()), Some(p));
        }
        assert_eq!(Perm::from_ioctl(ioctl::YOLO_PERM_UNSET), None);
    }
}
