use crate::{Profile, Store};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use poknite_protocol::*;
use reqwest::{Client, Url};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{
    Connector,
    tungstenite::{
        self,
        client::IntoClientRequest,
        protocol::{Message as Frame, WebSocketConfig},
    },
};

#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Offline,
    Revoked,
}
#[derive(Clone, Debug)]
pub enum Update {
    Status(ConnectionStatus),
    Channels(Vec<Channel>),
    Changed,
    Notifications,
    Gap,
}
#[derive(Clone)]
pub struct Api {
    http: Client,
    base: Url,
    tls: Arc<rustls::ClientConfig>,
}
impl Api {
    pub fn new(server: &str, dev_http: bool, extra_ca: Option<&Path>) -> Result<Self> {
        let mut base = Url::parse(server).context("Проверьте адрес сервера")?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            bail!("Укажите адрес сервера без пути, пароля и параметров")
        }
        let loopback = base.host_str().is_some_and(|s| {
            s.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        });
        if base.scheme() != "https" && !(dev_http && base.scheme() == "http" && loopback) {
            bail!("Нужен HTTPS. --dev-http разрешён только для числового loopback-адреса")
        }
        base.set_path("/");
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_native_certs::load_native_certs().certs {
            let _ = roots.add(cert);
        }
        let mut http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(1)
            .user_agent(concat!("Poknite/", env!("CARGO_PKG_VERSION")));
        if let Some(path) = extra_ca {
            let pem = std::fs::read(path)?;
            http = http.add_root_certificate(reqwest::Certificate::from_pem(&pem)?);
            for cert in rustls_pemfile::certs(&mut std::io::Cursor::new(pem)) {
                roots.add(cert?)?;
            }
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            http: http.build()?,
            base,
            tls: Arc::new(tls),
        })
    }
    fn endpoint(&self, path: &str) -> Result<Url> {
        Ok(self.base.join(path)?)
    }
    async fn decode<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
        let status = response.status();
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > MAX_FRAME_BYTES {
                bail!("Слишком большой ответ сервера")
            };
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            if status.as_u16() == 401 {
                bail!("unauthorized")
            };
            let error = serde_json::from_slice::<ApiError>(&bytes).ok();
            bail!(
                "{}",
                error
                    .map(|e| e.message)
                    .unwrap_or_else(|| format!("Ошибка сервера: {}", status.as_u16()))
            )
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub async fn enroll(&self, invitation: &str, name: &str) -> Result<EnrollResponse> {
        if !valid_name(name) || invitation.len() != 64 {
            bail!("Проверьте приглашение и имя устройства")
        };
        Self::decode(
            self.http
                .post(self.endpoint("v1/devices/enroll")?)
                .json(&EnrollRequest {
                    invitation: invitation.into(),
                    device_name: name.into(),
                })
                .send()
                .await?,
        )
        .await
    }
    pub async fn channels(&self, profile: &Profile) -> Result<Vec<Channel>> {
        Self::decode(
            self.http
                .get(self.endpoint("v1/channels")?)
                .bearer_auth(&profile.token)
                .send()
                .await?,
        )
        .await
    }
    pub async fn devices(&self, profile: &Profile) -> Result<Vec<Device>> {
        Self::decode(
            self.http
                .get(self.endpoint("v1/devices")?)
                .bearer_auth(&profile.token)
                .send()
                .await?,
        )
        .await
    }
    pub async fn revoke(&self, profile: &Profile, id: i64) -> Result<()> {
        let r = self
            .http
            .delete(self.endpoint(&format!("v1/devices/{id}"))?)
            .bearer_auth(&profile.token)
            .send()
            .await?;
        if r.status().is_success() {
            Ok(())
        } else {
            let _: serde_json::Value = Self::decode(r).await?;
            bail!("Не удалось отключить устройство")
        }
    }
    pub async fn publish(
        &self,
        profile: &Profile,
        channel: i64,
        request: &PublishRequest,
    ) -> Result<Message> {
        if !valid_text(&request.text) {
            bail!("Нужен текст до 4096 байт")
        };
        Self::decode(
            self.http
                .post(self.endpoint(&format!("v1/channels/{channel}/messages"))?)
                .bearer_auth(&profile.token)
                .json(request)
                .send()
                .await?,
        )
        .await
    }
    async fn session(
        &self,
        profile: &Profile,
        store: &Store,
        updates: &mpsc::Sender<Update>,
        restart: &mut watch::Receiver<u64>,
    ) -> Result<()> {
        let cursor = store.cursor()?;
        let mut url = self.endpoint("v1/stream")?;
        url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
            .map_err(|_| anyhow::anyhow!("Недопустимый адрес"))?;
        url.query_pairs_mut()
            .append_pair("after", &cursor.to_string());
        let mut request = url.as_str().into_client_request()?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", profile.token).parse()?,
        );
        let config = WebSocketConfig::default()
            .read_buffer_size(4096)
            .write_buffer_size(0)
            .max_write_buffer_size(65536)
            .max_message_size(Some(MAX_FRAME_BYTES))
            .max_frame_size(Some(MAX_FRAME_BYTES));
        let connecting = tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(config),
            false,
            Some(Connector::Rustls(self.tls.clone())),
        );
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), connecting)
            .await?
            .map_err(|e| match &e {
                tungstenite::Error::Http(r) if r.status().as_u16() == 401 => {
                    anyhow::anyhow!("unauthorized")
                }
                _ => anyhow::anyhow!("Соединение недоступно"),
            })?;
        let mut last_seen = tokio::time::Instant::now();
        let mut heartbeat = tokio::time::interval(Duration::from_secs(120));
        heartbeat.tick().await;
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _=restart.changed()=>{let _=socket.close(None).await;return Ok(())},
                _=heartbeat.tick()=> {if last_seen.elapsed()>Duration::from_secs(180){bail!("Таймаут соединения")};},
                incoming=socket.next()=> {
                    last_seen=tokio::time::Instant::now();let Some(incoming)=incoming else {bail!("Сервер отключился")};
                    match incoming? {
                        Frame::Text(text)=> {
                            match serde_json::from_str::<ServerEvent>(&text)? {
                                ServerEvent::Hello{user_id,device_id,channels}=>{if user_id!=profile.user_id || device_id!=profile.device_id {bail!("Устройство сервера не совпадает с профилем")};store.set_channels(&channels)?;updates.send(Update::Channels(channels)).await?;},
                                ServerEvent::Channels{channels}=>{store.set_channels(&channels)?;updates.send(Update::Channels(channels)).await?;},
                                ServerEvent::Message{message,replay}=>{if store.receive(&message,replay,profile.user_id)? {updates.send(Update::Changed).await?;}
                                    if !replay {updates.send(Update::Notifications).await?;}},
                                ServerEvent::Progress{cursor}=>{store.set_cursor(cursor)?;tokio::time::timeout(Duration::from_secs(5),socket.send(Frame::Text(serde_json::to_string(&ClientEvent::Ack{cursor})?.into()))).await??;},
                                ServerEvent::Synced{cursor,gap}=>{store.set_cursor(cursor)?;socket.send(Frame::Text(serde_json::to_string(&ClientEvent::Ack{cursor})?.into())).await?;updates.send(Update::Status(ConnectionStatus::Connected)).await?;if gap {updates.send(Update::Gap).await?;}updates.send(Update::Notifications).await?;},
                                ServerEvent::Reset{cursor}=>store.set_cursor(cursor)?,
                            }
                        }
                        Frame::Ping(payload)=>{socket.send(Frame::Pong(payload)).await?;},
                        Frame::Pong(_)=>{},
                        Frame::Close(frame)=>{if frame.is_some_and(|f|u16::from(f.code)==4001){bail!("unauthorized")};bail!("Сервер закрыл соединение")},
                        _=>bail!("Недопустимый ответ сервера"),
                    }
                }
            }
        }
    }
    pub async fn run(
        &self,
        profile: Profile,
        store: Arc<Store>,
        updates: mpsc::Sender<Update>,
        mut restart: watch::Receiver<u64>,
    ) {
        let mut delay = 1u64;
        loop {
            if updates
                .send(Update::Status(ConnectionStatus::Connecting))
                .await
                .is_err()
            {
                return;
            }
            let started = tokio::time::Instant::now();
            let result = self.session(&profile, &store, &updates, &mut restart).await;
            if let Err(error) = &result
                && error.to_string() == "unauthorized"
            {
                let _ = updates
                    .send(Update::Status(ConnectionStatus::Revoked))
                    .await;
                return;
            }
            if updates.is_closed() {
                return;
            }
            let _ = updates
                .send(Update::Status(ConnectionStatus::Offline))
                .await;
            if started.elapsed() > Duration::from_secs(30) {
                delay = 1;
            }
            let jitter = uuid::Uuid::new_v4().as_bytes()[0] as u64 % 1000;
            tokio::select! {_ =tokio::time::sleep(Duration::from_millis(delay*1000+jitter))=>{},_=restart.changed()=>{delay=1;}}
            delay = (delay * 2).min(60);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secure_server_policy() {
        assert!(Api::new("https://example.com", false, None).is_ok());
        assert!(Api::new("http://example.com", true, None).is_err());
        assert!(Api::new("http://127.0.0.1:8090", true, None).is_ok());
        assert!(Api::new("http://127.0.0.1", false, None).is_err());
        assert!(Api::new("https://user:secret@example.com", false, None).is_err());
        assert!(Api::new("https://example.com/path", false, None).is_err());
    }
    // The handshake Callback trait fixes ErrorResponse as a large HTTP response.
    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn committed_cursor_ack_reconnect_and_revocation() {
        use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
        let directory =
            std::env::temp_dir().join(format!("poknite-stream-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(Store::open(&directory).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let check = store.clone();
        let server = tokio::spawn(async move {
            for pass in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    move |request: &tungstenite::handshake::server::Request,
                          response: tungstenite::handshake::server::Response| {
                        assert_eq!(
                            request.uri().query(),
                            Some(if pass == 0 { "after=0" } else { "after=1" })
                        );
                        assert!(request.headers().contains_key("authorization"));
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                let hello = ServerEvent::Hello {
                    user_id: 1,
                    device_id: 1,
                    channels: vec![Channel {
                        id: 1,
                        name: "Общий".into(),
                    }],
                };
                socket
                    .send(Frame::Text(serde_json::to_string(&hello).unwrap().into()))
                    .await
                    .unwrap();
                if pass == 0 {
                    let m = Message {
                        id: "first".into(),
                        seq: 1,
                        channel_id: 1,
                        sender_id: 2,
                        sender_name: "Друг".into(),
                        text: "Текст".into(),
                        created_at: 1,
                        expires_at: 2,
                    };
                    socket
                        .send(Frame::Text(
                            serde_json::to_string(&ServerEvent::Message {
                                message: m,
                                replay: true,
                            })
                            .unwrap()
                            .into(),
                        ))
                        .await
                        .unwrap();
                    socket
                        .send(Frame::Text(
                            serde_json::to_string(&ServerEvent::Progress { cursor: 1 })
                                .unwrap()
                                .into(),
                        ))
                        .await
                        .unwrap();
                }
                socket
                    .send(Frame::Text(
                        serde_json::to_string(&ServerEvent::Synced {
                            cursor: 1,
                            gap: false,
                        })
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
                let ack = socket.next().await.unwrap().unwrap();
                let Frame::Text(ack) = ack else {
                    panic!("expected ack")
                };
                assert!(matches!(
                    serde_json::from_str::<ClientEvent>(&ack).unwrap(),
                    ClientEvent::Ack { cursor: 1 }
                ));
                assert_eq!(check.cursor().unwrap(), 1);
                assert_eq!(check.history(1, 0).unwrap().len(), 1);
                if pass == 0 {
                    let _ = socket.close(None).await;
                } else {
                    socket
                        .close(Some(CloseFrame {
                            code: CloseCode::Library(4001),
                            reason: "revoked".into(),
                        }))
                        .await
                        .unwrap();
                }
            }
        });
        let api = Api::new(&format!("http://{address}"), true, None).unwrap();
        let profile = Profile {
            server: format!("http://{address}"),
            token: "a".repeat(64),
            device_id: 1,
            user_id: 1,
            user_name: "Я".into(),
            autostart: false,
        };
        let (tx, mut rx) = mpsc::channel(64);
        let (restart, watch) = watch::channel(0);
        let cloned = store.clone();
        let job = tokio::spawn(async move { api.run(profile, cloned, tx, watch).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(update) = rx.recv().await {
                match update {
                    Update::Status(ConnectionStatus::Offline) => {
                        restart.send_modify(|n| *n += 1);
                    }
                    Update::Status(ConnectionStatus::Revoked) => break,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        job.await.unwrap();
        server.await.unwrap();
        assert_eq!(store.pending().unwrap().len(), 1);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
