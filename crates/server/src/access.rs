use anyhow::{Result, ensure};
use poknite_protocol::*;
use rusqlite::{Connection, OptionalExtension, params};

pub fn roles(c: &Connection) -> Result<Vec<Role>> {
    let mut q = c.prepare("SELECT id,name,allow,deny FROM roles WHERE disabled=0 ORDER BY id")?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<(i64, String, String, String)>>>()?;
    rows.into_iter()
        .map(|(id, name, allow, deny)| {
            Ok(Role {
                id,
                name,
                allow: serde_json::from_str(&allow)?,
                deny: serde_json::from_str(&deny)?,
            })
        })
        .collect()
}
pub fn role_ids(c: &Connection, user: i64) -> Result<Vec<i64>> {
    let mut q = c.prepare("SELECT ur.role_id FROM user_roles ur JOIN roles r ON r.id=ur.role_id WHERE ur.user_id=? AND r.disabled=0 ORDER BY ur.role_id")?;
    Ok(q.query_map([user], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
pub fn enabled(c: &Connection, user: i64) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id=? AND disabled=0)",
        [user],
        |r| r.get(0),
    )?)
}
pub fn allowed(c: &Connection, user: i64, permission: Permission) -> Result<bool> {
    if !enabled(c, user)? {
        return Ok(false);
    }
    let permission = serde_json::to_value(permission)?
        .as_str()
        .unwrap()
        .to_string();
    Ok(c.prepare_cached("SELECT EXISTS(SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id,json_each(r.allow) p WHERE ur.user_id=?1 AND r.disabled=0 AND p.value=?2) AND NOT EXISTS(SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id,json_each(r.deny) p WHERE ur.user_id=?1 AND r.disabled=0 AND p.value=?2)")?.query_row(params![user,permission],|r|r.get(0))?)
}
pub fn require(c: &Connection, user: i64, permission: Permission) -> Result<()> {
    ensure!(allowed(c, user, permission)?, "forbidden");
    Ok(())
}
pub fn user(c: &Connection, id: i64) -> Result<User> {
    let (name, disabled, color) = c
        .query_row(
            "SELECT name,disabled,color FROM users WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("not_found"))?;
    let actions = ALL_PERMISSIONS
        .iter()
        .copied()
        .filter_map(|p| match allowed(c, id, p) {
            Ok(true) => Some(Ok(p)),
            Ok(false) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<Result<_>>()?;
    Ok(User {
        id,
        name,
        color,
        disabled,
        roles: role_ids(c, id)?,
        actions,
    })
}
pub fn administrator(c: &Connection, id: i64) -> Result<bool> {
    Ok(enabled(c, id)? && role_ids(c, id)?.contains(&1))
}
pub fn rules(c: &Connection, channel: i64) -> Result<Vec<ChannelRule>> {
    let mut q = c.prepare(
        "SELECT role_id,allow,deny FROM channel_rules WHERE channel_id=? ORDER BY role_id",
    )?;
    let rows = q
        .query_map([channel], |r| {
            Ok((r.get(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, String, String)>>>()?;
    rows.into_iter()
        .map(|(role_id, allow, deny)| {
            Ok(ChannelRule {
                role_id,
                allow: serde_json::from_str(&allow)?,
                deny: serde_json::from_str(&deny)?,
            })
        })
        .collect()
}
fn channel_allowed(
    c: &Connection,
    user: i64,
    channel: i64,
    permission: Permission,
) -> Result<bool> {
    if !allowed(c, user, permission)? {
        return Ok(false);
    }
    if administrator(c, user)? {
        return Ok(true);
    }
    let ids = role_ids(c, user)?;
    let rules = rules(c, channel)?;
    let rules = rules
        .iter()
        .filter(|r| ids.contains(&r.role_id))
        .collect::<Vec<_>>();
    if rules.iter().any(|r| r.deny.contains(&permission)) {
        return Ok(false);
    }
    let personal: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM memberships WHERE user_id=? AND channel_id=?)",
        params![user, channel],
        |r| r.get(0),
    )?;
    Ok(rules.iter().any(|r| r.allow.contains(&permission))
        || personal
            && matches!(
                permission,
                Permission::Read | Permission::Send | Permission::Mention
            ))
}
pub fn common_channel(c: &Connection, a: i64, b: i64) -> Result<bool> {
    if a == b || !enabled(c, a)? || !enabled(c, b)? {
        return Ok(false);
    }
    let mut q =
        c.prepare("SELECT id FROM channels WHERE kind='channel' AND closed=0 ORDER BY id")?;
    let ids = q
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        if channel_allowed(c, a, id, Permission::Read)?
            && channel_allowed(c, b, id, Permission::Read)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}
pub fn conversation_allowed(
    c: &Connection,
    user: i64,
    id: i64,
    permission: Permission,
) -> Result<bool> {
    let row: Option<(String, bool, Option<i64>, Option<i64>)> = c
        .query_row(
            "SELECT kind,closed,user_lo,user_hi FROM channels WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((kind, closed, lo, hi)) = row else {
        return Ok(false);
    };
    if closed && permission != Permission::Read {
        return Ok(false);
    }
    if kind == "channel" {
        return channel_allowed(c, user, id, permission);
    }
    if Some(user) != lo && Some(user) != hi {
        return Ok(false);
    }
    if !allowed(c, user, Permission::Direct)? || !allowed(c, user, permission)? {
        return Ok(false);
    }
    if permission == Permission::Read {
        return Ok(true);
    }
    if permission != Permission::Send {
        return Ok(false);
    }
    common_channel(c, lo.unwrap(), hi.unwrap())
}
pub fn conversations(c: &Connection, user: i64) -> Result<Vec<Conversation>> {
    let mut q = c.prepare("SELECT id,name,kind,closed,user_lo,user_hi FROM channels WHERE kind='channel' OR user_lo=? OR user_hi=? ORDER BY id")?;
    let rows = q
        .query_map([user, user], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, String>(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<(i64, String, String, bool, Option<i64>, Option<i64>)>>>(
        )?;
    let mut result = Vec::new();
    for (id, mut name, kind, closed, lo, hi) in rows {
        if !conversation_allowed(c, user, id, Permission::Read)? {
            continue;
        }
        let actions = [
            Permission::Read,
            Permission::Send,
            Permission::Mention,
            Permission::ManageChannel,
        ]
        .into_iter()
        .filter_map(|p| match conversation_allowed(c, user, id, p) {
            Ok(true) => Some(Ok(p)),
            Ok(false) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<Result<_>>()?;
        let kind = if kind == "direct" {
            let peer = if lo == Some(user) {
                hi.unwrap()
            } else {
                lo.unwrap()
            };
            name = c.query_row("SELECT name FROM users WHERE id=?", [peer], |r| r.get(0))?;
            ConversationKind::Direct
        } else {
            ConversationKind::Channel
        };
        result.push(Conversation {
            id,
            name,
            kind,
            closed,
            actions,
        });
    }
    Ok(result)
}
pub fn contacts(c: &Connection, own: i64) -> Result<Vec<User>> {
    if !allowed(c, own, Permission::Contacts)? {
        return Ok(Vec::new());
    }
    let mut q = c.prepare("SELECT id FROM users WHERE disabled=0 AND id<>? ORDER BY name,id")?;
    let ids = q
        .query_map([own], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = Vec::new();
    for id in ids {
        if common_channel(c, own, id)? {
            let u = public_profile(c, id)?;
            result.push(u);
        }
    }
    Ok(result)
}
pub fn participants(c: &Connection, own: i64, channel: i64) -> Result<Vec<User>> {
    ensure!(
        conversation_allowed(c, own, channel, Permission::Read)?,
        "forbidden"
    );
    let kind: String = c.query_row("SELECT kind FROM channels WHERE id=?", [channel], |r| {
        r.get(0)
    })?;
    ensure!(kind == "channel", "forbidden");
    let mut q = c.prepare("SELECT id FROM users WHERE disabled=0 ORDER BY name,id")?;
    let ids = q
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = Vec::new();
    for id in ids {
        if conversation_allowed(c, id, channel, Permission::Read)? {
            let u = public_profile(c, id)?;
            result.push(u);
        }
    }
    Ok(result)
}
pub fn mentions_valid(
    c: &Connection,
    own: i64,
    channel: i64,
    text: &str,
    mentions: &[Mention],
) -> Result<()> {
    ensure!(mentions.len() <= 32, "invalid_mentions");
    if mentions.is_empty() {
        return Ok(());
    }
    ensure!(
        conversation_allowed(c, own, channel, Permission::Mention)?,
        "forbidden"
    );
    let sealed = poknite_protocol::e2ee::parse(text).is_some();
    let chars = text.chars().collect::<Vec<_>>();
    let mut end = 0;
    for m in mentions {
        ensure!(
            m.start >= end
                && m.start < m.end
                && m.end
                    <= if sealed {
                        poknite_protocol::MAX_TEXT_BYTES
                    } else {
                        chars.len()
                    },
            "invalid_mentions"
        );
        ensure!(
            enabled(c, m.user_id)?
                && conversation_allowed(c, m.user_id, channel, Permission::Read)?,
            "invalid_mentions"
        );
        if !sealed {
            let name: String =
                c.query_row("SELECT name FROM users WHERE id=?", [m.user_id], |r| {
                    r.get(0)
                })?;
            ensure!(
                chars[m.start..m.end].iter().collect::<String>() == format!("@{name}"),
                "invalid_mentions"
            );
        }
        end = m.end;
    }
    Ok(())
}

pub fn e2ee_members(
    c: &Connection,
    own: i64,
    channel: i64,
) -> Result<Vec<poknite_protocol::e2ee::Member>> {
    ensure!(
        conversation_allowed(c, own, channel, Permission::Read)?,
        "forbidden"
    );
    let mut q = c.prepare("SELECT d.user_id,d.id FROM devices d JOIN users u ON u.id=d.user_id WHERE d.revoked=0 AND u.disabled=0 ORDER BY d.user_id,d.id LIMIT 1001")?;
    let devices = q
        .query_map([], |r| {
            Ok(poknite_protocol::e2ee::Member {
                user_id: r.get(0)?,
                device_id: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        devices.len() <= poknite_protocol::e2ee::MAX_MEMBERS,
        "too_many_devices"
    );
    let mut result = Vec::new();
    for device in devices {
        if conversation_allowed(c, device.user_id, channel, Permission::Read)? {
            result.push(device);
        }
    }
    Ok(result)
}
pub fn notify(message: &Message, user: i64) -> bool {
    message.sender_id != user
        && (message.mentions.is_empty() || message.mentions.iter().any(|m| m.user_id == user))
}
pub fn ensure_last_admin(c: &Connection) -> Result<()> {
    let mut q = c.prepare("SELECT user_id FROM user_roles WHERE role_id=1")?;
    let ids = q
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        if enabled(c, id)?
            && [
                Permission::Users,
                Permission::Channels,
                Permission::Devices,
                Permission::Invitations,
                Permission::Roles,
            ]
            .into_iter()
            .all(|p| allowed(c, id, p).unwrap_or(false))
        {
            return Ok(());
        }
    }
    anyhow::bail!("last_admin")
}
pub fn delegation(c: &Connection, actor: i64, allow: &[Permission]) -> Result<()> {
    for p in allow {
        require(c, actor, *p)?;
    }
    Ok(())
}

pub fn public_profile(c: &Connection, id: i64) -> Result<User> {
    Ok(c.query_row(
        "SELECT name,color,disabled FROM users WHERE id=?",
        [id],
        |r| {
            Ok(User {
                id,
                name: r.get(0)?,
                color: r.get(1)?,
                disabled: r.get(2)?,
                roles: Vec::new(),
                actions: Vec::new(),
            })
        },
    )?)
}
pub fn visible_profiles(c: &Connection, own: i64) -> Result<Vec<User>> {
    let mut ids = std::collections::BTreeSet::new();
    for channel in conversations(c, own)? {
        if channel.kind == ConversationKind::Direct {
            let pair: (i64, i64) = c.query_row(
                "SELECT user_lo,user_hi FROM channels WHERE id=?",
                [channel.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            ids.insert(if pair.0 == own { pair.1 } else { pair.0 });
        }
        let mut q = c.prepare(
            "SELECT DISTINCT sender_id FROM messages WHERE channel_id=? AND expires_at>? LIMIT 100",
        )?;
        for id in q.query_map(params![channel.id, crate::db::now()], |r| {
            r.get::<_, i64>(0)
        })? {
            ids.insert(id?);
        }
    }
    ids.into_iter().map(|id| public_profile(c, id)).collect()
}
