use serde::{Deserialize, Serialize};

pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_FRAME_BYTES: usize = 32768;
pub const HISTORY_LIMIT: usize = 1000;
pub const PAGE_SIZE: usize = 50;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Channel {
    pub id: i64,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub current: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub seq: i64,
    pub channel_id: i64,
    pub sender_id: i64,
    pub sender_name: String,
    pub text: String,
    pub created_at: i64,
    pub expires_at: i64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct EnrollRequest {
    pub invitation: String,
    pub device_name: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct EnrollResponse {
    pub token: String,
    pub device_id: i64,
    pub user_id: i64,
    pub user_name: String,
    pub retention_seconds: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishRequest {
    pub client_message_id: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Hello {
        user_id: i64,
        device_id: i64,
        channels: Vec<Channel>,
    },
    Message {
        message: Message,
        replay: bool,
    },
    Progress {
        cursor: i64,
    },
    Synced {
        cursor: i64,
        gap: bool,
    },
    Reset {
        cursor: i64,
    },
    Channels {
        channels: Vec<Channel>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    Ack { cursor: i64 },
}

pub fn valid_text(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= MAX_TEXT_BYTES
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
}
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 64 && !name.chars().any(char::is_control)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_limit_counts_utf8_bytes() {
        assert!(valid_text(&"я".repeat(2048)));
        assert!(!valid_text(&"я".repeat(2049)));
        assert!(!valid_text("  \n"));
        assert!(!valid_text("a\0"));
    }
    #[test]
    fn wire_format_is_shared_with_android() {
        let event: ClientEvent = serde_json::from_str(r#"{"type":"ack","cursor":42}"#).unwrap();
        assert!(matches!(event, ClientEvent::Ack { cursor: 42 }));
    }
}
