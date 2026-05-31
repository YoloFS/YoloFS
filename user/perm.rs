// yolo CLI — perm.rs
//
// The `Perm` permission type — the single verdict type shared across the
// codebase: config rules carry it, the kernel ioctl ABI encodes it, the
// daemon's ask `decision` is one, and the journal records it. Its only
// dependency is the ioctl perm constants.

use crate::ioctl;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Perm {
    Ask,
    Allow,
    Read,
    Deny,
    Hide,
}

impl Perm {
    pub fn to_ioctl(self) -> u8 {
        match self {
            Perm::Ask => ioctl::YOLO_PERM_ASK,
            Perm::Allow => ioctl::YOLO_PERM_ALLOW,
            Perm::Read => ioctl::YOLO_PERM_READ,
            Perm::Deny => ioctl::YOLO_PERM_DENY,
            Perm::Hide => ioctl::YOLO_PERM_HIDE,
        }
    }

    /// Inverse of [`to_ioctl`]. `None` for `UNSET` or any unknown value.
    pub fn from_ioctl(v: u8) -> Option<Self> {
        match v {
            ioctl::YOLO_PERM_ASK => Some(Perm::Ask),
            ioctl::YOLO_PERM_ALLOW => Some(Perm::Allow),
            ioctl::YOLO_PERM_READ => Some(Perm::Read),
            ioctl::YOLO_PERM_DENY => Some(Perm::Deny),
            ioctl::YOLO_PERM_HIDE => Some(Perm::Hide),
            _ => None,
        }
    }

    /// The journal's single-letter code for this perm (matches the kernel's
    /// `perm_char`). `allow` is `y` ("yes") since `ask` takes `a`.
    pub fn to_letter(self) -> char {
        match self {
            Perm::Ask => 'a',
            Perm::Allow => 'y',
            Perm::Read => 'r',
            Perm::Deny => 'd',
            Perm::Hide => 'h',
        }
    }

    /// Inverse of [`to_letter`].
    pub fn from_letter(b: u8) -> Option<Self> {
        match b {
            b'a' => Some(Perm::Ask),
            b'y' => Some(Perm::Allow),
            b'r' => Some(Perm::Read),
            b'd' => Some(Perm::Deny),
            b'h' => Some(Perm::Hide),
            _ => None,
        }
    }
}

impl fmt::Display for Perm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Perm::Ask => "ask",
            Perm::Allow => "allow",
            Perm::Read => "read",
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
            "read" => Ok(Perm::Read),
            "deny" => Ok(Perm::Deny),
            "hide" => Ok(Perm::Hide),
            _ => anyhow::bail!("unknown permission: {s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Perm; 5] = [Perm::Ask, Perm::Allow, Perm::Read, Perm::Deny, Perm::Hide];

    #[test]
    fn letter_roundtrips() {
        for p in ALL {
            assert_eq!(Perm::from_letter(p.to_letter() as u8), Some(p));
        }
        // `allow` is `y` (not `a`, which `ask` takes).
        assert_eq!(Perm::Allow.to_letter(), 'y');
        assert_eq!(Perm::Ask.to_letter(), 'a');
        assert_eq!(Perm::from_letter(b'?'), None);
    }

    #[test]
    fn ioctl_roundtrips() {
        for p in ALL {
            assert_eq!(Perm::from_ioctl(p.to_ioctl()), Some(p));
        }
        assert_eq!(Perm::from_ioctl(ioctl::YOLO_PERM_UNSET), None);
    }
}
