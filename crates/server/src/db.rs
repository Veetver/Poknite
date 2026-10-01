use crate::config::Config;
use anyhow::{Context, Result, ensure};
use poknite_protocol::{Channel, Device, Message};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}
pub fn secret() -> Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|_| anyhow::anyhow!("Недоступен генератор случайных чисел"))?;
    Ok(hex::encode(b))
}
pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn open(config: &Config) -> Result<Connection> {
    private_dir(&config.data_dir)?;
    let conn = Connection::open(config.database())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(config.database(), std::fs::Permissions::from_mode(0o600))?;
    }
    conn.busy_timeout(Duration::from_secs(3))?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(version <= 1, "База создана более новой версией Poknite");
    if version == 0 {
        conn.execute_batch("PRAGMA page_size=4096;PRAGMA auto_vacuum=INCREMENTAL;")?;
    }
    conn.execute_batch("PRAGMA foreign_keys=ON;PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA secure_delete=ON;PRAGMA cache_size=-2048;PRAGMA temp_store=MEMORY;PRAGMA wal_autocheckpoint=64;")?;
    conn.pragma_update(None, "journal_size_limit", config.wal_bytes as i64)?;
    conn.pragma_update(
        None,
        "max_page_count",
        (config.database_bytes / 4096) as i64,
    )?;
    let actual: i64 = conn.pragma_query_value(None, "max_page_count", |r| r.get(0))?;
    ensure!(
        actual <= (config.database_bytes / 4096) as i64,
        "База превышает настроенный бюджет; сначала освободите место со старой конфигурацией"
    );
    if version == 0 {
        conn.execute_batch(include_str!("schema.sql"))?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS messages_device ON messages(device_id)")?;
    Ok(conn)
}
type Job = Box<dyn FnOnce(&mut Connection) + Send>;
#[derive(Clone)]
pub struct Database {
    sender: mpsc::Sender<Job>,
}
impl Database {
    pub fn start(config: &Config) -> Result<Self> {
        let mut conn = open(config)?;
        let (sender, mut receiver) = mpsc::channel::<Job>(64);
        std::thread::Builder::new()
            .name("sqlite".into())
            .spawn(move || {
                while let Some(job) = receiver.blocking_recv() {
                    job(&mut conn);
                }
                let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            })?;
        Ok(Self { sender })
    }
    pub async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .try_send(Box::new(move |c| {
                let _ = tx.send(work(c));
            }))
            .map_err(|_| anyhow::anyhow!("База занята или остановлена"))?;
        rx.await.context("База остановлена")?
    }
}
#[derive(Clone, Debug)]
pub struct Auth {
    pub device_id: i64,
    pub user_id: i64,
    pub user_name: String,
}
pub fn authenticate(c: &Connection, token_hash: &str) -> Result<Option<Auth>> {
    Ok(c.query_row("SELECT d.id,u.id,u.name FROM devices d JOIN users u ON u.id=d.user_id WHERE d.token_hash=? AND d.revoked=0",[token_hash],|r|Ok(Auth{device_id:r.get(0)?,user_id:r.get(1)?,user_name:r.get(2)?})).optional()?)
}
pub fn active(c: &Connection, device: i64) -> Result<()> {
    ensure!(
        c.query_row(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id=? AND revoked=0)",
            [device],
            |r| r.get::<_, bool>(0)
        )?,
        "unauthorized"
    );
    Ok(())
}
pub fn channels(c: &Connection, user: i64) -> Result<Vec<Channel>> {
    let mut q=c.prepare("SELECT c.id,c.name FROM channels c JOIN memberships m ON m.channel_id=c.id WHERE m.user_id=? ORDER BY c.id")?;
    Ok(q.query_map([user], |r| {
        Ok(Channel {
            id: r.get(0)?,
            name: r.get(1)?,
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()?)
}
pub fn devices(c: &Connection, a: &Auth) -> Result<Vec<Device>> {
    let mut q = c.prepare(
        "SELECT id,name,created_at FROM devices WHERE user_id=? AND revoked=0 ORDER BY id",
    )?;
    Ok(q.query_map([a.user_id], |r| {
        let id = r.get(0)?;
        Ok(Device {
            id,
            name: r.get(1)?,
            created_at: r.get(2)?,
            current: id == a.device_id,
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()?)
}
pub fn message_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        seq: r.get(0)?,
        id: r.get(1)?,
        channel_id: r.get(2)?,
        sender_id: r.get(3)?,
        sender_name: r.get(4)?,
        text: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
    })
}
pub const MESSAGE_COLUMNS: &str =
    "m.seq,m.id,m.channel_id,m.sender_id,u.name,m.text,m.created_at,m.expires_at";
pub fn highwater(c: &Connection) -> Result<i64> {
    Ok(c.query_row(
        "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='messages'),0)",
        [],
        |r| r.get(0),
    )?)
}
pub fn backlog(c: &mut Connection, a: &Auth, after: i64) -> Result<(Vec<Message>, i64, bool)> {
    let tx = c.transaction()?;
    active(&tx, a.device_id)?;
    let high = highwater(&tx)?;
    let current = now();
    let gap:bool=tx.query_row("SELECT lost_through>? OR EXISTS(SELECT 1 FROM messages m JOIN memberships g ON g.channel_id=m.channel_id WHERE g.user_id=devices.user_id AND m.seq>? AND m.expires_at<=?) FROM devices WHERE id=?",params![after,after,current,a.device_id],|r|r.get(0))?;
    let messages = {
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m JOIN users u ON u.id=m.sender_id JOIN memberships g ON g.channel_id=m.channel_id WHERE g.user_id=? AND m.seq>? AND m.expires_at>? ORDER BY m.seq LIMIT 50"
        );
        let mut q = tx.prepare(&sql)?;
        q.query_map(params![a.user_id, after, current], message_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let next = if messages.len() == 50 {
        messages.last().unwrap().seq
    } else {
        high
    };
    tx.commit()?;
    Ok((messages, next, gap))
}
pub fn writable(c: &Connection, config: &Config) -> Result<()> {
    let wal = config.database().with_extension("db-wal");
    if std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0) >= config.wal_bytes {
        let busy: i64 = c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
        ensure!(busy == 0, "storage_full");
    }
    Ok(())
}
pub fn cleanup(c: &mut Connection) -> Result<usize> {
    let tx = c.transaction()?;
    let n = now();
    tx.execute("UPDATE devices SET lost_through=MAX(lost_through,COALESCE((SELECT MAX(m.seq) FROM messages m JOIN memberships g ON g.channel_id=m.channel_id WHERE g.user_id=devices.user_id AND m.seq>devices.acked AND m.seq IN(SELECT seq FROM messages WHERE expires_at<=? ORDER BY expires_at LIMIT 256)),0)) WHERE revoked=0",[n])?;
    let removed=tx.execute("DELETE FROM messages WHERE seq IN(SELECT seq FROM messages WHERE expires_at<=? ORDER BY expires_at LIMIT 256)",[n])?;
    tx.execute("DELETE FROM invitations WHERE expires_at<=?", [n])?;
    tx.execute("DELETE FROM devices WHERE revoked=1 AND NOT EXISTS(SELECT 1 FROM messages WHERE messages.device_id=devices.id)",[])?;
    tx.commit()?;
    c.execute_batch("PRAGMA incremental_vacuum(32);PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(removed)
}
