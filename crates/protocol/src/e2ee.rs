//! JWE compact serialization (RFC 7516), direct A256GCM. No secret keys here.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{MAX_TEXT_BYTES, Mention};

pub const PREFIX: &str = "e2ee:";
pub const MAX_SEALED_BYTES: usize = 12288;
pub const MAX_MEMBERS: usize = 1000;

pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn decode(value: &str) -> Option<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    (encode(&bytes) == value).then_some(bytes)
}
pub fn digest(bytes: &[u8]) -> String {
    encode(&Sha256::digest(bytes))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub user_id: i64,
    pub device_id: i64,
}
pub fn members_digest(members: &[Member]) -> Option<String> {
    if members.is_empty() || members.len() > MAX_MEMBERS {
        return None;
    }
    let mut sorted = members.to_vec();
    sorted.sort();
    let mut bytes = b"Poknite members v1\0".to_vec();
    let mut devices = std::collections::HashSet::new();
    for m in sorted {
        if m.user_id <= 0 || m.device_id <= 0 || !devices.insert(m.device_id) {
            return None;
        }
        bytes.extend_from_slice(&m.user_id.to_be_bytes());
        bytes.extend_from_slice(&m.device_id.to_be_bytes());
    }
    Some(digest(&bytes))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub alg: String,
    pub enc: String,
    pub v: u8,
    pub kid: String,
    pub aud: String,
    pub cid: i64,
    pub sid: i64,
    pub did: i64,
    pub mid: String,
    pub members: String,
    pub mentions: Vec<Mention>,
}
impl Header {
    pub fn valid(&self) -> bool {
        self.alg == "dir"
            && self.enc == "A256GCM"
            && self.v == 1
            && decode(&self.kid).is_some_and(|b| b.len() == 32)
            && decode(&self.members).is_some_and(|b| b.len() == 32)
            && !self.aud.is_empty()
            && self.aud.len() <= 512
            && self.cid > 0
            && self.sid > 0
            && self.did > 0
            && self.mid.len() == 36
            && self.mentions.len() <= 32
            && self
                .mentions
                .iter()
                .all(|m| m.user_id > 0 && m.start < m.end && m.end <= MAX_TEXT_BYTES)
            && self.mentions.windows(2).all(|m| m[0].end <= m[1].start)
    }
}

pub struct Sealed {
    pub header: Header,
    /// The exact encoded protected header is the JWE associated data.
    pub protected: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub tag: Vec<u8>,
}
pub fn parse(text: &str) -> Option<Sealed> {
    if text.len() > MAX_SEALED_BYTES {
        return None;
    }
    let mut fields = text.strip_prefix(PREFIX)?.split('.');
    let protected = fields.next()?;
    if protected.len() > 8192 || !fields.next()?.is_empty() {
        return None;
    }
    let header: Header = serde_json::from_slice(&decode(protected)?).ok()?;
    let nonce = decode(fields.next()?)?;
    let ciphertext = decode(fields.next()?)?;
    let tag = decode(fields.next()?)?;
    if fields.next().is_some()
        || !header.valid()
        || nonce.len() != 12
        || ciphertext.is_empty()
        || ciphertext.len() > MAX_TEXT_BYTES
        || tag.len() != 16
    {
        return None;
    }
    Some(Sealed {
        header,
        protected: protected.into(),
        nonce,
        ciphertext,
        tag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn membership_is_order_independent_and_rejects_duplicate_devices() {
        let a = Member {
            user_id: 1,
            device_id: 3,
        };
        let b = Member {
            user_id: 2,
            device_id: 4,
        };
        assert_eq!(
            members_digest(&[a.clone(), b.clone()]),
            members_digest(&[b, a.clone()])
        );
        assert!(members_digest(&[a.clone(), a]).is_none());
        assert!(members_digest(&[]).is_none());
    }
    #[test]
    fn base64_is_canonical_and_bounded() {
        assert_eq!(decode("AA"), Some(vec![0]));
        assert!(decode("AB").is_none());
        assert!(decode("AA==").is_none());
        assert!(parse(&format!("{PREFIX}{}", "A".repeat(MAX_SEALED_BYTES))).is_none());
    }
}
