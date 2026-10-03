use crate::{
    access, db,
    http::{self, ApiResult, App},
};
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use poknite_protocol::*;
use rusqlite::{Connection, OptionalExtension, params};
use std::{net::SocketAddr, sync::Arc};

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/v2/admin/users", get(users).post(create_user))
        .route(
            "/v2/admin/users/{id}",
            get(get_user).put(edit_user).delete(disable_user),
        )
        .route("/v2/admin/channels", get(channels).post(create_channel))
        .route(
            "/v2/admin/channels/{id}",
            get(get_channel).put(edit_channel).delete(close_channel),
        )
        .route(
            "/v2/admin/channels/{id}/rules",
            get(get_rules).put(set_rules),
        )
        .route("/v2/admin/roles", get(roles).post(create_role))
        .route(
            "/v2/admin/roles/{id}",
            get(get_role).put(edit_role).delete(disable_role),
        )
        .route("/v2/admin/users/{id}/invitations", post(invite))
        .route("/v2/admin/users/{id}/devices", get(devices))
        .route("/v2/admin/devices/{id}", axum::routing::delete(revoke))
        .route(
            "/v2/admin/users/{id}/invitations",
            axum::routing::delete(revoke_invitations),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_FRAME_BYTES))
        .layer(middleware::from_fn_with_state(app.clone(), trusted_peer))
        .layer(middleware::from_fn_with_state(
            app.clone(),
            http::request_limit,
        ))
        .with_state(app)
}
async fn trusted_peer(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !app.config.management.enabled || !app.config.management.trusted(peer.ip()) {
        return http::error(
            StatusCode::FORBIDDEN,
            "untrusted_network",
            "Управление недоступно из текущей сети",
        )
        .into_response();
    }
    next.run(request).await
}
async fn read<T: Send + 'static>(
    app: &App,
    headers: &HeaderMap,
    p: Permission,
    f: impl FnOnce(&Connection, i64) -> Result<T> + Send + 'static,
) -> ApiResult<Json<T>> {
    let a = http::auth(app, headers).await?;
    let value = app
        .db
        .run(move |c| {
            let tx = c.transaction()?;
            db::active(&tx, a.device_id)?;
            let candidates: &[Permission] = match p {
                Permission::Users => &[
                    Permission::Users,
                    Permission::Devices,
                    Permission::Invitations,
                ],
                Permission::Roles => &[
                    Permission::Roles,
                    Permission::Users,
                    Permission::Channels,
                    Permission::ManageChannel,
                ],
                Permission::Channels | Permission::ManageChannel => {
                    &[Permission::Channels, Permission::ManageChannel]
                }
                _ => std::slice::from_ref(&p),
            };
            ensure!(
                candidates
                    .iter()
                    .any(|p| access::allowed(&tx, a.user_id, *p).unwrap_or(false)),
                "forbidden"
            );
            f(&tx, a.user_id)
        })
        .await
        .map_err(http::database_error)?;
    Ok(Json(value))
}
async fn write<T: Send + 'static>(
    app: &App,
    headers: &HeaderMap,
    p: Permission,
    f: impl FnOnce(&Connection, i64) -> Result<T> + Send + 'static,
) -> ApiResult<Json<T>> {
    let a = http::auth(app, headers).await?;
    app.limit(format!("admin:{}", a.device_id), 60)?;
    let cfg = app.config.clone();
    let _gate = app.delivery_gate.write().await;
    let value = app
        .db
        .run(move |c| {
            db::writable(c, &cfg)?;
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            db::active(&tx, a.device_id)?;
            if p == Permission::ManageChannel {
                ensure!(
                    access::allowed(&tx, a.user_id, Permission::Channels)?
                        || access::allowed(&tx, a.user_id, Permission::ManageChannel)?,
                    "forbidden"
                );
            } else {
                access::require(&tx, a.user_id, p)?;
            }
            let value = f(&tx, a.user_id)?;
            tx.commit()?;
            Ok(value)
        })
        .await
        .map_err(http::database_error)?;
    app.cancel_inactive().await.map_err(http::database_error)?;
    app.wake();
    Ok(Json(value))
}
fn valid_name(name: &str) -> Result<()> {
    ensure!(
        poknite_protocol::valid_name(name) && name == name.trim(),
        "invalid_input"
    );
    Ok(())
}
fn existing_user(c: &Connection, id: i64) -> Result<()> {
    let _ = access::user(c, id)?;
    Ok(())
}
fn assign(c: &Connection, actor: i64, id: i64, roles: &[i64]) -> Result<()> {
    ensure!(roles.len() <= 32, "invalid_input");
    let all = access::roles(c)?;
    for r in roles {
        let role = all
            .iter()
            .find(|v| v.id == *r)
            .ok_or_else(|| anyhow::anyhow!("not_found"))?;
        access::delegation(c, actor, &role.allow)?;
    }
    c.execute("DELETE FROM user_roles WHERE user_id=?", [id])?;
    for r in roles {
        c.execute(
            "INSERT OR IGNORE INTO user_roles VALUES(?,?)",
            params![id, r],
        )?;
    }
    Ok(())
}
async fn users(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(page): Query<http::ListPage>,
) -> ApiResult<Json<Vec<User>>> {
    read(&app, &h, Permission::Users, move |c, _| {
        let mut q = c.prepare("SELECT id FROM users ORDER BY id")?;
        let ids = q
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(page.take(
            ids.into_iter()
                .map(|id| access::user(c, id))
                .collect::<Result<Vec<_>>>()?,
        ))
    })
    .await
}
async fn get_user(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<User>> {
    read(&app, &h, Permission::Users, move |c, _| access::user(c, id)).await
}
async fn create_user(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(r): Json<UserRequest>,
) -> ApiResult<Json<User>> {
    let max = app.config.max_users as i64;
    write(&app, &h, Permission::Users, move |c, a| {
        valid_name(&r.name)?;
        let count: i64 = c.query_row("SELECT COUNT(*) FROM users WHERE disabled=0", [], |r| {
            r.get(0)
        })?;
        ensure!(count < max, "user_limit");
        c.execute(
            "INSERT INTO users(name,disabled) VALUES(?,?)",
            params![r.name, r.disabled],
        )?;
        let id = c.last_insert_rowid();
        if let Some(color) = r.color.as_ref() {
            ensure!(poknite_protocol::valid_color(color), "invalid_input");
            c.execute(
                "UPDATE users SET color=? WHERE id=?",
                params![color.to_lowercase(), id],
            )?;
        }
        assign(c, a, id, &r.roles)?;
        access::user(c, id)
    })
    .await
}
async fn edit_user(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
    Json(r): Json<UserRequest>,
) -> ApiResult<Json<User>> {
    write(&app, &h, Permission::Users, move |c, a| {
        existing_user(c, id)?;
        access::delegation(c, a, &access::user(c, id)?.actions)?;
        valid_name(&r.name)?;
        if let Some(color) = r.color.as_ref() {
            ensure!(poknite_protocol::valid_color(color), "invalid_input");
            c.execute(
                "UPDATE users SET color=? WHERE id=?",
                params![color.to_lowercase(), id],
            )?;
        }
        assign(c, a, id, &r.roles)?;
        c.execute(
            "UPDATE users SET name=?,disabled=? WHERE id=?",
            params![r.name, r.disabled, id],
        )?;
        access::ensure_last_admin(c)?;
        if r.disabled {
            disable(c, id)?;
        }
        access::user(c, id)
    })
    .await
}
fn disable(c: &Connection, id: i64) -> Result<()> {
    c.execute("UPDATE users SET disabled=1 WHERE id=?", [id])?;
    c.execute("UPDATE devices SET revoked=1 WHERE user_id=?", [id])?;
    c.execute("DELETE FROM invitations WHERE user_id=?", [id])?;
    Ok(())
}
async fn disable_user(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<User>> {
    write(&app, &h, Permission::Users, move |c, a| {
        existing_user(c, id)?;
        access::delegation(c, a, &access::user(c, id)?.actions)?;
        disable(c, id)?;
        access::ensure_last_admin(c)?;
        access::user(c, id)
    })
    .await
}
fn channel(c: &Connection, id: i64) -> Result<Channel> {
    let (name, closed, kind): (String, bool, String) = c
        .query_row(
            "SELECT name,closed,kind FROM channels WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("not_found"))?;
    ensure!(kind == "channel", "forbidden");
    Ok(Channel {
        id,
        name,
        closed,
        kind: ConversationKind::Channel,
        actions: Vec::new(),
    })
}
fn manage_channel(c: &Connection, a: i64, id: i64) -> Result<()> {
    ensure!(
        access::allowed(c, a, Permission::Channels)?
            || access::conversation_allowed(c, a, id, Permission::ManageChannel)?,
        "forbidden"
    );
    Ok(())
}
async fn channels(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(page): Query<http::ListPage>,
) -> ApiResult<Json<Vec<Channel>>> {
    read(&app, &h, Permission::Channels, move |c, a| {
        let mut q = c.prepare("SELECT id FROM channels WHERE kind='channel' ORDER BY id")?;
        let ids = q
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(page.take(
            ids.into_iter()
                .filter(|id| manage_channel(c, a, *id).is_ok())
                .map(|id| channel(c, id))
                .collect::<Result<Vec<_>>>()?,
        ))
    })
    .await
}
async fn get_channel(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Channel>> {
    read(&app, &h, Permission::ManageChannel, move |c, a| {
        manage_channel(c, a, id)?;
        channel(c, id)
    })
    .await
}
async fn create_channel(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(r): Json<ChannelRequest>,
) -> ApiResult<Json<Channel>> {
    write(&app, &h, Permission::Channels, move |c, a| {
        valid_name(&r.name)?;
        let count: i64 = c.query_row(
            "SELECT COUNT(*) FROM channels WHERE kind='channel' AND closed=0",
            [],
            |r| r.get(0),
        )?;
        ensure!(count < 64, "channel_limit");
        c.execute(
            "INSERT INTO channels(name,closed) VALUES(?,?)",
            params![r.name, r.closed],
        )?;
        let id = c.last_insert_rowid();
        c.execute("INSERT INTO memberships VALUES(?,?)", params![a, id])?;
        channel(c, id)
    })
    .await
}
async fn edit_channel(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
    Json(r): Json<ChannelRequest>,
) -> ApiResult<Json<Channel>> {
    write(&app, &h, Permission::ManageChannel, move |c, a| {
        manage_channel(c, a, id)?;
        let old = channel(c, id)?;
        ensure!(!old.closed || r.closed, "closed");
        valid_name(&r.name)?;
        c.execute(
            "UPDATE channels SET name=?,closed=? WHERE id=?",
            params![r.name, r.closed, id],
        )?;
        channel(c, id)
    })
    .await
}
async fn close_channel(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Channel>> {
    write(&app, &h, Permission::ManageChannel, move |c, a| {
        manage_channel(c, a, id)?;
        channel(c, id)?;
        c.execute("UPDATE channels SET closed=1 WHERE id=?", [id])?;
        channel(c, id)
    })
    .await
}
async fn get_rules(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<ChannelRule>>> {
    read(&app, &h, Permission::ManageChannel, move |c, a| {
        manage_channel(c, a, id)?;
        channel(c, id)?;
        access::rules(c, id)
    })
    .await
}
async fn set_rules(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
    Json(r): Json<Vec<ChannelRule>>,
) -> ApiResult<Json<Vec<ChannelRule>>> {
    write(&app, &h, Permission::ManageChannel, move |c, a| {
        manage_channel(c, a, id)?;
        channel(c, id)?;
        ensure!(r.len() <= 64, "invalid_input");
        let all = access::roles(c)?;
        for rule in &r {
            ensure!(all.iter().any(|v| v.id == rule.role_id), "not_found");
            for p in rule.allow.iter().chain(&rule.deny) {
                ensure!(
                    matches!(
                        p,
                        Permission::Read
                            | Permission::Send
                            | Permission::Mention
                            | Permission::ManageChannel
                    ),
                    "invalid_input"
                );
            }
            for p in &rule.allow {
                ensure!(access::conversation_allowed(c, a, id, *p)?, "forbidden");
            }
        }
        // Removing a deny is also a grant: delegated managers must own the affected action.
        for old in access::rules(c, id)? {
            for p in old.deny {
                if !r
                    .iter()
                    .any(|v| v.role_id == old.role_id && v.deny.contains(&p))
                {
                    ensure!(access::conversation_allowed(c, a, id, p)?, "forbidden");
                }
            }
        }
        c.execute("DELETE FROM channel_rules WHERE channel_id=?", [id])?;
        for rule in &r {
            c.execute(
                "INSERT INTO channel_rules VALUES(?,?,?,?)",
                params![
                    id,
                    rule.role_id,
                    serde_json::to_string(&rule.allow)?,
                    serde_json::to_string(&rule.deny)?
                ],
            )?;
        }
        access::rules(c, id)
    })
    .await
}
async fn roles(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Query(page): Query<http::ListPage>,
) -> ApiResult<Json<Vec<Role>>> {
    read(&app, &h, Permission::Roles, move |c, _| {
        Ok(page.take(access::roles(c)?))
    })
    .await
}
async fn get_role(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Role>> {
    read(&app, &h, Permission::Roles, move |c, _| {
        access::roles(c)?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("not_found"))
    })
    .await
}
fn validate_role(c: &Connection, a: i64, r: &RoleRequest) -> Result<()> {
    valid_name(&r.name)?;
    ensure!(
        r.allow.len() <= ALL_PERMISSIONS.len() && r.deny.len() <= ALL_PERMISSIONS.len(),
        "invalid_input"
    );
    access::delegation(c, a, &r.allow)
}
async fn create_role(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(r): Json<RoleRequest>,
) -> ApiResult<Json<Role>> {
    write(&app, &h, Permission::Roles, move |c, a| {
        validate_role(c, a, &r)?;
        ensure!(access::roles(c)?.len() < 64, "role_limit");
        c.execute(
            "INSERT INTO roles(name,allow,deny) VALUES(?,?,?)",
            params![
                r.name,
                serde_json::to_string(&r.allow)?,
                serde_json::to_string(&r.deny)?
            ],
        )?;
        Ok(Role {
            id: c.last_insert_rowid(),
            name: r.name,
            allow: r.allow,
            deny: r.deny,
        })
    })
    .await
}
async fn edit_role(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
    Json(r): Json<RoleRequest>,
) -> ApiResult<Json<Role>> {
    write(&app, &h, Permission::Roles, move |c, a| {
        ensure!(id > 3, "builtin_role");
        let old = access::roles(c)?
            .into_iter()
            .find(|v| v.id == id)
            .ok_or_else(|| anyhow::anyhow!("not_found"))?;
        access::delegation(c, a, &old.allow)?;
        validate_role(c, a, &r)?;
        c.execute(
            "UPDATE roles SET name=?,allow=?,deny=? WHERE id=?",
            params![
                r.name,
                serde_json::to_string(&r.allow)?,
                serde_json::to_string(&r.deny)?,
                id
            ],
        )?;
        access::ensure_last_admin(c)?;
        Ok(Role {
            id,
            name: r.name,
            allow: r.allow,
            deny: r.deny,
        })
    })
    .await
}
async fn disable_role(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<bool>> {
    write(&app, &h, Permission::Roles, move |c, a| {
        ensure!(id > 3, "builtin_role");
        let old = access::roles(c)?
            .into_iter()
            .find(|v| v.id == id)
            .ok_or_else(|| anyhow::anyhow!("not_found"))?;
        access::delegation(c, a, &old.allow)?;
        access::delegation(c, a, &old.deny)?;
        c.execute("UPDATE roles SET disabled=1 WHERE id=?", [id])?;
        access::ensure_last_admin(c)?;
        Ok(true)
    })
    .await
}
async fn invite(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    write(&app, &h, Permission::Invitations, move |c, _| {
        ensure!(access::enabled(c, id)?, "not_found");
        c.execute("DELETE FROM invitations WHERE expires_at<=?", [db::now()])?;
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM invitations WHERE user_id=?",
            [id],
            |r| r.get(0),
        )?;
        ensure!(n < 3, "invitation_limit");
        let code = db::secret()?;
        c.execute(
            "INSERT INTO invitations VALUES(?,?,?)",
            params![db::hash(&code), id, db::now() + 900],
        )?;
        Ok(serde_json::json!({"invitation":code,"expires_at":db::now()+900}))
    })
    .await
}
async fn revoke_invitations(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<bool>> {
    write(&app, &h, Permission::Invitations, move |c, _| {
        existing_user(c, id)?;
        c.execute("DELETE FROM invitations WHERE user_id=?", [id])?;
        Ok(true)
    })
    .await
}
async fn devices(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<Device>>> {
    read(&app, &h, Permission::Devices, move |c, _| {
        existing_user(c, id)?;
        db::devices(
            c,
            &db::Auth {
                device_id: 0,
                user_id: id,
                user_name: String::new(),
            },
        )
    })
    .await
}
async fn revoke(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<bool>> {
    write(&app, &h, Permission::Devices, move |c, _| {
        ensure!(
            c.execute(
                "UPDATE devices SET revoked=1 WHERE id=? AND revoked=0",
                [id]
            )? > 0,
            "not_found"
        );
        Ok(true)
    })
    .await
}
