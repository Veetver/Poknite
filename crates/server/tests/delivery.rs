use futures_util::{SinkExt, StreamExt};
use poknite_protocol::{ClientEvent, EnrollResponse, Message, PublishRequest, ServerEvent};
use poknite_server::{
    config::Config,
    db,
    http::{self, App},
};
use reqwest::{Client, StatusCode};
use rusqlite::params;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tempfile::TempDir;
use tokio::{net::TcpStream, task::JoinHandle};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message as Frame, client::IntoClientRequest},
};

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[tokio::test]
async fn e2ee_secure_default_ciphertext_replay_retry_and_membership_rotation() {
    use poknite_protocol::e2ee::Member;
    let server = Server::start_with_policy(Config::default()).await;
    assert!(server.app.config.e2ee_required);
    assert_eq!(
        server
            .publish(A, 1, &uuid::Uuid::new_v4().to_string(), "Открытый текст")
            .await
            .status(),
        StatusCode::UPGRADE_REQUIRED
    );
    let members: Vec<Member> = server
        .client
        .get(format!("{}/v2/conversations/1/e2ee-members", server.base))
        .bearer_auth(A)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(members.len(), 3);
    let dir = tempfile::tempdir().unwrap();
    let receiver_dir = tempfile::tempdir().unwrap();
    let sender = poknite_client::Store::open(dir.path()).unwrap();
    let receiver = poknite_client::Store::open(receiver_dir.path()).unwrap();
    let code = sender
        .create_conversation_key(&server.base, 1, &members)
        .unwrap();
    receiver
        .import_conversation_key(&server.base, 1, &members, &code)
        .unwrap();
    let request = PublishRequest {
        client_message_id: uuid::Uuid::new_v4().to_string(),
        text: "Секрет, которого нет на сервере".into(),
        mentions: vec![],
    };
    let encrypted = sender
        .seal_publish(&server.base, 1, 1, 1, &members, &request)
        .unwrap();
    let r = server
        .publish(A, 1, &request.client_message_id, &encrypted.text)
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    let wire: Message = r.json().await.unwrap();
    assert_eq!(
        receiver.unseal_message(&server.base, &wire).unwrap().text,
        request.text
    );
    let again: Message = server
        .publish(A, 1, &request.client_message_id, &encrypted.text)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(again, wire);
    let stored = server
        .app
        .db
        .run(|c| {
            Ok(c.query_row("SELECT text FROM messages LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(stored, encrypted.text);
    assert!(!stored.contains(&request.text));
    assert_eq!(
        server
            .publish(C, 1, &request.client_message_id, &encrypted.text)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut socket = server.socket(C, 0).await;
    loop {
        if let ServerEvent::Message {
            message,
            replay,
            notify,
        } = event(&mut socket).await
        {
            assert!(replay && notify);
            assert_eq!(message.text, encrypted.text);
            assert!(
                receiver
                    .receive_secure(&server.base, &message, true, 2, true)
                    .unwrap()
            );
            break;
        }
    }
    assert_eq!(receiver.pending().unwrap().len(), 1);
    server
        .app
        .db
        .run(|c| {
            c.execute("UPDATE devices SET revoked=1 WHERE id=3", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let fresh = PublishRequest {
        client_message_id: uuid::Uuid::new_v4().to_string(),
        text: "После исключения".into(),
        mentions: vec![],
    };
    let stale = sender
        .seal_publish(&server.base, 1, 1, 1, &members, &fresh)
        .unwrap();
    assert_eq!(
        server
            .publish(A, 1, &fresh.client_message_id, &stale.text)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let remaining = members
        .into_iter()
        .filter(|m| m.device_id != 3)
        .collect::<Vec<_>>();
    sender
        .create_conversation_key(&server.base, 1, &remaining)
        .unwrap();
    let new_request = PublishRequest {
        client_message_id: uuid::Uuid::new_v4().to_string(),
        ..fresh
    };
    let sealed = sender
        .seal_publish(&server.base, 1, 1, 1, &remaining, &new_request)
        .unwrap();
    let new_wire: Message = server
        .publish(A, 1, &new_request.client_message_id, &sealed.text)
        .await
        .json()
        .await
        .unwrap();
    assert!(receiver.unseal_message(&server.base, &new_wire).is_err());
    // Direct conversations use the same authenticated envelope and delivery path.
    server
        .app
        .db
        .run(|c| {
            c.execute("UPDATE devices SET revoked=0 WHERE id=3", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let direct: poknite_protocol::Channel = server
        .client
        .post(format!("{}/v2/conversations/direct", server.base))
        .bearer_auth(A)
        .json(&serde_json::json!({"user_id":2}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let dm: Vec<Member> = server
        .client
        .get(format!(
            "{}/v2/conversations/{}/e2ee-members",
            server.base, direct.id
        ))
        .bearer_auth(A)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = sender
        .create_conversation_key(&server.base, direct.id, &dm)
        .unwrap();
    receiver
        .import_conversation_key(&server.base, direct.id, &dm, &code)
        .unwrap();
    let request = PublishRequest {
        client_message_id: uuid::Uuid::new_v4().to_string(),
        text: "Личный секрет".into(),
        mentions: vec![],
    };
    let encrypted = sender
        .seal_publish(&server.base, 1, 1, direct.id, &dm, &request)
        .unwrap();
    let wire: Message = server
        .publish(A, direct.id, &request.client_message_id, &encrypted.text)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        receiver.unseal_message(&server.base, &wire).unwrap().text,
        request.text
    );
}
struct Server {
    _dir: TempDir,
    app: Arc<App>,
    base: String,
    task: JoinHandle<()>,
    client: Client,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start(mut config: Config) -> Self {
        // Existing tests exercise v2 compatibility. Dedicated E2EE tests below
        // explicitly turn the secure default back on.
        config.e2ee_required = false;
        Self::start_with_policy(config).await
    }
    async fn start_with_policy(mut config: Config) -> Self {
        let dir = tempfile::tempdir().unwrap();
        config.data_dir = dir.path().into();
        let app = App::new(config).unwrap();
        app.db.run(|c| {
            c.execute_batch("INSERT INTO users(id,name) VALUES(1,'Аня'),(2,'Борис'); INSERT INTO channels(id,name) VALUES(2,'Закрытый'); INSERT INTO memberships VALUES(1,1),(2,1),(2,2);")?;
            for (id, user, token) in [(1, 1, A), (2, 1, B), (3, 2, C)] {
                c.execute("INSERT INTO devices(id,user_id,name,token_hash,created_at) VALUES(?,?,?,?,?)", params![id,user,format!("Устройство {id}"),db::hash(token),db::now()])?;
            }
            Ok(())
        }).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let router = http::router(app.clone());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            _dir: dir,
            app,
            base,
            task,
            client: Client::new(),
        }
    }
    async fn publish(&self, token: &str, channel: i64, id: &str, text: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/v2/conversations/{channel}/messages", self.base))
            .bearer_auth(token)
            .json(&PublishRequest {
                client_message_id: id.into(),
                text: text.into(),
                mentions: Vec::new(),
            })
            .send()
            .await
            .unwrap()
    }
    async fn message(&self, token: &str, channel: i64, text: &str) -> Message {
        let response = self
            .publish(token, channel, &uuid::Uuid::new_v4().to_string(), text)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }
    async fn socket(&self, token: &str, cursor: i64) -> Socket {
        let mut request = format!(
            "{}/v2/stream?after={cursor}",
            self.base.replace("http://", "ws://")
        )
        .into_client_request()
        .unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        tokio_tungstenite::connect_async(request).await.unwrap().0
    }
    async fn invite(&self, user: i64, ttl: i64) -> String {
        let code = db::secret().unwrap();
        let hash = db::hash(&code);
        self.app
            .db
            .run(move |c| {
                c.execute(
                    "INSERT INTO invitations VALUES(?,?,?)",
                    params![hash, user, db::now() + ttl],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        code
    }
    async fn enroll(&self, invitation: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/v2/devices/enroll", self.base))
            .json(&serde_json::json!({"invitation":invitation,"device_name":"Телефон"}))
            .send()
            .await
            .unwrap()
    }
    async fn restart(&mut self) {
        self.task.abort();
        self.app = App::new(self.app.config.clone()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", listener.local_addr().unwrap());
        let router = http::router(self.app.clone());
        self.task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
    }
}
async fn event(socket: &mut Socket) -> ServerEvent {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Frame::Text(text) => {
                    let e = serde_json::from_str(&text).unwrap();
                    if !matches!(e, ServerEvent::Profiles { .. }) {
                        return e;
                    }
                }
                Frame::Ping(body) => socket.send(Frame::Pong(body)).await.unwrap(),
                frame => panic!("Unexpected frame: {frame:?}"),
            }
        }
    })
    .await
    .expect("stream stalled")
}
async fn ack(socket: &mut Socket, cursor: i64) {
    socket
        .send(Frame::Text(
            serde_json::to_string(&ClientEvent::Ack { cursor })
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
}
async fn initial(socket: &mut Socket) -> (Vec<Message>, i64, bool) {
    assert!(matches!(event(socket).await, ServerEvent::Hello { .. }));
    let mut messages = Vec::new();
    loop {
        match event(socket).await {
            ServerEvent::Message {
                message, replay, ..
            } => {
                assert!(replay);
                messages.push(message);
            }
            ServerEvent::Progress { .. }
            | ServerEvent::Reset { .. }
            | ServerEvent::State { .. } => {}
            ServerEvent::Synced { cursor, gap } => return (messages, cursor, gap),
            other => panic!("Unexpected event: {other:?}"),
        }
    }
}

#[tokio::test]
async fn independent_devices_replay_ack_and_lost_response_idempotency() {
    let server = Server::start(Config::default()).await;
    let id = uuid::Uuid::new_v4().to_string();
    let first: Message = server
        .publish(A, 1, &id, "Первое")
        .await
        .json()
        .await
        .unwrap();
    let retry: Message = server
        .publish(A, 1, &id, "Первое")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(first, retry);
    assert_eq!(
        server.publish(A, 1, &id, "Другой текст").await.status(),
        StatusCode::CONFLICT
    );
    let mut phone = server.socket(A, 0).await;
    let mut desktop = server.socket(B, 0).await;
    assert_eq!(initial(&mut phone).await.0, vec![first.clone()]);
    assert_eq!(initial(&mut desktop).await.0, vec![first]);
    let second = server.message(A, 1, "Второе").await;
    for socket in [&mut phone, &mut desktop] {
        assert!(
            matches!(event(socket).await,ServerEvent::Message {message,replay:false,..} if message.id==second.id)
        );
        assert!(matches!(event(socket).await,ServerEvent::Progress {cursor} if cursor==second.seq));
    }
    ack(&mut phone, second.seq).await;
    let mut acknowledged = false;
    for _ in 0..50 {
        let values = server
            .app
            .db
            .run(|c| {
                let mut q = c.prepare("SELECT acked FROM devices WHERE id IN(1,2) ORDER BY id")?;
                Ok(q.query_map([], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap();
        if values[0] == second.seq {
            assert_eq!(values[1], 0);
            acknowledged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        acknowledged,
        "The first device's acknowledgement was not persisted"
    );
    desktop.close(None).await.unwrap();
    let mut desktop = server.socket(B, second.seq - 1).await;
    assert_eq!(initial(&mut desktop).await.0, vec![second]);
}

#[tokio::test]
async fn concurrent_invitation_is_single_use_and_device_limit_preserves_invite() {
    let server = Server::start(Config::default()).await;
    let invitation = server.invite(1, 900).await;
    let (one, two) = tokio::join!(server.enroll(&invitation), server.enroll(&invitation));
    assert!([one.status(), two.status()].contains(&StatusCode::OK));
    assert!([one.status(), two.status()].contains(&StatusCode::UNAUTHORIZED));
    let accepted: EnrollResponse = if one.status() == StatusCode::OK {
        one.json().await.unwrap()
    } else {
        two.json().await.unwrap()
    };
    let raw = accepted.token.clone();
    let hash = server
        .app
        .db
        .run(move |c| {
            Ok(c.query_row(
                "SELECT token_hash FROM devices WHERE id=?",
                [accepted.device_id],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(hash, db::hash(&raw));
    assert_ne!(hash, raw);
    let fresh = server.invite(1, 900).await;
    assert_eq!(server.enroll(&fresh).await.status(), StatusCode::CONFLICT);
    let revoked = server
        .client
        .delete(format!("{}/v2/devices/2", server.base))
        .bearer_auth(A)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(server.enroll(&fresh).await.status(), StatusCode::OK);
    let expired = server.invite(2, -1).await;
    assert_eq!(
        server.enroll(&expired).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn authentication_acl_utf8_limits_and_cache_headers() {
    let server = Server::start(Config::default()).await;
    for path in ["/v2/channels", "/v2/devices"] {
        assert_eq!(
            server
                .client
                .get(format!("{}{path}", server.base))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let valid = server
            .client
            .get(format!("{}{path}", server.base))
            .bearer_auth(A)
            .send()
            .await
            .unwrap();
        assert_eq!(valid.status(), StatusCode::OK);
        assert_eq!(valid.headers()["cache-control"], "no-store");
        assert!(!valid.text().await.unwrap().contains(A));
    }
    let id = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        server.publish(A, 2, &id, "Закрыто").await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        server.publish(A, 1, &id, &"я".repeat(2049)).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        server.publish(A, 1, "not-a-uuid", "Текст").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        server.publish(A, 1, &id, " \n\t ").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        server.publish(A, 1, &id, "a\0").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        server.publish(A, 1, &id, &"a".repeat(40000)).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        server.publish(A, 1, &id, &"я".repeat(2048)).await.status(),
        StatusCode::OK
    );
    let response = server
        .client
        .delete(format!("{}/v2/devices/3", server.base))
        .bearer_auth(A)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let private = server.message(C, 2, "Только Борису").await;
    let mut socket = server.socket(A, 0).await;
    assert!(
        initial(&mut socket)
            .await
            .0
            .iter()
            .all(|m| m.id != private.id)
    );
}

#[tokio::test]
async fn revoked_device_cannot_publish_upgrade_or_continue_stream() {
    let server = Server::start(Config::default()).await;
    let mut socket = server.socket(B, 0).await;
    initial(&mut socket).await;
    assert_eq!(
        server
            .client
            .delete(format!("{}/v2/devices/2", server.base))
            .bearer_auth(A)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let result = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap();
    assert!(matches!(
        result,
        Some(Ok(Frame::Close(_))) | None | Some(Err(_))
    ));
    assert_eq!(
        server
            .publish(B, 1, &uuid::Uuid::new_v4().to_string(), "Запрещено")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        server
            .client
            .get(format!("{}/v2/channels", server.base))
            .bearer_auth(B)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut request = format!("{}/v2/stream", server.base.replace("http://", "ws://"))
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {B}").parse().unwrap());
    assert!(
        matches!(tokio_tungstenite::connect_async(request).await,Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status()==401)
    );
}

#[tokio::test]
async fn expiration_is_immediate_without_cleanup_and_new_retention_is_only_for_new_messages() {
    let server = Server::start(Config::default()).await;
    let old = server.message(A, 1, "Старое").await;
    server.app.retention.store(60, Ordering::Relaxed);
    let fresh = server.message(A, 1, "Новое").await;
    assert_eq!(old.expires_at - old.created_at, 86400);
    assert_eq!(fresh.expires_at - fresh.created_at, 60);
    let old_id = old.id.clone();
    server
        .app
        .db
        .run(move |c| {
            c.execute(
                "UPDATE messages SET expires_at=? WHERE id=?",
                params![db::now() - 1, old_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut socket = server.socket(B, 0).await;
    let (messages, cursor, gap) = initial(&mut socket).await;
    assert_eq!(messages, vec![fresh]);
    assert_eq!(cursor, 2);
    assert!(gap);
    socket.close(None).await.unwrap();
    assert_eq!(server.app.db.run(db::cleanup).await.unwrap(), 1);
    let mut socket = server.socket(B, 0).await;
    let (messages, _, gap) = initial(&mut socket).await;
    assert_eq!(messages.len(), 1);
    assert!(gap);
}

#[tokio::test]
async fn cursor_ahead_of_database_resets_and_invalid_ack_never_advances_state() {
    let server = Server::start(Config::default()).await;
    let message = server.message(A, 1, "После восстановления базы").await;
    let mut socket = server.socket(A, 999).await;
    assert!(matches!(
        event(&mut socket).await,
        ServerEvent::Hello { .. }
    ));
    assert!(matches!(
        event(&mut socket).await,
        ServerEvent::Reset { cursor: 0 }
    ));
    assert!(matches!(
        event(&mut socket).await,
        ServerEvent::State { .. }
    ));
    assert!(
        matches!(event(&mut socket).await,ServerEvent::Message {message:m,..} if m.id==message.id)
    );
    assert!(matches!(
        event(&mut socket).await,
        ServerEvent::Progress { cursor: 1 }
    ));
    assert!(matches!(
        event(&mut socket).await,
        ServerEvent::Synced { cursor: 1, .. }
    ));
    ack(&mut socket, 999).await;
    let result = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap();
    assert!(matches!(
        result,
        Some(Ok(Frame::Close(_))) | None | Some(Err(_))
    ));
    let saved = server
        .app
        .db
        .run(|c| {
            Ok(
                c.query_row("SELECT acked FROM devices WHERE id=1", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(saved, 0);
}

#[tokio::test]
async fn live_membership_change_updates_stream_and_rechecks_publish_access() {
    let server = Server::start(Config::default()).await;
    let mut socket = server.socket(A, 0).await;
    initial(&mut socket).await;
    server
        .app
        .db
        .run(|c| {
            c.execute("INSERT INTO memberships VALUES(1,2)", [])?;
            Ok(())
        })
        .await
        .unwrap();
    server.app.wake();
    assert!(
        matches!(event(&mut socket).await,ServerEvent::State {channels,..} if channels.len()==2)
    );
    let message = server.message(C, 2, "Теперь видно").await;
    assert!(
        matches!(event(&mut socket).await,ServerEvent::Message {message:m,..} if m.id==message.id)
    );
    assert!(matches!(
        event(&mut socket).await,
        ServerEvent::Progress { .. }
    ));
    server
        .app
        .db
        .run(|c| {
            c.execute(
                "DELETE FROM memberships WHERE user_id=1 AND channel_id=2",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    server.app.wake();
    assert!(
        matches!(event(&mut socket).await,ServerEvent::State {channels,..} if channels.len()==1)
    );
    assert_eq!(
        server
            .publish(A, 2, &uuid::Uuid::new_v4().to_string(), "Уже нельзя")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn storage_quota_rejects_new_messages_but_retry_returns_stored_result() {
    let server = Server::start(Config::default()).await;
    let id = uuid::Uuid::new_v4().to_string();
    let first: Message = server
        .publish(A, 1, &id, "Доставлено")
        .await
        .json()
        .await
        .unwrap();
    server.app.db.run(|c| {
        let count:i64 = c.pragma_query_value(None,"page_count",|r|r.get(0))?;
        c.pragma_update(None,"max_page_count",count)?;
        // Fill free space in current pages without consuming the HTTP publishing rate budget.
        loop {
            let result = c.execute("INSERT INTO messages(id,channel_id,sender_id,device_id,client_message_id,text,created_at,expires_at) VALUES(?,1,1,1,?,?,?,?)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),"a".repeat(4096),db::now(),db::now()+86400]);
            if result.is_err() {break;}
        }
        Ok(())
    }).await.unwrap();
    let retry: Message = server
        .publish(A, 1, &id, "Доставлено")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(retry, first);
    let failed = server
        .publish(A, 1, &uuid::Uuid::new_v4().to_string(), &"я".repeat(2048))
        .await;
    assert_eq!(failed.status(), StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(
        failed.json::<serde_json::Value>().await.unwrap()["code"],
        "storage_full"
    );
    let count = server
        .app
        .db
        .run(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM messages WHERE id=?",
                [first.id],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn cleanup_is_batched_and_revoked_metadata_is_reclaimed() {
    let server = Server::start(Config::default()).await;
    server.app.db.run(|c| {
        for _ in 0..300 {
            c.execute("INSERT INTO messages(id,channel_id,sender_id,device_id,client_message_id,text,created_at,expires_at) VALUES(?,1,1,2,?,'x',?,?)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),db::now()-2,db::now()-1])?;
        }
        c.execute("UPDATE devices SET revoked=1 WHERE id=2",[])?;
        Ok(())
    }).await.unwrap();
    assert_eq!(server.app.db.run(db::cleanup).await.unwrap(), 256);
    assert_eq!(server.app.db.run(db::cleanup).await.unwrap(), 44);
    let exists = server
        .app
        .db
        .run(|c| {
            Ok(
                c.query_row("SELECT EXISTS(SELECT 1 FROM devices WHERE id=2)", [], |r| {
                    r.get::<_, bool>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert!(!exists);
    let mut socket = server.socket(A, 0).await;
    let (messages, cursor, gap) = initial(&mut socket).await;
    assert!(messages.is_empty());
    assert_eq!(cursor, 300);
    assert!(gap);
}

#[tokio::test]
async fn persisted_messages_and_device_identity_survive_server_restart() {
    let mut server = Server::start(Config::default()).await;
    let id = uuid::Uuid::new_v4().to_string();
    let stored: Message = server
        .publish(A, 1, &id, "Переживает перезапуск")
        .await
        .json()
        .await
        .unwrap();
    server.restart().await;
    let retry: Message = server
        .publish(A, 1, &id, "Переживает перезапуск")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(retry, stored);
    let mut socket = server.socket(B, 0).await;
    assert_eq!(initial(&mut socket).await.0, vec![stored]);
}

#[tokio::test]
async fn bounded_connections_and_replacement_of_same_device() {
    let config = Config {
        max_connections: 1,
        ..Config::default()
    };
    let server = Server::start(config).await;
    let mut old = server.socket(A, 0).await;
    initial(&mut old).await;
    let mut replacement = server.socket(A, 0).await;
    initial(&mut replacement).await;
    let result = tokio::time::timeout(Duration::from_secs(2), old.next())
        .await
        .unwrap();
    assert!(matches!(
        result,
        Some(Ok(Frame::Close(_))) | None | Some(Err(_))
    ));
    let mut request = format!("{}/v2/stream", server.base.replace("http://", "ws://"))
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {C}").parse().unwrap());
    assert!(
        matches!(tokio_tungstenite::connect_async(request).await,Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status()==503)
    );
    replacement.close(None).await.unwrap();
    let mut admitted = server.socket(C, 0).await;
    initial(&mut admitted).await;
}

#[tokio::test]
async fn replacing_device_during_long_replay_cancels_old_connection_with_spare_slots() {
    let server = Server::start(Config::default()).await;
    server.app.db.run(|c| {
        let tx=c.transaction()?;
        for _ in 0..5000 {
            tx.execute("INSERT INTO messages(id,channel_id,sender_id,device_id,client_message_id,text,created_at,expires_at) VALUES(?,1,1,1,?,?,?,?)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),"x".repeat(4096),db::now(),db::now()+86400])?;
        }
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let mut old = server.socket(B, 0).await;
    assert!(matches!(event(&mut old).await, ServerEvent::Hello { .. }));
    assert!(matches!(event(&mut old).await, ServerEvent::State { .. }));
    assert!(matches!(event(&mut old).await, ServerEvent::Message { .. }));
    // Suspend the old sender through TCP backpressure before its map entry is
    // replaced, so resuming it exercises the cancellation-map race.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut replacement = server.socket(B, 5000).await;
    assert_eq!(initial(&mut replacement).await.1, 5000);
    let received = tokio::time::timeout(Duration::from_secs(3), async {
        let mut count = 1;
        loop {
            match old.next().await {
                Some(Ok(Frame::Text(body))) => {
                    if matches!(
                        serde_json::from_str::<ServerEvent>(&body).unwrap(),
                        ServerEvent::Message { .. }
                    ) {
                        count += 1;
                    }
                }
                Some(Ok(Frame::Close(_))) | None | Some(Err(_)) => return count,
                _ => {}
            }
        }
    })
    .await
    .expect("Old replay was not cancelled");
    assert!(
        received < 5000,
        "Cancelled connection kept replaying all messages"
    );
}

async fn administration(server: &Server) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = poknite_server::management::router(server.app.clone());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (base, task)
}
fn managed_config() -> Config {
    let mut cfg = Config::default();
    cfg.management.enabled = true;
    cfg
}
async fn make_admin(server: &Server) {
    server
        .app
        .db
        .run(|c| {
            c.execute("INSERT OR IGNORE INTO user_roles VALUES(1,1)", [])?;
            Ok(())
        })
        .await
        .unwrap();
}
async fn admin_call(
    server: &Server,
    base: &str,
    path: &str,
    method: reqwest::Method,
    token: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    server
        .client
        .request(method, format!("{base}/v2/admin/{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}
#[tokio::test]
async fn administration_is_separate_role_and_peer_protected_and_v1_requires_update() {
    let server = Server::start(managed_config()).await;
    make_admin(&server).await;
    let (base, task) = administration(&server).await;
    let r = admin_call(
        &server,
        &base,
        "users",
        reqwest::Method::GET,
        A,
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let r = admin_call(
        &server,
        &base,
        "users",
        reqwest::Method::GET,
        C,
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = admin_call(
        &server,
        &server.base,
        "users",
        reqwest::Method::GET,
        A,
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        server
            .client
            .get(format!("{}/v1/channels", server.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UPGRADE_REQUIRED
    );
    task.abort();
    let mut cfg = managed_config();
    cfg.management.listen = "192.168.90.2:8444".parse().unwrap();
    cfg.management.allowed_subnets = vec!["192.168.90.0/24".into()];
    let server = Server::start(cfg).await;
    make_admin(&server).await;
    let (base, task) = administration(&server).await;
    let r = server
        .client
        .get(format!("{base}/v2/admin/users"))
        .bearer_auth(A)
        .header("x-forwarded-for", "192.168.90.3")
        .header("x-real-ip", "192.168.90.3")
        .header("forwarded", "for=192.168.90.3")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    // Public traffic stays usable after administration fails from this network.
    assert_eq!(
        server
            .publish(
                A,
                1,
                &uuid::Uuid::new_v4().to_string(),
                "Публичная переписка"
            )
            .await
            .status(),
        StatusCode::OK
    );
    task.abort();
}
#[tokio::test]
async fn conflicting_roles_denies_cover_legacy_memberships_and_delegation_and_last_admin() {
    use serde_json::json;
    let server = Server::start(managed_config()).await;
    make_admin(&server).await;
    let (base, task) = administration(&server).await;
    let r = admin_call(
        &server,
        &base,
        "roles",
        reqwest::Method::POST,
        A,
        json!({"name":"Нет отправки","allow":["send"],"deny":["send"]}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let role: poknite_protocol::Role = r.json().await.unwrap();
    let r = admin_call(
        &server,
        &base,
        "users/2",
        reqwest::Method::PUT,
        A,
        json!({"name":"Борис","roles":[2,role.id],"disabled":false}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        server
            .publish(
                C,
                1,
                &uuid::Uuid::new_v4().to_string(),
                "Запрет важнее разрешения"
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let r = admin_call(
        &server,
        &base,
        "users/1",
        reqwest::Method::DELETE,
        A,
        json!(null),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    let r = admin_call(
        &server,
        &base,
        "users/1",
        reqwest::Method::PUT,
        A,
        json!({"name":"Аня","roles":[2],"disabled":false}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    let r = admin_call(
        &server,
        &base,
        "roles",
        reqwest::Method::POST,
        A,
        json!({"name":"Управляющий пользователями","allow":["users","read"],"deny":[]}),
    )
    .await;
    let manager: poknite_protocol::Role = r.json().await.unwrap();
    let r = admin_call(
        &server,
        &base,
        "users/2",
        reqwest::Method::PUT,
        A,
        json!({"name":"Борис","roles":[2,manager.id]}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let r = admin_call(
        &server,
        &base,
        "users",
        reqwest::Method::POST,
        C,
        json!({"name":"Повышение","roles":[1]}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = admin_call(
        &server,
        &base,
        "users/1",
        reqwest::Method::DELETE,
        C,
        json!(null),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    // Channel group deny applies over the preserved personal grant.
    let r = admin_call(
        &server,
        &base,
        "channels/1/rules",
        reqwest::Method::PUT,
        A,
        json!([{"role_id":2,"allow":[],"deny":["read"]}]),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let list: Vec<poknite_protocol::Channel> = server
        .client
        .get(format!("{}/v2/conversations", server.base))
        .bearer_auth(C)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().all(|c| c.id != 1));
    task.abort();
}
#[tokio::test]
async fn direct_dialog_is_unique_private_even_from_admin_and_survives_loss_of_common_channels() {
    use serde_json::json;
    let server = Server::start(Config::default()).await;
    server.app.db.run(|c|{c.execute("INSERT INTO users(id,name) VALUES(3,'Админ')",[])?;c.execute("INSERT INTO user_roles VALUES(3,1)",[])?;c.execute("INSERT INTO devices(id,user_id,name,token_hash,created_at) VALUES(4,3,'Админ',?,?)",params![db::hash(&"d".repeat(64)),db::now()])?;Ok(())}).await.unwrap();
    let create = |token: &str, peer: i64| {
        server
            .client
            .post(format!("{}/v2/conversations/direct", server.base))
            .bearer_auth(token)
            .json(&json!({"user_id":peer}))
    };
    let (first, second) = tokio::join!(create(A, 2).send(), create(C, 1).send());
    let first: poknite_protocol::Conversation = first.unwrap().json().await.unwrap();
    let second: poknite_protocol::Conversation = second.unwrap().json().await.unwrap();
    assert_eq!(first.id, second.id);
    let private = server.message(A, first.id, "Только двум").await;
    let mut admin = server.socket(&"d".repeat(64), 0).await;
    assert!(
        initial(&mut admin)
            .await
            .0
            .iter()
            .all(|m| m.id != private.id)
    );
    assert_eq!(
        server
            .publish(
                &"d".repeat(64),
                first.id,
                &uuid::Uuid::new_v4().to_string(),
                "Чужой диалог"
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    server
        .app
        .db
        .run(|c| {
            c.execute(
                "DELETE FROM memberships WHERE user_id=2 AND channel_id=1",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    server.app.wake();
    assert_eq!(
        server
            .publish(
                A,
                first.id,
                &uuid::Uuid::new_v4().to_string(),
                "Общий канал исчез"
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut own = server.socket(C, 0).await;
    assert!(initial(&mut own).await.0.iter().any(|m| m.id == private.id));
    let existing: poknite_protocol::Conversation =
        create(A, 2).send().await.unwrap().json().await.unwrap();
    assert_eq!(existing.id, first.id);
    assert!(
        !existing
            .actions
            .contains(&poknite_protocol::Permission::Send)
    );
}
#[tokio::test]
async fn unicode_mentions_route_notifications_validate_targets_and_retries() {
    use serde_json::json;
    let server = Server::start(Config::default()).await;
    server
        .app
        .db
        .run(|c| {
            c.execute("INSERT INTO users(id,name) VALUES(3,'Вера')", [])?;
            c.execute("INSERT INTO memberships VALUES(3,1)", [])?;
            c.execute(
                "INSERT INTO devices(id,user_id,name,token_hash,created_at) VALUES(4,3,'Вера',?,?)",
                params![db::hash(&"d".repeat(64)), db::now()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let body = json!({"client_message_id":id,"text":"😀 @Борис и @Вера","mentions":[{"user_id":2,"start":2,"end":8},{"user_id":3,"start":11,"end":16}]});
    let publish = |body: &serde_json::Value| {
        server
            .client
            .post(format!("{}/v2/conversations/1/messages", server.base))
            .bearer_auth(A)
            .json(body)
    };
    let r = publish(&body).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let original: Message = r.json().await.unwrap();
    assert_eq!(original.mentions.len(), 2);
    let again: Message = publish(&body).send().await.unwrap().json().await.unwrap();
    assert_eq!(again.id, original.id);
    let mut changed = body.clone();
    changed["mentions"] = json!([]);
    assert_eq!(
        publish(&changed).send().await.unwrap().status(),
        StatusCode::CONFLICT
    );
    for (token, expected) in [
        (A, false),
        (C, true),
        (
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            true,
        ),
    ] {
        let mut socket = server.socket(token, 0).await;
        assert!(matches!(
            event(&mut socket).await,
            ServerEvent::Hello { .. }
        ));
        assert!(matches!(
            event(&mut socket).await,
            ServerEvent::State { .. }
        ));
        assert!(
            matches!(event(&mut socket).await,ServerEvent::Message{notify,replay:true,..} if notify==expected)
        );
    }
    let invalid = json!({"client_message_id":uuid::Uuid::new_v4().to_string(),"text":"😀 @Борис","mentions":[{"user_id":2,"start":3,"end":9}]});
    assert_eq!(
        publish(&invalid).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    // Another device does not get a notification for its own user's message.
    let mut socket = server.socket(B, 0).await;
    assert!(matches!(
        event(&mut socket).await,
        ServerEvent::Hello { .. }
    ));
    assert!(matches!(
        event(&mut socket).await,
        ServerEvent::State { .. }
    ));
    assert!(matches!(
        event(&mut socket).await,
        ServerEvent::Message { notify: false, .. }
    ));
}
#[tokio::test]
async fn disable_revokes_invites_and_devices_closed_channel_keeps_history_and_profile_ids() {
    use serde_json::json;
    let server = Server::start(managed_config()).await;
    make_admin(&server).await;
    let (base, task) = administration(&server).await;
    let original = server.message(C, 1, "Авторство сохраняется").await;
    let invitation = server.invite(2, 900).await;
    let r = server
        .client
        .put(format!("{}/v2/profile", server.base))
        .bearer_auth(C)
        .json(&json!({"name":"Борис новый"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let renamed: poknite_protocol::User = r.json().await.unwrap();
    assert_eq!(renamed.id, 2);
    let r = admin_call(
        &server,
        &base,
        "channels/1",
        reqwest::Method::DELETE,
        A,
        json!(null),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        server
            .publish(C, 1, &uuid::Uuid::new_v4().to_string(), "Закрыто")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut own = server.socket(C, 0).await;
    let messages = initial(&mut own).await.0;
    assert_eq!(messages[0].id, original.id);
    assert_eq!(messages[0].sender_id, 2);
    assert_eq!(messages[0].sender_name, "Борис новый");
    let r = admin_call(
        &server,
        &base,
        "users/2",
        reqwest::Method::DELETE,
        A,
        json!(null),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        server.enroll(&invitation).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        server
            .publish(C, 1, &uuid::Uuid::new_v4().to_string(), "Отключено")
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut admin = server.socket(A, 0).await;
    assert_eq!(initial(&mut admin).await.0[0].sender_id, 2);
    task.abort();
}
#[test]
fn migration_preserves_ids_tokens_cursors_personal_grants_and_creates_verified_backup() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        data_dir: dir.path().into(),
        ..Config::default()
    };
    {
        let c = rusqlite::Connection::open(cfg.database()).unwrap();
        c.execute_batch(include_str!("../src/schema.sql")).unwrap();
        c.execute_batch(
            "INSERT INTO users(id,name) VALUES(7,'Старый');INSERT INTO memberships VALUES(7,1);",
        )
        .unwrap();
        c.execute("INSERT INTO devices(id,user_id,name,token_hash,created_at,acked) VALUES(9,7,'Устройство',?,1,42)",[db::hash(A)]).unwrap();
    }
    let c = db::open(&cfg).unwrap();
    let a = db::authenticate(&c, &db::hash(A)).unwrap().unwrap();
    assert_eq!(a.user_id, 7);
    assert_eq!(a.device_id, 9);
    assert_eq!(
        c.query_row("SELECT acked FROM devices WHERE id=9", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        42
    );
    assert_eq!(db::channels(&c, 7).unwrap()[0].id, 1);
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    let backups = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".backup.db")
                .then_some(p)
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let backup = rusqlite::Connection::open(&backups[0]).unwrap();
    assert_eq!(
        backup
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        backup
            .query_row("SELECT token_hash FROM devices WHERE id=9", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        db::hash(A)
    );
    drop(c);
    let _ = db::open(&cfg).unwrap();
}

#[tokio::test]
async fn nickname_color_is_random_persistent_validated_and_updates_message_author() {
    use poknite_protocol::{User, valid_color};
    let mut server = Server::start(Config::default()).await;
    let before: User = server
        .client
        .get(format!("{}/v2/profile", server.base))
        .bearer_auth(C)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(valid_color(&before.color));
    let message = server.message(C, 1, "цвет автора").await;
    assert_eq!(message.sender_color, before.color);
    let r = server
        .client
        .put(format!("{}/v2/profile", server.base))
        .bearer_auth(C)
        .json(&serde_json::json!({"name":"Борис","color":"#12ABef"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let user: User = r.json().await.unwrap();
    assert_eq!(user.id, before.id);
    assert_eq!(user.color, "#12abef");
    let r = server
        .client
        .put(format!("{}/v2/profile", server.base))
        .bearer_auth(C)
        .json(&serde_json::json!({"name":"Недопустимый","color":"#12zzef"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    server.restart().await;
    let after: User = server
        .client
        .get(format!("{}/v2/profile", server.base))
        .bearer_auth(C)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after.name, "Борис");
    assert_eq!(after.color, "#12abef");
    let mut socket = server.socket(A, 0).await;
    let copy = initial(&mut socket).await.0.remove(0);
    assert_eq!(copy.sender_color, "#12abef");
    assert_eq!(copy.sender_id, 2);
}

#[tokio::test]
async fn large_catalogs_and_state_preserve_frame_limit() {
    use poknite_protocol::{CATALOG_PAGE_SIZE, MAX_FRAME_BYTES, User};
    let server = Server::start(Config {
        max_users: 100,
        ..Config::default()
    })
    .await;
    server
        .app
        .db
        .run(|c| {
            for id in 3..=100 {
                c.execute(
                    "INSERT INTO users(id,name) VALUES(?,?)",
                    params![id, format!("{id}{}", "🙂".repeat(60))],
                )?;
                c.execute("INSERT INTO memberships VALUES(?,1)", [id])?;
            }
            for id in 3..=64 {
                c.execute(
                    "INSERT INTO channels(id,name) VALUES(?,?)",
                    params![id, format!("{id}{}", "🙂".repeat(60))],
                )?;
                c.execute("INSERT INTO memberships VALUES(1,?)", [id])?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut count = 0;
    loop {
        let response = server
            .client
            .get(format!("{}/v2/contacts?offset={count}", server.base))
            .bearer_auth(A)
            .send()
            .await
            .unwrap();
        let body = response.bytes().await.unwrap();
        assert!(body.len() <= MAX_FRAME_BYTES);
        let users: Vec<User> = serde_json::from_slice(&body).unwrap();
        count += users.len();
        if users.len() < CATALOG_PAGE_SIZE {
            break;
        }
    }
    assert_eq!(count, 99);
    let mut socket = server.socket(A, 0).await;
    let mut people = 0;
    let mut rooms = 0;
    let mut first = false;
    let mut complete = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Frame::Text(text) => {
                assert!(text.len() <= MAX_FRAME_BYTES);
                match serde_json::from_str::<ServerEvent>(&text).unwrap() {
                    ServerEvent::StatePart {
                        contacts,
                        channels,
                        first: begin,
                        last,
                        ..
                    } => {
                        if begin {
                            assert!(!first);
                            first = true;
                        }
                        assert!(!complete);
                        people += contacts.len();
                        rooms += channels.len();
                        complete = last;
                    }
                    ServerEvent::Synced { .. } => break,
                    _ => {}
                }
            }
            Frame::Ping(p) => socket.send(Frame::Pong(p)).await.unwrap(),
            other => panic!("{other:?}"),
        }
    }
    assert!(first && complete);
    assert_eq!(people, 99);
    assert_eq!(rooms, 63);
}
