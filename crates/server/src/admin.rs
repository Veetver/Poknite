use crate::{config::Config, db};
use anyhow::{Result, bail, ensure};
use rusqlite::params;

pub fn command(config: &Config, args: &[String]) -> Result<()> {
    let mut conn = db::open(config)?;
    let Some(command) = args.first().map(String::as_str) else {
        bail!("Нет команды");
    };
    if !matches!(command, "init" | "status") {
        db::writable(&conn, config)?;
    }
    let value = |index: usize| {
        args.get(index)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Не указан аргумент"))
    };
    match command {
        "init" => println!("База подготовлена. Общий канал: 1"),
        "user" => {
            let name = value(1)?;
            ensure!(poknite_protocol::valid_name(&name), "Имя: 1…64 символа");
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
            ensure!(
                count < config.max_users as i64,
                "Лимит пользователей достигнут"
            );
            tx.execute("INSERT INTO users(name) VALUES(?)", [name.trim()])?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO memberships(user_id,channel_id) VALUES(?,1)",
                [id],
            )?;
            tx.commit()?;
            println!("Пользователь: {id}");
        }
        "channel" => {
            let name = value(1)?;
            ensure!(poknite_protocol::valid_name(&name), "Имя: 1…64 символа");
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))?;
            ensure!(count < 64, "Лимит 64 каналов достигнут");
            tx.execute("INSERT INTO channels(name) VALUES(?)", [name.trim()])?;
            let id = tx.last_insert_rowid();
            tx.commit()?;
            println!("Канал: {id}");
        }
        "grant" => {
            let user = value(1)?.parse::<i64>()?;
            let channel = value(2)?.parse::<i64>()?;
            conn.execute(
                "INSERT OR IGNORE INTO memberships VALUES(?,?)",
                params![user, channel],
            )?;
            println!("Доступ предоставлен");
        }
        "ungrant" => {
            let user = value(1)?.parse::<i64>()?;
            let channel = value(2)?.parse::<i64>()?;
            conn.execute(
                "DELETE FROM memberships WHERE user_id=? AND channel_id=?",
                params![user, channel],
            )?;
            println!("Доступ отозван");
        }
        "invite" => {
            let user = value(1)?.parse::<i64>()?;
            let code = db::secret()?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute("DELETE FROM invitations WHERE expires_at<=?", [db::now()])?;
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM invitations WHERE user_id=?",
                [user],
                |r| r.get(0),
            )?;
            ensure!(count < 3, "Уже есть три действующих приглашения");
            tx.execute(
                "INSERT INTO invitations VALUES(?,?,?)",
                params![db::hash(&code), user, db::now() + 900],
            )?;
            tx.commit()?;
            println!("{code}");
        }
        "revoke" => {
            let id = value(1)?.parse::<i64>()?;
            ensure!(
                conn.execute(
                    "UPDATE devices SET revoked=1 WHERE id=? AND revoked=0",
                    [id]
                )? > 0,
                "Устройство не найдено"
            );
            println!("Устройство отключено");
        }
        "status" => {
            for table in ["users", "channels", "devices", "messages"] {
                let count: i64 =
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
                println!("{table}: {count}");
            }
            let mut q = conn.prepare("SELECT id,name FROM users ORDER BY id")?;
            for entry in q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                let (id, name) = entry?;
                println!("user {id}: {name}");
            }
        }
        "cleanup" => {
            println!("Удалено: {}", db::cleanup(&mut conn)?);
        }
        _ => bail!(
            "Команды: serve, init, user ИМЯ, channel ИМЯ, grant USER CHANNEL, ungrant USER CHANNEL, invite USER, revoke DEVICE, status, cleanup"
        ),
    }
    // Admin commands are serialized by SQLite; the private socket wakes live streams immediately.
    if let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() {
        let _ = socket.send_to(b"wake", config.admin_socket());
    }
    Ok(())
}
