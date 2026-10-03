use serde::{Deserialize, Serialize};

pub mod e2ee;

pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_FRAME_BYTES: usize = 32768;
pub const HISTORY_LIMIT: usize = 1000;
pub const PAGE_SIZE: usize = 50;
pub const CATALOG_PAGE_SIZE: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub kind: ConversationKind,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub actions: Vec<Permission>,
}
pub type Conversation = Channel;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    #[default]
    Channel,
    Direct,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Profile,
    Contacts,
    Direct,
    Read,
    Send,
    Mention,
    ManageChannel,
    Users,
    Channels,
    Devices,
    Invitations,
    Roles,
}
pub const ALL_PERMISSIONS: &[Permission] = &[
    Permission::Profile,
    Permission::Contacts,
    Permission::Direct,
    Permission::Read,
    Permission::Send,
    Permission::Mention,
    Permission::ManageChannel,
    Permission::Users,
    Permission::Channels,
    Permission::Devices,
    Permission::Invitations,
    Permission::Roles,
];
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct User {
    pub id: i64,
    pub name: String,
    #[serde(default = "default_color")]
    pub color: String,
    pub disabled: bool,
    pub roles: Vec<i64>,
    pub actions: Vec<Permission>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Role {
    pub id: i64,
    pub name: String,
    pub allow: Vec<Permission>,
    pub deny: Vec<Permission>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelRule {
    pub role_id: i64,
    pub allow: Vec<Permission>,
    pub deny: Vec<Permission>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mention {
    pub user_id: i64,
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileRequest {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectRequest {
    pub user_id: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRequest {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    pub roles: Vec<i64>,
    #[serde(default)]
    pub disabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelRequest {
    pub name: String,
    #[serde(default)]
    pub closed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleRequest {
    pub name: String,
    pub allow: Vec<Permission>,
    pub deny: Vec<Permission>,
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
    #[serde(default = "default_color")]
    pub sender_color: String,
    pub text: String,
    pub created_at: i64,
    pub expires_at: i64,
    #[serde(default)]
    pub mentions: Vec<Mention>,
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
    #[serde(default)]
    pub mentions: Vec<Mention>,
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
        notify: bool,
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
    Profiles {
        users: Vec<User>,
    },
    StatePart {
        profile: Option<User>,
        contacts: Vec<User>,
        channels: Vec<Channel>,
        first: bool,
        last: bool,
    },
    State {
        profile: User,
        contacts: Vec<User>,
        channels: Vec<Channel>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    Ack { cursor: i64 },
}

pub fn default_color() -> String {
    "#808080".into()
}
pub fn valid_color(color: &str) -> bool {
    color.len() == 7
        && color.starts_with('#')
        && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
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
