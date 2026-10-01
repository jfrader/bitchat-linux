use anyhow::{Result, bail};

pub const APP_NAME: &str = "bitchat-linux";
pub const HISTORY_LIMIT: usize = 500;
pub const MAX_TEXT_BYTES: usize = bitchatd::MAX_TEXT_BYTES;
pub const CHANNEL_CAPACITY: usize = 128;
pub const MAX_GEOHASH_LENGTH: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Room {
    Mesh,
    Internet(String),
}

impl Room {
    pub fn label(&self) -> String {
        match self {
            Self::Mesh => "#mesh".into(),
            Self::Internet(geohash) => format!("#{geohash}"),
        }
    }
}

pub fn send_failure_notice(room: &Room, text: &str, reason: &str) -> String {
    let label = |value: &str| bitchatd::clean_message(value).replace(['\n', '\t'], " ");
    format!(
        "Send failed in {}: {} — {}",
        label(&room.label()),
        label(reason),
        label(text)
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: String,
    pub room: Room,
    pub author: String,
    pub nickname: String,
    pub text: String,
    pub timestamp_ms: u64,
    pub mine: bool,
}

#[derive(Clone, Debug)]
pub enum Update {
    Message(Message),
    InternetStatus {
        geohash: Option<String>,
        detail: String,
        connected: usize,
    },
    Notice(String),
    SendFailed {
        room: Room,
        text: String,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Send {
        room: Room,
        text: String,
        nickname: String,
    },
    Join(String),
    Nickname(String),
    Radio(bitchatd::Mode),
    Clear(Room),
    Quit,
}

pub fn parse_geohash(input: &str) -> Result<String> {
    let code = input.trim().trim_start_matches('#').to_ascii_lowercase();
    if code.is_empty() || code.len() > MAX_GEOHASH_LENGTH {
        bail!("geohash must contain 1–{MAX_GEOHASH_LENGTH} characters");
    }
    geohash::decode(&code).map_err(|_| anyhow::anyhow!("invalid geohash: {code}"))?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_geohashes_without_inventing_a_location() {
        assert_eq!(parse_geohash("#DR5RS").unwrap(), "dr5rs");
        assert!(parse_geohash("").is_err());
        assert!(parse_geohash("invalid").is_err());
        assert!(parse_geohash("u".repeat(MAX_GEOHASH_LENGTH + 1).as_str()).is_err());
    }
}
