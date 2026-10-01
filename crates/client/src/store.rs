use anyhow::{Result, bail};
use poknite_protocol::{Channel, HISTORY_LIMIT, Message, PAGE_SIZE};
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
fn protect(value: &str) -> Result<String> {
    Ok(value.into())
}
#[cfg(not(windows))]
fn unprotect(value: &str) -> Result<String> {
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
fn protect(value: &str) -> Result<String> {
    Ok(crypt(value.as_bytes(), true)?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
#[cfg(windows)]
fn unprotect(value: &str) -> Result<String> {
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
    connection: Mutex<Connection>,
}
impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        secure_directory(directory)?;
        let path = directory.join("history.db");
        let connection = Connection::open(&path)?;
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
        Ok(Self {
            connection: Mutex::new(connection),
        })
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
        if !poknite_protocol::valid_text(&message.text) || message.seq < 1 {
            bail!("Некорректное сообщение сервера")
        }
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let pending = message.sender_id != user;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO messages VALUES(?,?,?,?,?,?,?,?,?,?)",
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
                replay
            ],
        )? > 0;
        tx.execute("DELETE FROM messages WHERE rowid IN (SELECT rowid FROM messages ORDER BY created_at DESC,rowid DESC LIMIT -1 OFFSET ?)",[HISTORY_LIMIT as i64])?;
        tx.commit()?;
        Ok(inserted)
    }
    pub fn history(&self, channel: i64, offset: usize) -> Result<Vec<Message>> {
        let c = self.connection.lock().unwrap();
        let mut s=c.prepare("SELECT id,seq,channel_id,sender_id,sender_name,text,created_at,expires_at FROM messages WHERE channel_id=? ORDER BY created_at DESC,rowid DESC LIMIT ? OFFSET ?")?;
        Ok(s.query_map(
            params![channel, PAGE_SIZE as i64, offset as i64],
            message_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn pending(&self) -> Result<Vec<(Message, bool)>> {
        let c = self.connection.lock().unwrap();
        let mut s=c.prepare("SELECT id,seq,channel_id,sender_id,sender_name,text,created_at,expires_at,replay FROM messages WHERE pending=1 ORDER BY created_at,rowid LIMIT 1000")?;
        Ok(s.query_map([], |r| Ok((message_row(r)?, r.get(8)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn take_pending(&self) -> Result<Vec<(Message, bool)>> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let messages = {
            let mut s=tx.prepare("SELECT id,seq,channel_id,sender_id,sender_name,text,created_at,expires_at,replay FROM messages WHERE pending=1 ORDER BY created_at,rowid LIMIT 1000")?;
            s.query_map([], |r| Ok((message_row(r)?, r.get(8)?)))?
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
    pub fn clear(&self) -> Result<()> {
        let c = self.connection.lock().unwrap();
        c.execute_batch("DELETE FROM messages;PRAGMA incremental_vacuum(2048);")?;
        Ok(())
    }
    pub fn reset_account(&self) -> Result<()> {
        self.connection.lock().unwrap().execute_batch("DELETE FROM messages;DELETE FROM channels;DELETE FROM muted;DELETE FROM drafts;DELETE FROM meta;PRAGMA incremental_vacuum(2048);")?;
        Ok(())
    }
    pub fn set_channels(&self, channels: &[Channel]) -> Result<()> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute("DELETE FROM channels", [])?;
        for ch in channels {
            tx.execute("INSERT INTO channels VALUES(?,?)", params![ch.id, ch.name])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn channels(&self) -> Result<Vec<Channel>> {
        let c = self.connection.lock().unwrap();
        let mut s = c.prepare("SELECT id,name FROM channels ORDER BY id")?;
        Ok(s.query_map([], |r| {
            Ok(Channel {
                id: r.get(0)?,
                name: r.get(1)?,
            })
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
    pub fn save_draft(&self, channel: i64, text: &str) -> Result<String> {
        if text.len() > poknite_protocol::MAX_TEXT_BYTES {
            bail!("Текст превышает 4096 байт")
        }
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let old: Option<(String, String)> = tx
            .query_row(
                "SELECT text,message_id FROM drafts WHERE channel_id=?",
                [channel],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let id = old
            .filter(|(previous, _)| previous == text)
            .map(|(_, id)| id)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        tx.execute("INSERT INTO drafts VALUES(?,?,?) ON CONFLICT(channel_id) DO UPDATE SET text=excluded.text,message_id=excluded.message_id",params![channel,text,id])?;
        tx.commit()?;
        Ok(id)
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
        text: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
    })
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
        }
    }
    #[test]
    fn durable_pending_deduplicates_and_retains_expired_copy() {
        let (s, p) = store();
        assert!(s.receive(&message(1), true, 1).unwrap());
        assert!(!s.receive(&message(1), false, 1).unwrap());
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
        s.receive(&message(1), false, 2).unwrap();
        s.set_muted(1, true).unwrap();
        s.receive(&message(2), false, 1).unwrap();
        assert_eq!(s.pending().unwrap().len(), 1);
        assert_eq!(s.history(1, 0).unwrap().len(), 2);
        assert!(s.muted(1).unwrap());
        drop(s);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn notification_claim_is_exclusive_and_recovers_after_crash() {
        let (s, p) = store();
        s.receive(&message(1), false, 1).unwrap();
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
            s.receive(&message(n), false, 1).unwrap();
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
}
