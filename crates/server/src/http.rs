use crate::{
    config::Config,
    db::{self, Auth, Database},
};
use anyhow::Result;
use axum::{
    Json, Router,
    extract::{
        ConnectInfo, DefaultBodyLimit, Path, Query, Request, State,
        ws::{Message as Frame, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use futures_util::{SinkExt, StreamExt};
use poknite_protocol::*;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, watch};

pub struct App {
    pub db: Database,
    pub config: Config,
    pub retention: AtomicI64,
    pub changed: watch::Sender<u64>,
    slots: Arc<Semaphore>,
    requests: Arc<Semaphore>,
    sessions: Mutex<HashMap<i64, (uuid::Uuid, watch::Sender<bool>)>>,
    rates: Mutex<HashMap<String, (Instant, u32)>>,
}
impl App {
    pub fn new(config: Config) -> Result<Arc<Self>> {
        config.validate()?;
        let db = Database::start(&config)?;
        let (changed, _) = watch::channel(0);
        Ok(Arc::new(Self {
            db,
            retention: AtomicI64::new(config.retention_seconds),
            slots: Arc::new(Semaphore::new(config.max_connections)),
            requests: Arc::new(Semaphore::new(128)),
            config,
            changed,
            sessions: Mutex::new(HashMap::new()),
            rates: Mutex::new(HashMap::new()),
        }))
    }
    pub fn wake(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    fn cancel(&self, device: i64) {
        if let Some((_, cancel)) = self.sessions.lock().unwrap().get(&device) {
            cancel.send_replace(true);
        }
    }
    fn limit(&self, key: String, max: u32) -> ApiResult<()> {
        let mut rates = self.rates.lock().unwrap();
        let now = Instant::now();
        if rates.len() >= 1024 {
            rates.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(60));
        }
        if rates.len() >= 1024 && !rates.contains_key(&key) {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit",
                "Слишком много запросов",
            ));
        }
        let (start, count) = rates.entry(key).or_insert((now, 0));
        if now.duration_since(*start) >= Duration::from_secs(60) {
            *start = now;
            *count = 0;
        }
        if *count >= max {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit",
                "Слишком много запросов",
            ));
        }
        *count += 1;
        Ok(())
    }
}
type ApiResult<T> = std::result::Result<T, Failure>;
pub struct Failure(StatusCode, ApiError);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}
fn error(status: StatusCode, code: &str, message: &str) -> Failure {
    Failure(
        status,
        ApiError {
            code: code.into(),
            message: message.into(),
        },
    )
}
fn database_error(e: anyhow::Error) -> Failure {
    if e.to_string() == "unauthorized" {
        return denied();
    }
    if e.to_string()=="storage_full" || e.chain().any(|e|matches!(e.downcast_ref::<rusqlite::Error>(),Some(rusqlite::Error::SqliteFailure(err,_)) if err.code==rusqlite::ErrorCode::DiskFull)) {
        error(StatusCode::INSUFFICIENT_STORAGE,"storage_full","Хранилище заполнено. Повторите после освобождения места")
    } else { error(StatusCode::SERVICE_UNAVAILABLE,"database_unavailable","Хранилище временно недоступно") }
}
fn denied() -> Failure {
    error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Устройство отключено или токен недействителен",
    )
}
async fn auth(app: &App, headers: &HeaderMap) -> ApiResult<Auth> {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| s.len() == 64)
        .ok_or_else(denied)?;
    let hash = db::hash(token);
    app.db
        .run(move |c| db::authenticate(c, &hash))
        .await
        .map_err(database_error)?
        .ok_or_else(denied)
}
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route(
            "/healthz",
            get(|| async {
                Json(serde_json::json!({"status":"ok","version":env!("CARGO_PKG_VERSION")}))
            }),
        )
        .route("/v1/devices/enroll", post(enroll))
        .route("/v1/channels", get(channel_list))
        .route("/v1/channels/{id}/messages", post(publish))
        .route("/v1/devices", get(device_list))
        .route("/v1/devices/{id}", delete(revoke))
        .route("/v1/stream", get(stream))
        .layer(DefaultBodyLimit::max(MAX_FRAME_BYTES))
        .layer(middleware::from_fn_with_state(app.clone(), request_limit))
        .with_state(app)
}
async fn request_limit(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if let Err(failure) = app.limit(format!("http:{}", peer.ip()), 2400) {
        return failure.into_response();
    }
    let Ok(_permit) = app.requests.clone().try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "request_limit",
            "Сервер занят",
        )
        .into_response();
    };
    let mut response = next.run(request).await;
    // Authentication responses and messages must never be kept by shared caches.
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
async fn enroll(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<EnrollRequest>,
) -> ApiResult<Json<EnrollResponse>> {
    app.limit(format!("enroll:{}", peer.ip()), 10)?;
    if request.invitation.len() != 64 || !valid_name(&request.device_name) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_input",
            "Проверьте приглашение и имя устройства",
        ));
    }
    let invitation_hash = db::hash(&request.invitation);
    let token = db::secret().map_err(database_error)?;
    let token_hash = db::hash(&token);
    let max = app.config.max_devices_per_user as i64;
    let capacity = app.config.max_users * app.config.max_devices_per_user;
    let app_config = app.config.clone();
    let result=app.db.run(move |c| {
        db::writable(c,&app_config)?;
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let user:Option<(i64,String)>=tx.query_row("SELECT u.id,u.name FROM invitations i JOIN users u ON u.id=i.user_id WHERE i.hash=? AND i.expires_at>?",params![invitation_hash,db::now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((user_id,user_name))=user else { return Ok(None); };
        let count:i64=tx.query_row("SELECT COUNT(*) FROM devices WHERE user_id=? AND revoked=0",[user_id],|r|r.get(0))?;
        let total:i64=tx.query_row("SELECT COUNT(*) FROM devices WHERE revoked=0",[],|r|r.get(0))?;
        if count>=max || total>=capacity as i64 { return Ok(Some(Err(()))); }
        tx.execute("DELETE FROM devices WHERE revoked=1 AND NOT EXISTS(SELECT 1 FROM messages WHERE messages.device_id=devices.id)",[])?;
        tx.execute("INSERT INTO devices(user_id,name,token_hash,created_at) VALUES(?,?,?,?)",params![user_id,request.device_name.trim(),token_hash,db::now()])?;
        let device_id=tx.last_insert_rowid();tx.execute("DELETE FROM invitations WHERE hash=?",[invitation_hash])?;tx.commit()?;
        Ok(Some(Ok((user_id,user_name,device_id))))
    }).await.map_err(database_error)?;
    let (user_id, user_name, device_id) = result
        .ok_or_else(|| {
            error(
                StatusCode::UNAUTHORIZED,
                "invalid_invitation",
                "Приглашение недействительно или уже использовано",
            )
        })?
        .map_err(|_| {
            error(
                StatusCode::CONFLICT,
                "device_limit",
                "Лимит устройств достигнут. Сначала отзовите старое устройство",
            )
        })?;
    app.wake();
    Ok(Json(EnrollResponse {
        token,
        user_id,
        user_name,
        device_id,
        retention_seconds: app.retention.load(Ordering::Relaxed),
    }))
}
async fn channel_list(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<Channel>>> {
    let a = auth(&app, &headers).await?;
    Ok(Json(
        app.db
            .run(move |c| {
                let tx = c.transaction()?;
                db::active(&tx, a.device_id)?;
                db::channels(&tx, a.user_id)
            })
            .await
            .map_err(database_error)?,
    ))
}
async fn device_list(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<Device>>> {
    let a = auth(&app, &headers).await?;
    Ok(Json(
        app.db
            .run(move |c| {
                let tx = c.transaction()?;
                db::active(&tx, a.device_id)?;
                db::devices(&tx, &a)
            })
            .await
            .map_err(database_error)?,
    ))
}
async fn revoke(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let a = auth(&app, &headers).await?;
    let cfg = app.config.clone();
    let changed = app
        .db
        .run(move |c| {
            db::writable(c, &cfg)?;
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            db::active(&tx, a.device_id)?;
            let count = tx.execute(
                "UPDATE devices SET revoked=1 WHERE id=? AND user_id=? AND revoked=0",
                params![id, a.user_id],
            )?;
            tx.commit()?;
            Ok(count)
        })
        .await
        .map_err(database_error)?;
    if changed == 0 {
        return Err(error(
            StatusCode::NOT_FOUND,
            "not_found",
            "Устройство не найдено",
        ));
    }
    app.cancel(id);
    app.wake();
    Ok(StatusCode::NO_CONTENT)
}
async fn publish(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(channel): Path<i64>,
    Json(request): Json<PublishRequest>,
) -> ApiResult<Json<Message>> {
    let a = auth(&app, &headers).await?;
    app.limit(format!("publish:{}", a.device_id), 30)?;
    if !valid_text(&request.text) || uuid::Uuid::parse_str(&request.client_message_id).is_err() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_message",
            "Нужен текст до 4096 байт и уникальный идентификатор",
        ));
    }
    let cfg = app.config.clone();
    let ttl = app.retention.load(Ordering::Relaxed);
    let result=app.db.run(move |c| {
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        db::active(&tx,a.device_id)?;
        // Recheck access inside the write transaction: revocation cannot race a publish.
        let allowed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM memberships g JOIN devices d ON d.user_id=g.user_id WHERE g.channel_id=? AND d.id=? AND d.revoked=0)",params![channel,a.device_id],|r|r.get(0))?;
        if !allowed {return Ok(Err("forbidden"));}
        let sql=format!("SELECT {} FROM messages m JOIN users u ON u.id=m.sender_id WHERE m.device_id=? AND m.client_message_id=?",db::MESSAGE_COLUMNS);
        if let Some(existing)=tx.query_row(&sql,params![a.device_id,request.client_message_id],db::message_row).optional()? {
            if existing.channel_id!=channel || existing.text!=request.text {return Ok(Err("id_conflict"));}
            if existing.expires_at<=db::now() {return Ok(Err("expired"));}
            return Ok(Ok(existing));
        }
        // Read-only retries succeed even when the storage quota is exhausted.
        tx.commit()?;
        db::writable(c,&cfg)?;
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        db::active(&tx,a.device_id)?;
        let allowed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM memberships WHERE user_id=? AND channel_id=?)",params![a.user_id,channel],|r|r.get(0))?;
        if !allowed {return Ok(Err("forbidden"));}
        let message=Message{id:uuid::Uuid::new_v4().to_string(),seq:0,channel_id:channel,sender_id:a.user_id,sender_name:a.user_name,text:request.text,created_at:db::now(),expires_at:db::now()+ttl};
        tx.execute("INSERT INTO messages(id,channel_id,sender_id,device_id,client_message_id,text,created_at,expires_at) VALUES(?,?,?,?,?,?,?,?)",params![message.id,channel,message.sender_id,a.device_id,request.client_message_id,message.text,message.created_at,message.expires_at])?;
        let seq=tx.last_insert_rowid();tx.commit()?;Ok(Ok(Message{seq,..message}))
    }).await.map_err(database_error)?;
    let message = result.map_err(|code| match code {
        "id_conflict" => error(
            StatusCode::CONFLICT,
            code,
            "Идентификатор уже использован для другого сообщения",
        ),
        "expired" => error(
            StatusCode::GONE,
            code,
            "Срок хранения отправленного сообщения истёк",
        ),
        _ => error(StatusCode::FORBIDDEN, code, "Нет доступа к каналу"),
    })?;
    app.wake();
    Ok(Json(message))
}
#[derive(Deserialize, Default)]
struct Position {
    #[serde(default)]
    after: i64,
}
async fn stream(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(position): Query<Position>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    app.limit(format!("stream:{}", peer.ip()), 120)?;
    let a = auth(&app, &headers).await?;
    if position.after < 0 {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "Недопустимая позиция потока",
        ));
    }
    if let Some((_, old)) = app.sessions.lock().unwrap().get(&a.device_id) {
        old.send_replace(true);
    }
    let permit = tokio::time::timeout(Duration::from_secs(3), app.slots.clone().acquire_owned())
        .await
        .map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "connection_limit",
                "Сервер занят",
            )
        })?
        .map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "stopping",
                "Сервер останавливается",
            )
        })?;
    Ok(ws
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(65536)
        .max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            let _ = connection(app, a, position.after, socket).await;
        })
        .into_response())
}
struct SessionGuard {
    app: Arc<App>,
    device: i64,
    id: uuid::Uuid,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        let mut sessions = self.app.sessions.lock().unwrap();
        if sessions
            .get(&self.device)
            .is_some_and(|(id, _)| *id == self.id)
        {
            sessions.remove(&self.device);
        }
    }
}
async fn send(socket: &mut WebSocket, event: &ServerEvent) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(Frame::Text(serde_json::to_string(event)?.into())),
    )
    .await??;
    Ok(())
}
async fn pump(
    app: &App,
    a: &Auth,
    socket: &mut WebSocket,
    cursor: &mut i64,
    replay: bool,
    cancelled: &watch::Receiver<bool>,
) -> Result<bool> {
    let mut gap = false;
    loop {
        let a = a.clone();
        let after = *cursor;
        let (messages, next, lost) = app.db.run(move |c| db::backlog(c, &a, after)).await?;
        gap |= lost;
        let full = messages.len() == PAGE_SIZE;
        for message in messages {
            // Consult this connection's cancellation flag. The session-map entry
            // may already belong to a replacement connection for the same device.
            anyhow::ensure!(!*cancelled.borrow(), "session_closed");
            // A slow replay must not expose text whose TTL elapsed while sending the page.
            if message.expires_at > db::now() {
                send(socket, &ServerEvent::Message { message, replay }).await?;
            } else {
                gap = true;
            }
        }
        if next > *cursor {
            *cursor = next;
            send(socket, &ServerEvent::Progress { cursor: next }).await?;
        }
        if !full {
            break;
        }
    }
    Ok(gap)
}
async fn connection(app: Arc<App>, a: Auth, mut cursor: i64, mut socket: WebSocket) -> Result<()> {
    let mut changed = app.changed.subscribe();
    let id = uuid::Uuid::new_v4();
    let (cancel, mut cancelled) = watch::channel(false);
    if let Some((_, old)) = app
        .sessions
        .lock()
        .unwrap()
        .insert(a.device_id, (id, cancel))
    {
        old.send_replace(true);
    }
    let _guard = SessionGuard {
        app: app.clone(),
        device: a.device_id,
        id,
    };
    let check = a.clone();
    let list = app
        .db
        .run(move |c| {
            let tx = c.transaction()?;
            db::active(&tx, check.device_id)?;
            db::channels(&tx, check.user_id)
        })
        .await?;
    send(
        &mut socket,
        &ServerEvent::Hello {
            user_id: a.user_id,
            device_id: a.device_id,
            channels: list.clone(),
        },
    )
    .await?;
    let high = app.db.run(|c| db::highwater(c)).await?;
    if cursor > high {
        cursor = 0;
        send(&mut socket, &ServerEvent::Reset { cursor: 0 }).await?;
    }
    let gap = pump(&app, &a, &mut socket, &mut cursor, true, &cancelled).await?;
    send(&mut socket, &ServerEvent::Synced { cursor, gap }).await?;
    let mut known_channels = list;
    let mut ping = tokio::time::interval(Duration::from_secs(app.config.heartbeat_seconds));
    ping.tick().await;
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut awaiting: Option<Instant> = None;
    let mut last_ack = 0;
    let mut pong_check =
        tokio::time::interval(Duration::from_secs(app.config.pong_timeout_seconds));
    pong_check.tick().await;
    loop {
        tokio::select! {
            _=cancelled.changed()=>break,
            result=changed.changed()=> {
                if result.is_err(){break;}
                let device=a.device_id;
                let active=app.db.run(move |c|Ok(db::active(c,device).is_ok())).await?;
                if !active {let _=socket.send(Frame::Close(Some(axum::extract::ws::CloseFrame{code:4001,reason:"revoked".into()}))).await;break;}
                let user=a.user_id;let channels=app.db.run(move |c|db::channels(c,user)).await?;
                if channels!=known_channels {known_channels=channels.clone();send(&mut socket,&ServerEvent::Channels{channels}).await?;}
                pump(&app,&a,&mut socket,&mut cursor,false,&cancelled).await?;
            }
            _=ping.tick()=> {if awaiting.is_none() {tokio::time::timeout(Duration::from_secs(5),socket.send(Frame::Ping(vec![1].into()))).await??;awaiting=Some(Instant::now());}}
            _=pong_check.tick()=> {if awaiting.is_some_and(|t|t.elapsed().as_secs()>=app.config.pong_timeout_seconds){break;}}
            incoming=socket.next()=>match incoming {
                Some(Ok(Frame::Text(text)))=> {
                    if app.limit(format!("frames:{}",a.device_id),2400).is_err(){break;}
                    let ClientEvent::Ack{cursor:ack}=serde_json::from_str(&text)?;
                    if ack<0 || ack>cursor {break;}
                    if ack<=last_ack {continue;}
                    let device=a.device_id;
                    let cfg=app.config.clone();
                    app.db.run(move |c| {db::writable(c,&cfg)?;db::active(c,device)?;c.execute("UPDATE devices SET acked=MAX(acked,?) WHERE id=? AND revoked=0",params![ack,device])?;Ok(())}).await?;
                    last_ack=ack;
                }
                Some(Ok(Frame::Pong(_)))=>awaiting=None,
                Some(Ok(Frame::Ping(_)))=>{},
                Some(Ok(Frame::Close(_)))|None|Some(Err(_))=>break,
                _=>break,
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.close()).await;
    Ok(())
}

pub async fn listen_admin(app: Arc<App>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = app.config.admin_socket();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let socket = tokio::net::UnixDatagram::bind(&path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let mut buffer = [0u8; 16];
    loop {
        socket.recv(&mut buffer).await?;
        // Cancel revoked streams even when they are still paging a long replay.
        let devices: Vec<i64> = app.sessions.lock().unwrap().keys().copied().collect();
        let inactive = app
            .db
            .run(move |c| {
                let mut result = Vec::new();
                for device in devices {
                    if db::active(c, device).is_err() {
                        result.push(device);
                    }
                }
                Ok(result)
            })
            .await?;
        for device in inactive {
            app.cancel(device);
        }
        app.wake();
    }
}
pub fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}
