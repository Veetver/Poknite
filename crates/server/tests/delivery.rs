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
            .post(format!("{}/v1/channels/{channel}/messages", self.base))
            .bearer_auth(token)
            .json(&PublishRequest {
                client_message_id: id.into(),
                text: text.into(),
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
            "{}/v1/stream?after={cursor}",
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
            .post(format!("{}/v1/devices/enroll", self.base))
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
                Frame::Text(text) => return serde_json::from_str(&text).unwrap(),
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
            ServerEvent::Message { message, replay } => {
                assert!(replay);
                messages.push(message);
            }
            ServerEvent::Progress { .. } | ServerEvent::Reset { .. } => {}
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
            matches!(event(socket).await,ServerEvent::Message {message,replay:false} if message.id==second.id)
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
        .delete(format!("{}/v1/devices/2", server.base))
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
    for path in ["/v1/channels", "/v1/devices"] {
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
        .delete(format!("{}/v1/devices/3", server.base))
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
            .delete(format!("{}/v1/devices/2", server.base))
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
            .get(format!("{}/v1/channels", server.base))
            .bearer_auth(B)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut request = format!("{}/v1/stream", server.base.replace("http://", "ws://"))
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
        matches!(event(&mut socket).await,ServerEvent::Channels {channels} if channels.len()==2)
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
        matches!(event(&mut socket).await,ServerEvent::Channels {channels} if channels.len()==1)
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
    let mut request = format!("{}/v1/stream", server.base.replace("http://", "ws://"))
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
