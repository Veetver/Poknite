//! Conversation keys are generated, imported and used only on the client.
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Result, bail, ensure};
use poknite_protocol::{
    Message, PublishRequest,
    e2ee::{self, Header, Member},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    Store,
    store::{protect, unprotect},
};

const CODE_PREFIX: &str = "poknite-key-v1:";

// Intentionally no Debug implementation: this structure contains a secret.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyCode {
    v: u8,
    aud: String,
    cid: i64,
    members: String,
    key: String,
}
impl KeyCode {
    fn bytes(&self) -> Result<Zeroizing<Vec<u8>>> {
        let bytes =
            e2ee::decode(&self.key).ok_or_else(|| anyhow::anyhow!("Повреждён ключ разговора"))?;
        ensure!(self.v == 1 && bytes.len() == 32, "Повреждён ключ разговора");
        Ok(Zeroizing::new(bytes))
    }
    fn kid(&self) -> Result<String> {
        Ok(e2ee::digest(&self.bytes()?))
    }
}

impl Store {
    pub fn create_conversation_key(
        &self,
        server: &str,
        channel: i64,
        members: &[Member],
    ) -> Result<String> {
        let mut key = Zeroizing::new([0u8; 32]);
        getrandom::fill(key.as_mut())
            .map_err(|_| anyhow::anyhow!("Недоступен генератор случайных чисел"))?;
        let code = KeyCode {
            v: 1,
            aud: server.into(),
            cid: channel,
            members: e2ee::members_digest(members)
                .ok_or_else(|| anyhow::anyhow!("Некорректный состав устройств"))?,
            key: e2ee::encode(key.as_ref()),
        };
        self.save_key(&code)?;
        self.unlock_history(server, channel)?;
        Ok(format!(
            "{CODE_PREFIX}{}",
            e2ee::encode(&serde_json::to_vec(&code)?)
        ))
    }
    pub fn import_conversation_key(
        &self,
        server: &str,
        channel: i64,
        members: &[Member],
        token: &str,
    ) -> Result<()> {
        ensure!(token.len() <= 2048, "Код ключа слишком длинный");
        let invalid = || anyhow::anyhow!("Неверный код ключа разговора");
        let raw = e2ee::decode(token.trim().strip_prefix(CODE_PREFIX).ok_or_else(invalid)?)
            .ok_or_else(invalid)?;
        let code: KeyCode = serde_json::from_slice(&raw).map_err(|_| invalid())?;
        ensure!(
            code.aud == server && code.cid == channel,
            "Ключ предназначен для другого сервера или разговора"
        );
        ensure!(
            Some(code.members.clone()) == e2ee::members_digest(members),
            "Состав устройств изменился. Создайте новый ключ на доверенном устройстве"
        );
        self.save_key(&code)?;
        self.unlock_history(server, channel)?;
        Ok(())
    }
    fn save_key(&self, code: &KeyCode) -> Result<()> {
        let kid = code.kid()?;
        let secret = protect(&serde_json::to_string(code)?)?;
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT kid FROM e2ee_active WHERE aud=? AND cid=?",
                params![code.aud, code.cid],
                |r| r.get(0),
            )
            .optional()?;
        let count: i64 = tx.query_row("SELECT count(*) FROM e2ee_keys", [], |r| r.get(0))?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM e2ee_keys WHERE aud=? AND cid=? AND kid=?)",
            params![code.aud, code.cid, kid],
            |r| r.get(0),
        )?;
        ensure!(
            exists || count < 1024,
            "Локальное хранилище ключей заполнено"
        );
        tx.execute("INSERT INTO e2ee_keys VALUES(?,?,?,?) ON CONFLICT(aud,cid,kid) DO UPDATE SET secret=excluded.secret",params![code.aud,code.cid,kid,secret])?;
        tx.execute("INSERT INTO e2ee_active VALUES(?,?,?) ON CONFLICT(aud,cid) DO UPDATE SET kid=excluded.kid",params![code.aud,code.cid,kid])?;
        if previous.as_deref() != Some(kid.as_str()) {
            tx.execute(
                "UPDATE drafts SET message_id=? WHERE channel_id=?",
                params![uuid::Uuid::new_v4().to_string(), code.cid],
            )?;
            tx.execute(
                "DELETE FROM e2ee_drafts WHERE aud=? AND cid=?",
                params![code.aud, code.cid],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    fn key(&self, server: &str, channel: i64, kid: Option<&str>) -> Result<KeyCode> {
        let c = self.connection.lock().unwrap();
        let raw: Option<String> = if let Some(kid) = kid {
            c.query_row(
                "SELECT secret FROM e2ee_keys WHERE aud=? AND cid=? AND kid=?",
                params![server, channel, kid],
                |r| r.get(0),
            )
            .optional()?
        } else {
            c.query_row("SELECT k.secret FROM e2ee_keys k JOIN e2ee_active a USING(aud,cid,kid) WHERE k.aud=? AND k.cid=?",params![server,channel],|r|r.get(0)).optional()?
        };
        let raw = raw.ok_or_else(|| {
            anyhow::anyhow!(
                "Нет ключа разговора. Откройте «Шифрование» и получите код на доверенном устройстве"
            )
        })?;
        let code: KeyCode = serde_json::from_str(&unprotect(&raw)?)
            .map_err(|_| anyhow::anyhow!("Повреждено локальное хранилище ключей"))?;
        ensure!(
            code.aud == server
                && code.cid == channel
                && kid.is_none_or(|k| code.kid().is_ok_and(|id| id == k)),
            "Повреждено локальное хранилище ключей"
        );
        Ok(code)
    }
    /// This code contains the secret. It is shown only on explicit user request.
    pub fn export_conversation_key(&self, server: &str, channel: i64) -> Result<String> {
        let code = self.key(server, channel, None)?;
        Ok(format!(
            "{CODE_PREFIX}{}",
            e2ee::encode(&serde_json::to_vec(&code)?)
        ))
    }
    pub fn key_fingerprint(&self, server: &str, channel: i64) -> Result<String> {
        self.key(server, channel, None)?.kid()
    }
    pub fn seal_publish(
        &self,
        server: &str,
        user: i64,
        device: i64,
        channel: i64,
        members: &[Member],
        request: &PublishRequest,
    ) -> Result<PublishRequest> {
        ensure!(
            poknite_protocol::valid_text(&request.text),
            "Нужен текст до 4096 байт"
        );
        ensure!(
            uuid::Uuid::parse_str(&request.client_message_id).is_ok(),
            "Некорректный идентификатор сообщения"
        );
        let source_hash = e2ee::digest(&serde_json::to_vec(request)?);
        // Persist the complete envelope before HTTP. Retrying must use the same nonce,
        // ciphertext and UUID even if the server response was lost.
        let cached: Option<(String, String, String)> = self
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT mid,source_hash,sealed FROM e2ee_drafts WHERE aud=? AND cid=?",
                params![server, channel],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((mid, hash, sealed)) = cached
            && mid == request.client_message_id
        {
            ensure!(
                hash == source_hash,
                "Идентификатор черновика уже использован"
            );
            return Ok(PublishRequest {
                client_message_id: mid,
                text: sealed,
                mentions: request.mentions.clone(),
            });
        }
        let code = self.key(server, channel, None)?;
        ensure!(
            Some(code.members.clone()) == e2ee::members_digest(members),
            "Состав устройств изменился. Смените ключ в разделе «Шифрование»"
        );
        ensure!(
            members
                .iter()
                .any(|m| m.user_id == user && m.device_id == device),
            "Ваше устройство отсутствует в разговоре"
        );
        let header = Header {
            alg: "dir".into(),
            enc: "A256GCM".into(),
            v: 1,
            kid: code.kid()?,
            aud: server.into(),
            cid: channel,
            sid: user,
            did: device,
            mid: request.client_message_id.clone(),
            members: code.members.clone(),
            mentions: request.mentions.clone(),
        };
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce)
            .map_err(|_| anyhow::anyhow!("Недоступен генератор случайных чисел"))?;
        let text = encrypt(&code.bytes()?, &header, &request.text, &nonce)?;
        self.connection.lock().unwrap().execute("INSERT INTO e2ee_drafts VALUES(?,?,?,?,?) ON CONFLICT(aud,cid) DO UPDATE SET mid=excluded.mid,source_hash=excluded.source_hash,sealed=excluded.sealed",params![server,channel,request.client_message_id,source_hash,text])?;
        Ok(PublishRequest {
            client_message_id: request.client_message_id.clone(),
            text,
            mentions: request.mentions.clone(),
        })
    }
    pub fn unseal_message(&self, server: &str, message: &Message) -> Result<Message> {
        let sealed = e2ee::parse(&message.text)
            .ok_or_else(|| anyhow::anyhow!("Сообщение не защищено E2EE или повреждено"))?;
        let h = &sealed.header;
        ensure!(
            h.aud == server
                && h.cid == message.channel_id
                && h.sid == message.sender_id
                && h.mentions == message.mentions,
            "Не совпадают защищённые данные сообщения"
        );
        ensure!(
            uuid::Uuid::parse_str(&h.mid).is_ok(),
            "Некорректный идентификатор сообщения"
        );
        let code = self.key(server, h.cid, Some(&h.kid))?;
        ensure!(
            code.members == h.members,
            "Не совпадает состав устройств ключа"
        );
        let cipher = Aes256Gcm::new_from_slice(&code.bytes()?)
            .map_err(|_| anyhow::anyhow!("Повреждён ключ"))?;
        let mut encrypted = sealed.ciphertext;
        encrypted.extend_from_slice(&sealed.tag);
        let plain = cipher
            .decrypt(
                Nonce::from_slice(&sealed.nonce),
                Payload {
                    msg: &encrypted,
                    aad: sealed.protected.as_bytes(),
                },
            )
            .map_err(|_| anyhow::anyhow!("Не удалось проверить подлинность сообщения"))?;
        let text = String::from_utf8(plain)
            .map_err(|_| anyhow::anyhow!("Некорректный текст сообщения"))?;
        ensure!(
            poknite_protocol::valid_text(&text),
            "Некорректный текст сообщения"
        );
        let chars = text.chars().collect::<Vec<_>>();
        ensure!(
            h.mentions
                .iter()
                .all(|m| m.end <= chars.len() && chars[m.start] == '@'),
            "Некорректные упоминания"
        );
        Ok(Message {
            id: format!("{}:{}", h.did, h.mid),
            text,
            ..message.clone()
        })
    }
    pub fn receive_secure(
        &self,
        server: &str,
        message: &Message,
        replay: bool,
        user: i64,
        notify: bool,
    ) -> Result<bool> {
        if let Ok(plain) = self.unseal_message(server, message) {
            return self.receive_inner(&plain, replay, user, notify, None);
        }
        let parsed = e2ee::parse(&message.text);
        let id = parsed
            .as_ref()
            .map(|s| format!("{}:{}", s.header.did, s.header.mid))
            .unwrap_or_else(|| message.id.clone());
        let placeholder = Message {
            id,
            text: "🔒 Сообщение недоступно: нужен ключ разговора или данные повреждены".into(),
            mentions: vec![],
            ..message.clone()
        };
        let wire = parsed.map(|_| serde_json::to_string(message)).transpose()?;
        self.receive_inner(&placeholder, replay, user, false, wire.as_deref())
    }
    fn unlock_history(&self, server: &str, channel: i64) -> Result<()> {
        let rows: Vec<(String, String)> = {
            let c = self.connection.lock().unwrap();
            let mut q = c.prepare("SELECT p.id,p.wire FROM e2ee_pending p JOIN messages m USING(id) WHERE m.channel_id=? LIMIT 1000")?;
            q.query_map([channel], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (id, wire) in rows {
            let wire: Message = serde_json::from_str(&wire)?;
            if let Ok(plain) = self.unseal_message(server, &wire) {
                let mut c = self.connection.lock().unwrap();
                let tx = c.transaction()?;
                tx.execute(
                    "UPDATE messages SET text=?,mentions=? WHERE id=?",
                    params![plain.text, serde_json::to_string(&plain.mentions)?, id],
                )?;
                tx.execute("DELETE FROM e2ee_pending WHERE id=?", [id])?;
                tx.commit()?;
            }
        }
        Ok(())
    }
}

fn encrypt(key: &[u8], header: &Header, text: &str, nonce: &[u8; 12]) -> Result<String> {
    ensure!(header.valid(), "Некорректные данные шифрования");
    let protected = e2ee::encode(&serde_json::to_vec(header)?);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow::anyhow!("Повреждён ключ"))?;
    let mut encrypted = cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: text.as_bytes(),
                aad: protected.as_bytes(),
            },
        )
        .map_err(|_| anyhow::anyhow!("Не удалось зашифровать сообщение"))?;
    let tag = encrypted.split_off(encrypted.len() - 16);
    let sealed = format!(
        "{}{}..{}.{}.{}",
        e2ee::PREFIX,
        protected,
        e2ee::encode(nonce),
        e2ee::encode(&encrypted),
        e2ee::encode(&tag)
    );
    if sealed.len() > e2ee::MAX_SEALED_BYTES {
        bail!("Зашифрованное сообщение слишком велико")
    }
    Ok(sealed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> serde_json::Value {
        serde_json::from_str(include_str!("../../../tools/fixtures/e2ee.json")).unwrap()
    }
    fn message(text: String) -> Message {
        Message {
            id: "server-id".into(),
            seq: 1,
            channel_id: 1,
            sender_id: 1,
            sender_name: "Автор".into(),
            sender_color: "#808080".into(),
            text,
            created_at: 1,
            expires_at: 2,
            mentions: vec![],
        }
    }
    fn members() -> Vec<Member> {
        vec![
            Member {
                user_id: 1,
                device_id: 1,
            },
            Member {
                user_id: 2,
                device_id: 2,
            },
        ]
    }
    #[test]
    fn independent_jwe_vector_matches_and_tampering_fails() {
        let f = fixture();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let key = e2ee::decode(f["key"].as_str().unwrap()).unwrap();
        let nonce: [u8; 12] = e2ee::decode(f["nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let header: Header = serde_json::from_value(f["header"].clone()).unwrap();
        assert_eq!(
            encrypt(&key, &header, f["text"].as_str().unwrap(), &nonce).unwrap(),
            f["sealed"].as_str().unwrap()
        );
        store
            .import_conversation_key(
                "https://example.test",
                1,
                &members(),
                f["code"].as_str().unwrap(),
            )
            .unwrap();
        let mut wire = message(f["sealed"].as_str().unwrap().into());
        wire.mentions = header.mentions.clone();
        assert_eq!(
            store
                .unseal_message("https://example.test", &wire)
                .unwrap()
                .text,
            f["text"].as_str().unwrap()
        );
        // Android JSONObject escapes slash characters. Verify both encodings
        // against their exact authenticated header rather than reserializing it.
        let android = Message {
            text: f["sealed_android"].as_str().unwrap().into(),
            ..wire.clone()
        };
        assert_eq!(
            store
                .unseal_message("https://example.test", &android)
                .unwrap()
                .text,
            f["text"].as_str().unwrap()
        );
        assert!(store.unseal_message("https://other.test", &wire).is_err());
        let mut changed = wire.clone();
        changed.channel_id = 2;
        assert!(
            store
                .unseal_message("https://example.test", &changed)
                .is_err()
        );
        let mut changed = wire.clone();
        changed.sender_id = 2;
        assert!(
            store
                .unseal_message("https://example.test", &changed)
                .is_err()
        );
        let mut changed = wire.clone();
        changed.mentions.clear();
        assert!(
            store
                .unseal_message("https://example.test", &changed)
                .is_err()
        );
        let mut parts = wire
            .text
            .strip_prefix(e2ee::PREFIX)
            .unwrap()
            .split('.')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut tag = e2ee::decode(&parts[4]).unwrap();
        tag[0] ^= 1;
        parts[4] = e2ee::encode(&tag);
        let changed = message(format!("{}{}", e2ee::PREFIX, parts.join(".")));
        assert!(
            store
                .unseal_message("https://example.test", &changed)
                .is_err()
        );
    }
    #[test]
    fn withheld_messages_survive_restart_and_unlock_without_duplicate_notifications() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let sender = Store::open(a.path()).unwrap();
        let receiver = Store::open(b.path()).unwrap();
        let code = sender
            .create_conversation_key("https://example.test", 1, &members())
            .unwrap();
        let request = PublishRequest {
            client_message_id: uuid::Uuid::new_v4().to_string(),
            text: "Секретное сообщение".into(),
            mentions: vec![],
        };
        let encrypted = sender
            .seal_publish("https://example.test", 1, 1, 1, &members(), &request)
            .unwrap();
        assert_eq!(
            encrypted.text,
            sender
                .seal_publish("https://example.test", 1, 1, 1, &members(), &request)
                .unwrap()
                .text
        );
        assert!(!encrypted.text.contains(&request.text));
        let wire = message(encrypted.text);
        assert!(
            receiver
                .receive_secure("https://example.test", &wire, false, 2, true)
                .unwrap()
        );
        assert!(receiver.pending().unwrap().is_empty());
        drop(receiver);
        let receiver = Store::open(b.path()).unwrap();
        receiver
            .import_conversation_key("https://example.test", 1, &members(), &code)
            .unwrap();
        assert_eq!(receiver.history(1, 0).unwrap()[0].text, request.text);
        assert!(
            !receiver
                .receive_secure(
                    "https://example.test",
                    &Message {
                        id: "replayed-as-another-id".into(),
                        seq: 2,
                        ..wire
                    },
                    false,
                    2,
                    true
                )
                .unwrap()
        );
        assert!(receiver.pending().unwrap().is_empty());
        let plain = message("Открытый текст с сервера".into());
        receiver
            .receive_secure("https://example.test", &plain, false, 2, true)
            .unwrap();
        assert!(
            !receiver
                .history(1, 0)
                .unwrap()
                .iter()
                .any(|m| m.text == plain.text)
        );
    }
    #[test]
    fn rotation_requires_new_key_and_removed_member_cannot_decrypt_future_messages() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let sender = Store::open(a.path()).unwrap();
        let former = Store::open(b.path()).unwrap();
        let old = sender
            .create_conversation_key("https://example.test", 1, &members())
            .unwrap();
        former
            .import_conversation_key("https://example.test", 1, &members(), &old)
            .unwrap();
        let remaining = vec![Member {
            user_id: 1,
            device_id: 1,
        }];
        let request = PublishRequest {
            client_message_id: uuid::Uuid::new_v4().to_string(),
            text: "После исключения".into(),
            mentions: vec![],
        };
        assert!(
            sender
                .seal_publish("https://example.test", 1, 1, 1, &remaining, &request)
                .is_err()
        );
        assert!(
            sender
                .import_conversation_key("https://example.test", 1, &remaining, &old)
                .is_err()
        );
        let new = sender
            .create_conversation_key("https://example.test", 1, &remaining)
            .unwrap();
        assert_ne!(old, new);
        let encrypted = sender
            .seal_publish("https://example.test", 1, 1, 1, &remaining, &request)
            .unwrap();
        let wire = message(encrypted.text);
        assert!(
            former
                .unseal_message("https://example.test", &wire)
                .is_err()
        );
        assert_eq!(
            sender
                .unseal_message("https://example.test", &wire)
                .unwrap()
                .text,
            request.text
        );
        let mut changed = request;
        changed.text = "Другой текст с тем же UUID".into();
        assert!(
            sender
                .seal_publish("https://example.test", 1, 1, 1, &remaining, &changed)
                .is_err()
        );
    }
}
