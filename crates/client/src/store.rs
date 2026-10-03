use anyhow::{Result, bail};
use poknite_protocol::{Channel, HISTORY_LIMIT, Mention, Message, PAGE_SIZE, User};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub server: String,
    pub token: String,
    pub device_id: i64,
    pub user_id: i64,
    pub user_name: String,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub management_server: String,
    #[serde(default)]
    pub management_ip: Option<std::net::IpAddr>,
}
impl Profile {
    pub fn save(&self, directory: &Path) -> Result<()> {
        secure_directory(directory)?;
        let mut profile = self.clone();
        profile.token = protect(&profile.token)?;
        let temporary = directory.join("profile.tmp");
        write_private(&temporary, &serde_json::to_vec(&profile)?)?;
        let destination = directory.join("profile.json");
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            };
            let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = destination
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        #[cfg(not(windows))]
        std::fs::rename(temporary, destination)?;
        Ok(())
    }
    pub fn load(directory: &Path) -> Result<Option<Self>> {
        let file = directory.join("profile.json");
        if !file.exists() {
            return Ok(None);
        }
        let mut profile: Self = serde_json::from_slice(&std::fs::read(file)?)?;
        profile.token = unprotect(&profile.token)?;
        Ok(Some(profile))
    }
}
pub fn default_directory() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into()))
            .join("Poknite")
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
                    .join(".local/share")
            })
            .join("poknite")
    }
}
fn secure_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
#[cfg(not(windows))]
pub(crate) fn protect(value: &str) -> Result<String> {
    Ok(value.into())
}
#[cfg(not(windows))]
pub(crate) fn unprotect(value: &str) -> Result<String> {
    Ok(value.into())
}
#[cfg(windows)]
fn crypt(value: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: value.len() as u32,
        pbData: value.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        let ok = if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            bail!("Не удалось защитить учётные данные Windows")
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut _);
        Ok(bytes)
    }
}
#[cfg(windows)]
pub(crate) fn protect(value: &str) -> Result<String> {
    Ok(crypt(value.as_bytes(), true)?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
#[cfg(windows)]
pub(crate) fn unprotect(value: &str) -> Result<String> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("Повреждены учётные данные")
    };
    let bytes = (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(String::from_utf8(crypt(&bytes, false)?)?)
}

pub struct Store {
    pub(crate) connection: Mutex<Connection>,
}
impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        secure_directory(directory)?;
        let path = directory.join("history.db");
        let mut connection = Connection::open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        connection.execute_batch("PRAGMA page_size=4096; PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA max_page_count=2048; PRAGMA cache_size=-256; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY,seq INTEGER NOT NULL,channel_id INTEGER NOT NULL,sender_id INTEGER NOT NULL,sender_name TEXT NOT NULL,text TEXT NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,pending INTEGER NOT NULL DEFAULT 0,replay INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS popups(message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,notification_id INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS history_channel ON messages(channel_id,created_at DESC);
            CREATE TABLE IF NOT EXISTS channels(id INTEGER PRIMARY KEY,name TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS muted(channel_id INTEGER PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS drafts(channel_id INTEGER PRIMARY KEY,text TEXT NOT NULL,message_id TEXT NOT NULL); UPDATE messages SET pending=1 WHERE pending=2;")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 3 {
            bail!("Локальная база создана более новой версией Poknite");
        }
        if version < 3 {
            let tx = connection.transaction()?;
            if version < 2 {
                tx.execute_batch("ALTER TABLE messages ADD COLUMN mentions TEXT NOT NULL DEFAULT '[]';ALTER TABLE channels ADD COLUMN details TEXT NOT NULL DEFAULT '{}';ALTER TABLE drafts ADD COLUMN mentions TEXT NOT NULL DEFAULT '[]';CREATE TABLE users(id INTEGER PRIMARY KEY,details TEXT NOT NULL);PRAGMA user_version=2;")?;
            }
            tx.execute_batch("ALTER TABLE messages ADD COLUMN sender_color TEXT NOT NULL DEFAULT '#808080';PRAGMA user_version=3;")?;
            tx.commit()?;
        }
        connection.execute_batch("CREATE TABLE IF NOT EXISTS cancelled_popups(key TEXT PRIMARY KEY,system_id INTEGER NOT NULL)")?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS e2ee_keys(aud TEXT NOT NULL,cid INTEGER NOT NULL,kid TEXT NOT NULL,secret TEXT NOT NULL,PRIMARY KEY(aud,cid,kid));
            CREATE TABLE IF NOT EXISTS e2ee_active(aud TEXT NOT NULL,cid INTEGER NOT NULL,kid TEXT NOT NULL,PRIMARY KEY(aud,cid));
            CREATE TABLE IF NOT EXISTS e2ee_drafts(aud TEXT NOT NULL,cid INTEGER NOT NULL,mid TEXT NOT NULL,source_hash TEXT NOT NULL,sealed TEXT NOT NULL,PRIMARY KEY(aud,cid));
            CREATE TABLE IF NOT EXISTS e2ee_pending(id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,wire TEXT NOT NULL);")?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn selected_channel(&self) -> Result<i64> {
        Ok(self
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM meta WHERE key='selected_channel'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
    pub fn select_channel(&self, id: i64) -> Result<()> {
        anyhow::ensure!(id >= 0, "Недопустимый разговор");
        self.connection.lock().unwrap().execute("INSERT INTO meta VALUES('selected_channel',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[id])?;
        Ok(())
    }
    pub fn restore_channel(&self, channels: &[Channel]) -> Result<i64> {
        let saved = self.selected_channel()?;
        let selected = channels
            .iter()
            .find(|c| c.id == saved)
            .or_else(|| channels.first())
            .map(|c| c.id)
            .unwrap_or(0);
        if selected > 0 {
            self.select_channel(selected)?;
        }
        Ok(selected)
    }
    pub fn cursor(&self) -> Result<i64> {
        Ok(self
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT value FROM meta WHERE key='cursor'", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0))
    }
    pub fn set_cursor(&self, cursor: i64) -> Result<()> {
        if cursor < 0 {
            bail!("Недопустимая позиция потока")
        }
        self.connection.lock().unwrap().execute("INSERT INTO meta VALUES('cursor',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[cursor])?;
        Ok(())
    }
    pub fn receive(&self, message: &Message, replay: bool, user: i64) -> Result<bool> {
        self.receive_delivery(message, replay, user, false)
    }
    pub fn receive_delivery(
        &self,
        message: &Message,
        replay: bool,
        user: i64,
        notify: bool,
    ) -> Result<bool> {
        self.receive_inner(message, replay, user, notify, None)
    }
    pub(crate) fn receive_inner(
        &self,
        message: &Message,
        replay: bool,
        user: i64,
        notify: bool,
        sealed: Option<&str>,
    ) -> Result<bool> {
        if !poknite_protocol::valid_text(&message.text) || message.seq < 1 {
            bail!("Некорректное сообщение сервера")
        }
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let pending = notify && message.sender_id != user;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO messages VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                message.id,
                message.seq,
                message.channel_id,
                message.sender_id,
                message.sender_name,
                message.text,
                message.created_at,
                message.expires_at,
                pending,
                replay,
                serde_json::to_string(&message.mentions)?,
                message.sender_color
            ],
        )? > 0;
        if inserted && let Some(wire) = sealed {
            tx.execute(
                "INSERT INTO e2ee_pending(id,wire) VALUES(?,?)",
                params![message.id, wire],
            )?;
        }
        tx.execute("DELETE FROM messages WHERE rowid IN (SELECT rowid FROM messages ORDER BY created_at DESC,rowid DESC LIMIT -1 OFFSET ?)",[HISTORY_LIMIT as i64])?;
        tx.commit()?;
        Ok(inserted)
    }
    pub fn history(&self, channel: i64, offset: usize) -> Result<Vec<Message>> {
        let c = self.connection.lock().unwrap();
        let mut s=c.prepare("SELECT id,seq,channel_id,sender_id,sender_name,text,created_at,expires_at,mentions,sender_color FROM messages WHERE channel_id=? ORDER BY created_at DESC,rowid DESC LIMIT ? OFFSET ?")?;
        Ok(s.query_map(
            params![channel, PAGE_SIZE as i64, offset as i64],
            message_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn pending(&self) -> Result<Vec<(Message, bool)>> {
        let c = self.connection.lock().unwrap();
        let mut s=c.prepare("SELECT id,seq,channel_id,sender_id,sender_name,text,created_at,expires_at,mentions,sender_color,replay FROM messages WHERE pending=1 ORDER BY created_at,rowid LIMIT 1000")?;
        Ok(s.query_map([], |r| Ok((message_row(r)?, r.get(10)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn take_pending(&self) -> Result<Vec<(Message, bool)>> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let messages = {
            let mut s=tx.prepare("SELECT id,seq,channel_id,sender_id,sender_name,text,created_at,expires_at,mentions,sender_color,replay FROM messages WHERE pending=1 ORDER BY created_at,rowid LIMIT 1000")?;
            s.query_map([], |r| Ok((message_row(r)?, r.get(10)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.execute("UPDATE messages SET pending=2 WHERE pending=1", [])?;
        tx.commit()?;
        Ok(messages)
    }
    pub fn notification_failed(&self, ids: &[String]) -> Result<()> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE messages SET pending=1 WHERE id=? AND pending=2",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn popup_id(&self, summary: bool, message_id: &str) -> Result<u32> {
        let c = self.connection.lock().unwrap();
        Ok(if summary {
            c.query_row(
                "SELECT value FROM meta WHERE key='popup_summary'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
        } else {
            c.query_row(
                "SELECT notification_id FROM popups WHERE message_id=?",
                [message_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
        })
    }
    pub fn set_popup_id(&self, summary: bool, message_id: &str, id: u32) -> Result<()> {
        let c = self.connection.lock().unwrap();
        if summary {
            c.execute("INSERT INTO meta VALUES('popup_summary',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[id])?;
        } else {
            c.execute("INSERT INTO popups SELECT id,? FROM messages WHERE id=? ON CONFLICT(message_id) DO UPDATE SET notification_id=excluded.notification_id",params![id,message_id])?;
        }
        Ok(())
    }
    pub fn notified(&self, ids: &[String]) -> Result<()> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        for id in ids {
            tx.execute("UPDATE messages SET pending=0 WHERE id=?", [id])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn drain_cancelled(&self) -> Result<Vec<(String, u32)>> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let rows = {
            let mut q = tx.prepare("SELECT key,system_id FROM cancelled_popups LIMIT 1001")?;
            q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.execute("DELETE FROM cancelled_popups", [])?;
        tx.commit()?;
        Ok(rows)
    }
    pub fn process_notifications(
        &self,
        notify: impl FnOnce(&[(Message, bool)], u32, bool) -> Result<u32>,
    ) -> Result<()> {
        // Keep the same lock as permission snapshots through OS delivery. A revocation
        // either cancels this batch before display or removes the displayed popup next.
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let messages = {
            let mut q=tx.prepare("SELECT id,seq,channel_id,sender_id,sender_name,text,created_at,expires_at,mentions,sender_color,replay FROM messages WHERE pending=1 ORDER BY created_at,rowid LIMIT 1000")?;
            q.query_map([], |r| Ok((message_row(r)?, r.get(10)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if messages.is_empty() {
            return Ok(());
        }
        let summary = messages.len() > 1 || messages.iter().any(|(_, replay)| *replay);
        let key = &messages[0].0.id;
        let replace = if summary {
            tx.query_row(
                "SELECT value FROM meta WHERE key='popup_summary'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
        } else {
            tx.query_row(
                "SELECT notification_id FROM popups WHERE message_id=?",
                [key],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
        };
        let mut quiet = true;
        for (m, _) in &messages {
            quiet &= tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM muted WHERE channel_id=?)",
                [m.channel_id],
                |r| r.get::<_, bool>(0),
            )?;
        }
        let id = notify(&messages, replace, quiet)?;
        if summary {
            tx.execute("INSERT INTO meta VALUES('popup_summary',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[id])?;
        } else {
            tx.execute("INSERT INTO popups VALUES(?,?) ON CONFLICT(message_id) DO UPDATE SET notification_id=excluded.notification_id",params![key,id])?;
        }
        for (message, _) in &messages {
            tx.execute("UPDATE messages SET pending=0 WHERE id=?", [&message.id])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn clear(&self) -> Result<()> {
        let c = self.connection.lock().unwrap();
        c.execute_batch("DELETE FROM messages;PRAGMA incremental_vacuum(2048);")?;
        Ok(())
    }
    pub fn reset_account(&self) -> Result<()> {
        self.connection.lock().unwrap().execute_batch("DELETE FROM messages;DELETE FROM channels;DELETE FROM muted;DELETE FROM drafts;DELETE FROM users;DELETE FROM cancelled_popups;DELETE FROM meta;PRAGMA incremental_vacuum(2048);")?;
        Ok(())
    }
    pub fn set_channels(&self, channels: &[Channel]) -> Result<()> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let former = {
            let mut q = tx.prepare("SELECT id FROM channels")?;
            q.query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let lost = former
            .into_iter()
            .filter(|id| {
                !channels.iter().any(|ch| {
                    ch.id == *id && ch.actions.contains(&poknite_protocol::Permission::Read)
                })
            })
            .collect::<Vec<_>>();
        for id in &lost {
            tx.execute("INSERT OR IGNORE INTO cancelled_popups SELECT m.id,COALESCE(p.notification_id,0) FROM messages m LEFT JOIN popups p ON p.message_id=m.id WHERE m.channel_id=?",[id])?;
        }
        if !lost.is_empty() {
            tx.execute("INSERT OR IGNORE INTO cancelled_popups VALUES('summary',COALESCE((SELECT value FROM meta WHERE key='popup_summary'),0))",[])?;
        }
        tx.execute("DELETE FROM channels", [])?;
        // Rights changes cancel unsent notices, while history and drafts remain on disk.
        for ch in channels {
            if !ch.actions.contains(&poknite_protocol::Permission::Read) {
                tx.execute("UPDATE messages SET pending=0 WHERE channel_id=?", [ch.id])?;
            }
        }

        for ch in channels {
            tx.execute(
                "INSERT INTO channels VALUES(?,?,?)",
                params![ch.id, ch.name, serde_json::to_string(ch)?],
            )?;
        }
        tx.execute(
            "UPDATE messages SET pending=0 WHERE channel_id NOT IN(SELECT id FROM channels)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn channels(&self) -> Result<Vec<Channel>> {
        let c = self.connection.lock().unwrap();
        let mut s = c.prepare("SELECT id,name,details FROM channels ORDER BY id")?;
        Ok(s.query_map([], |r| {
            let details: String = r.get(2)?;
            Ok(
                serde_json::from_str::<Channel>(&details).unwrap_or(Channel {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: Default::default(),
                    closed: false,
                    actions: Vec::new(),
                }),
            )
        })?
        .collect::<rusqlite::Result<_>>()?)
    }
    pub fn muted(&self, channel: i64) -> Result<bool> {
        Ok(self.connection.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM muted WHERE channel_id=?)",
            [channel],
            |r| r.get(0),
        )?)
    }
    pub fn set_muted(&self, channel: i64, value: bool) -> Result<()> {
        let c = self.connection.lock().unwrap();
        if value {
            c.execute("INSERT OR IGNORE INTO muted VALUES(?)", [channel])?;
        } else {
            c.execute("DELETE FROM muted WHERE channel_id=?", [channel])?;
        }
        Ok(())
    }
    pub fn draft(&self, channel: i64) -> Result<(String, String)> {
        Ok(self
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT text,message_id FROM drafts WHERE channel_id=?",
                [channel],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or_else(|| ("".into(), uuid::Uuid::new_v4().to_string())))
    }
    pub fn draft_mentions(&self, channel: i64) -> Result<Vec<Mention>> {
        let c = self.connection.lock().unwrap();
        let value: Option<String> = c
            .query_row(
                "SELECT mentions FROM drafts WHERE channel_id=?",
                [channel],
                |r| r.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(value.as_deref().unwrap_or("[]"))?)
    }
    pub fn save_draft(&self, channel: i64, text: &str) -> Result<String> {
        let (old, _) = self.draft(channel)?;
        let mentions = adjust_mentions(&old, text, &self.draft_mentions(channel)?);
        self.save_draft_with_mentions(channel, text, &mentions)
    }
    pub fn save_draft_with_mentions(
        &self,
        channel: i64,
        text: &str,
        mentions: &[Mention],
    ) -> Result<String> {
        if text.len() > poknite_protocol::MAX_TEXT_BYTES {
            bail!("Текст превышает 4096 байт");
        }
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let old: Option<(String, String, String)> = tx
            .query_row(
                "SELECT text,message_id,mentions FROM drafts WHERE channel_id=?",
                [channel],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let encoded = serde_json::to_string(mentions)?;
        let id = old
            .filter(|(previous, _, m)| previous == text && m == &encoded)
            .map(|(_, id, _)| id)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        tx.execute("INSERT INTO drafts VALUES(?,?,?,?) ON CONFLICT(channel_id) DO UPDATE SET text=excluded.text,message_id=excluded.message_id,mentions=excluded.mentions",params![channel,text,id,encoded])?;
        tx.commit()?;
        Ok(id)
    }
    pub fn update_users(&self, users: &[User]) -> Result<()> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        for user in users {
            tx.execute("INSERT INTO users VALUES(?,?) ON CONFLICT(id) DO UPDATE SET details=excluded.details",params![user.id,serde_json::to_string(user)?])?;
            tx.execute(
                "UPDATE messages SET sender_name=?,sender_color=? WHERE sender_id=?",
                params![user.name, user.color, user.id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn clear_draft_if(&self, channel: i64, message_id: &str) -> Result<()> {
        self.connection.lock().unwrap().execute(
            "DELETE FROM drafts WHERE channel_id=? AND message_id=?",
            params![channel, message_id],
        )?;
        Ok(())
    }
}
fn message_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        seq: r.get(1)?,
        channel_id: r.get(2)?,
        sender_id: r.get(3)?,
        sender_name: r.get(4)?,
        sender_color: r.get(9)?,
        text: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
        mentions: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
    })
}

pub fn adjust_mentions(old: &str, new: &str, mentions: &[Mention]) -> Vec<Mention> {
    let a = old.chars().collect::<Vec<_>>();
    let b = new.chars().collect::<Vec<_>>();
    let prefix = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_end = a.len() - suffix;
    let delta = b.len() as isize - a.len() as isize;
    mentions
        .iter()
        .filter_map(|m| {
            if m.end <= prefix {
                Some(m.clone())
            } else if m.start >= old_end {
                Some(Mention {
                    user_id: m.user_id,
                    start: (m.start as isize + delta) as usize,
                    end: (m.end as isize + delta) as usize,
                })
            } else {
                None
            }
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> (Store, PathBuf) {
        let p = std::env::temp_dir().join(format!("poknite-test-{}", uuid::Uuid::new_v4()));
        (Store::open(&p).unwrap(), p)
    }
    fn message(n: i64) -> Message {
        Message {
            id: format!("id-{n}"),
            seq: n,
            channel_id: 1,
            sender_id: 2,
            sender_name: "Тест".into(),
            text: "я".repeat(2048),
            created_at: n,
            expires_at: n + 1,
            mentions: Vec::new(),
            sender_color: poknite_protocol::default_color(),
        }
    }
    #[test]
    fn durable_pending_deduplicates_and_retains_expired_copy() {
        let (s, p) = store();
        assert!(s.receive_delivery(&message(1), true, 1, true).unwrap());
        assert!(!s.receive_delivery(&message(1), false, 1, true).unwrap());
        s.set_cursor(1).unwrap();
        drop(s);
        let s = Store::open(&p).unwrap();
        assert_eq!(s.cursor().unwrap(), 1);
        assert_eq!(s.pending().unwrap().len(), 1);
        assert_eq!(s.history(1, 0).unwrap().len(), 1);
        s.notified(&["id-1".into()]).unwrap();
        assert!(s.pending().unwrap().is_empty());
        drop(s);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn own_sender_is_silent_and_muted_messages_still_queue() {
        let (s, p) = store();
        s.receive_delivery(&message(1), false, 2, true).unwrap();
        s.set_muted(1, true).unwrap();
        s.receive_delivery(&message(2), false, 1, true).unwrap();
        assert_eq!(s.pending().unwrap().len(), 1);
        assert_eq!(s.history(1, 0).unwrap().len(), 2);
        assert!(s.muted(1).unwrap());
        drop(s);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn notification_claim_is_exclusive_and_recovers_after_crash() {
        let (s, p) = store();
        s.receive_delivery(&message(1), false, 1, true).unwrap();
        let batch = s.take_pending().unwrap();
        assert_eq!(batch.len(), 1);
        assert!(s.take_pending().unwrap().is_empty());
        drop(s);
        let s = Store::open(&p).unwrap();
        assert_eq!(s.take_pending().unwrap().len(), 1);
        s.notification_failed(&["id-1".into()]).unwrap();
        assert_eq!(s.take_pending().unwrap().len(), 1);
        s.notified(&["id-1".into()]).unwrap();
        assert!(s.take_pending().unwrap().is_empty());
        drop(s);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn quota_and_paging() {
        let (s, p) = store();
        for n in 1..=1005 {
            s.receive_delivery(&message(n), false, 1, true).unwrap();
        }
        assert_eq!(s.pending().unwrap().len(), 1000);
        assert_eq!(s.history(1, 0).unwrap().len(), PAGE_SIZE);
        assert_eq!(s.history(1, 1000).unwrap().len(), 0);
        assert!(std::fs::metadata(p.join("history.db")).unwrap().len() <= 8 * 1024 * 1024);
        drop(s);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn manual_draft_keeps_id_until_edit() {
        let (s, p) = store();
        let a = s.save_draft(1, "Привет").unwrap();
        assert_eq!(a, s.save_draft(1, "Привет").unwrap());
        let b = s.save_draft(1, "Пока").unwrap();
        assert_ne!(a, b);
        s.clear_draft_if(1, &a).unwrap();
        assert_eq!(s.draft(1).unwrap().0, "Пока");
        drop(s);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn selected_conversation_survives_reopen_and_tracks_access_by_id() {
        let (s, p) = store();
        let mut channels = vec![
            Channel {
                id: 1,
                name: "Общий".into(),
                kind: Default::default(),
                closed: false,
                actions: vec![poknite_protocol::Permission::Read],
            },
            Channel {
                id: 7,
                name: "Рабочий".into(),
                kind: Default::default(),
                closed: false,
                actions: vec![poknite_protocol::Permission::Read],
            },
        ];
        assert_eq!(s.restore_channel(&channels).unwrap(), 1);
        s.select_channel(7).unwrap();
        s.save_draft(1, "Первый черновик").unwrap();
        s.save_draft(7, "Второй черновик").unwrap();
        drop(s);
        let s = Store::open(&p).unwrap();
        channels[1].name = "Переименован".into();
        channels[1].closed = true;
        assert_eq!(s.restore_channel(&channels).unwrap(), 7);
        assert_eq!(s.draft(7).unwrap().0, "Второй черновик");
        assert_eq!(s.draft(1).unwrap().0, "Первый черновик");
        s.clear().unwrap();
        assert_eq!(s.selected_channel().unwrap(), 7);
        assert_eq!(s.restore_channel(&[]).unwrap(), 0);
        assert_eq!(s.selected_channel().unwrap(), 7);
        assert_eq!(s.restore_channel(&channels).unwrap(), 7);
        assert_eq!(s.restore_channel(&channels[..1]).unwrap(), 1);
        assert_eq!(s.selected_channel().unwrap(), 1);
        assert!(s.select_channel(-1).is_err());
        s.reset_account().unwrap();
        drop(s);
        let s = Store::open(&p).unwrap();
        assert_eq!(s.selected_channel().unwrap(), 0);
        assert!(s.draft(1).unwrap().0.is_empty());
        drop(s);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn mentions_use_codepoints_and_survive_edits_outside_the_range() {
        let mention = Mention {
            user_id: 2,
            start: 2,
            end: 6,
        };
        assert_eq!(
            adjust_mentions("🙂 @Аня!", "x🙂 @Аня!", std::slice::from_ref(&mention)),
            vec![Mention {
                user_id: 2,
                start: 3,
                end: 7
            }]
        );
        assert!(adjust_mentions("🙂 @Аня!", "🙂 @Ася!", &[mention]).is_empty());
    }
    #[test]
    fn server_notify_controls_replay_and_revocation_cancels_without_erasing_history() {
        let (s, p) = store();
        s.set_channels(&[Channel {
            id: 1,
            name: "Общий".into(),
            kind: Default::default(),
            closed: false,
            actions: vec![poknite_protocol::Permission::Read],
        }])
        .unwrap();
        s.receive_delivery(&message(1), true, 1, false).unwrap();
        s.receive_delivery(&message(2), true, 1, true).unwrap();
        assert_eq!(s.pending().unwrap().len(), 1);
        s.set_channels(&[]).unwrap();
        assert!(s.pending().unwrap().is_empty());
        assert_eq!(s.history(1, 0).unwrap().len(), 2);
        drop(s);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn v1_migration_preserves_history_cursor_draft_and_new_colors() {
        let (s, p) = store();
        drop(s);
        std::fs::remove_file(p.join("history.db")).unwrap();
        let c = Connection::open(p.join("history.db")).unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL);INSERT INTO meta VALUES('cursor',42);CREATE TABLE messages(id TEXT PRIMARY KEY,seq INTEGER NOT NULL,channel_id INTEGER NOT NULL,sender_id INTEGER NOT NULL,sender_name TEXT NOT NULL,text TEXT NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,pending INTEGER NOT NULL DEFAULT 0,replay INTEGER NOT NULL DEFAULT 0);INSERT INTO messages VALUES('old',1,1,2,'Старый','история',1,2,0,0);CREATE TABLE channels(id INTEGER PRIMARY KEY,name TEXT NOT NULL);INSERT INTO channels VALUES(1,'Общий');CREATE TABLE drafts(channel_id INTEGER PRIMARY KEY,text TEXT NOT NULL,message_id TEXT NOT NULL);INSERT INTO drafts VALUES(1,'черновик','retry-id');PRAGMA user_version=1;").unwrap();
        drop(c);
        let s = Store::open(&p).unwrap();
        assert_eq!(s.cursor().unwrap(), 42);
        assert_eq!(s.draft(1).unwrap(), ("черновик".into(), "retry-id".into()));
        s.update_users(&[User {
            id: 2,
            name: "Новый".into(),
            color: "#ff8000".into(),
            disabled: false,
            roles: vec![],
            actions: vec![],
        }])
        .unwrap();
        drop(s);
        let s = Store::open(&p).unwrap();
        let m = s.history(1, 0).unwrap().remove(0);
        assert_eq!(m.id, "old");
        assert_eq!(m.text, "история");
        assert_eq!(m.sender_name, "Новый");
        assert_eq!(m.sender_color, "#ff8000");
        drop(s);
        std::fs::remove_dir_all(p).unwrap();
    }
}
