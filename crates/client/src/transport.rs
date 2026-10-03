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
    Profile(User),
    Contacts(Vec<User>),
    Diagnostic(String),
}
#[derive(Clone)]
pub struct Api {
    http: Client,
    base: Url,
    tls: Arc<rustls::ClientConfig>,
    trust_notes: Vec<String>,
    management: bool,
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
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let native = rustls_native_certs::load_native_certs();
        let mut trust_notes = native
            .errors
            .iter()
            .map(|e| format!("Системные корни: {e}; используются встроенные доверенные корни"))
            .collect::<Vec<_>>();
        for cert in native.certs {
            if let Err(e) = roots.add(cert) {
                trust_notes.push(format!("Системный сертификат отклонён: {e}"));
            }
        }
        if let Some(path) = extra_ca {
            let pem = std::fs::read(path).context("Не удалось прочитать доверенный сертификат")?;
            let certs = rustls_pemfile::certs(&mut std::io::Cursor::new(pem))
                .collect::<std::io::Result<Vec<_>>>()?;
            if certs.is_empty() {
                bail!("Файл доверенных сертификатов пуст")
            }
            for cert in certs {
                roots.add(cert)?;
            }
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            http: Self::http_builder(&tls).build()?,
            base,
            tls: Arc::new(tls),
            trust_notes,
            management: false,
        })
    }
    fn http_builder(tls: &rustls::ClientConfig) -> reqwest::ClientBuilder {
        Client::builder()
            .use_preconfigured_tls(tls.clone())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(1)
            .user_agent(concat!("Poknite/", env!("CARGO_PKG_VERSION")))
    }
    pub fn management(&self, server: &str, connect_ip: Option<std::net::IpAddr>) -> Result<Self> {
        let mut api = Self::new(server, false, None)?;
        api.tls = self.tls.clone();
        let mut builder = Self::http_builder(&self.tls).no_proxy();
        if let Some(ip) = connect_ip {
            let host = api
                .base
                .host_str()
                .context("Нужно доменное имя управления")?;
            builder = builder.resolve(
                host,
                std::net::SocketAddr::new(ip, api.base.port_or_known_default().unwrap_or(443)),
            );
        }
        api.http = builder.build()?;
        api.management = true;
        Ok(api)
    }
    pub async fn diagnostics(&self) -> Vec<String> {
        let mut report = self.trust_notes.clone();
        let host = self.base.host_str().unwrap_or("").trim_matches(['[', ']']);
        let port = self.base.port_or_known_default().unwrap_or(443);
        match tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host, port)),
        )
        .await
        {
            Ok(Ok(addresses)) => report.push(format!(
                "DNS: {}",
                addresses
                    .map(|a| a.ip().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Ok(Err(e)) => report.push(format!("DNS: не удалось определить адрес: {e}")),
            Err(_) => report.push("DNS: превышено время ожидания".into()),
        }
        match self
            .http
            .get(self.endpoint("healthz").unwrap())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                report.push("HTTPS: соединение и сертификат проверены".into())
            }
            Ok(r) => report.push(format!("HTTPS: сервер ответил {}", r.status())),
            Err(e) => report.push(format!("HTTPS: {}", network_reason(&e))),
        }
        let mut url = self.endpoint("v2/stream").unwrap();
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        let _ = url.set_scheme(scheme);
        let mut legacy = false;
        loop {
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                tokio_tungstenite::connect_async_tls_with_config(
                    url.as_str(),
                    None,
                    false,
                    Some(Connector::Rustls(self.tls.clone())),
                ),
            )
            .await;
            if !legacy
                && matches!(&result,Ok(Err(tungstenite::Error::Http(r))) if r.status().as_u16()==404)
            {
                legacy = true;
                url.set_path("/v1/stream");
                continue;
            }
            if legacy {
                report.push(
                    "Сервер использует API v1: требуется совместное обновление сервера и клиентов"
                        .into(),
                );
            }
            match result {
                Ok(Err(tungstenite::Error::Http(r))) if r.status().as_u16() == 401 => report.push(
                    "WebSocket: HTTPS/WSS доступны; для потока нужен токен устройства".into(),
                ),
                Ok(Ok((mut socket, _))) => {
                    let _ = socket.close(None).await;
                    report.push("WebSocket: соединение установлено".into());
                }
                Ok(Err(e)) => report.push(format!("WebSocket: {}", network_reason(&e))),
                Err(_) => report.push("WebSocket: превышено время ожидания".into()),
            }
            break;
        }
        report
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
                .post(self.endpoint("v2/devices/enroll")?)
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
        self.request(profile, "v2/conversations", "GET", None).await
    }
    pub async fn devices(&self, profile: &Profile) -> Result<Vec<Device>> {
        Self::decode(
            self.http
                .get(self.endpoint("v2/devices")?)
                .bearer_auth(&profile.token)
                .send()
                .await?,
        )
        .await
    }
    pub async fn revoke(&self, profile: &Profile, id: i64) -> Result<()> {
        let r = self
            .http
            .delete(self.endpoint(&format!("v2/devices/{id}"))?)
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
        store: &Store,
        channel: i64,
        request: &PublishRequest,
    ) -> Result<Message> {
        if !valid_text(&request.text) {
            bail!("Нужен текст до 4096 байт")
        };
        let members = self.e2ee_members(profile, channel).await?;
        let request = store.seal_publish(
            self.audience(),
            profile.user_id,
            profile.device_id,
            channel,
            &members,
            request,
        )?;
        let message: Message = Self::decode(
            self.http
                .post(self.endpoint(&format!("v2/conversations/{channel}/messages"))?)
                .bearer_auth(&profile.token)
                .json(&request)
                .send()
                .await?,
        )
        .await?;
        anyhow::ensure!(
            message.text == request.text
                && message.channel_id == channel
                && message.sender_id == profile.user_id,
            "Сервер изменил отправленное сообщение"
        );
        store.unseal_message(self.audience(), &message)
    }
    pub fn audience(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }
    pub async fn e2ee_members(
        &self,
        profile: &Profile,
        channel: i64,
    ) -> Result<Vec<poknite_protocol::e2ee::Member>> {
        let members: Vec<poknite_protocol::e2ee::Member> = self
            .request(
                profile,
                &format!("v2/conversations/{channel}/e2ee-members"),
                "GET",
                None,
            )
            .await?;
        anyhow::ensure!(
            poknite_protocol::e2ee::members_digest(&members).is_some(),
            "Некорректный состав устройств"
        );
        Ok(members)
    }
    pub async fn request<T: serde::de::DeserializeOwned>(
        &self,
        profile: &Profile,
        path: &str,
        method: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T> {
        if path.starts_with("v2/admin") && !self.management {
            bail!("Управление требует отдельного HTTPS-адреса");
        }
        if method == "GET" && catalog_path(path) {
            let mut rows = Vec::<serde_json::Value>::new();
            loop {
                let response = self
                    .http
                    .get(self.endpoint(&format!("{path}?offset={}", rows.len()))?)
                    .bearer_auth(&profile.token)
                    .send()
                    .await
                    .map_err(|e| {
                        if self.management {
                            anyhow::anyhow!(
                                "Управление недоступно из текущей сети: {}",
                                network_reason(&e)
                            )
                        } else {
                            anyhow::anyhow!(network_reason(&e))
                        }
                    })?;
                let page: Vec<serde_json::Value> = Self::decode(response).await?;
                let complete = page.len() < CATALOG_PAGE_SIZE;
                rows.extend(page);
                anyhow::ensure!(rows.len() <= 1000, "Слишком большой каталог сервера");
                if complete {
                    break;
                }
            }
            return Ok(serde_json::from_value(serde_json::Value::Array(rows))?);
        }
        let mut request = self
            .http
            .request(method.parse()?, self.endpoint(path)?)
            .bearer_auth(&profile.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|e| {
            if self.management {
                anyhow::anyhow!(
                    "Управление недоступно из текущей сети: {}",
                    network_reason(&e)
                )
            } else {
                anyhow::anyhow!(network_reason(&e))
            }
        })?;
        Self::decode(response).await
    }
    pub async fn current_profile(&self, profile: &Profile) -> Result<User> {
        self.request(profile, "v2/profile", "GET", None).await
    }
    pub async fn contacts(&self, profile: &Profile) -> Result<Vec<User>> {
        self.request(profile, "v2/contacts", "GET", None).await
    }
    pub async fn participants(&self, profile: &Profile, id: i64) -> Result<Vec<User>> {
        self.request(
            profile,
            &format!("v2/conversations/{id}/participants"),
            "GET",
            None,
        )
        .await
    }
    pub async fn rename(&self, profile: &Profile, name: String) -> Result<User> {
        self.request(
            profile,
            "v2/profile",
            "PUT",
            Some(serde_json::to_value(ProfileRequest { name, color: None })?),
        )
        .await
    }
    pub async fn profile_color(
        &self,
        profile: &Profile,
        name: String,
        color: String,
    ) -> Result<User> {
        self.request(
            profile,
            "v2/profile",
            "PUT",
            Some(serde_json::to_value(ProfileRequest {
                name,
                color: Some(color),
            })?),
        )
        .await
    }
    pub async fn direct(&self, profile: &Profile, user_id: i64) -> Result<Conversation> {
        self.request(
            profile,
            "v2/conversations/direct",
            "POST",
            Some(serde_json::to_value(DirectRequest { user_id })?),
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
        let mut url = self.endpoint("v2/stream")?;
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
                _ => anyhow::anyhow!("WebSocket: {}", network_reason(&e)),
            })?;
        let mut parts: Option<(Option<User>, Vec<User>, Vec<Channel>)> = None;
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
                                ServerEvent::Hello{user_id,device_id,channels}=>{if user_id!=profile.user_id || device_id!=profile.device_id {bail!("Устройство сервера не совпадает с профилем")};if !channels.is_empty() {store.set_channels(&channels)?;updates.send(Update::Channels(channels)).await?;}},
                                ServerEvent::Channels{channels}=>{store.set_channels(&channels)?;updates.send(Update::Channels(channels)).await?;},
                                ServerEvent::Message{message,replay,notify}=>{if store.receive_secure(self.audience(),&message,replay,profile.user_id,notify)? {updates.send(Update::Changed).await?;}
                                    if !replay {updates.send(Update::Notifications).await?;}},
                                ServerEvent::Progress{cursor}=>{store.set_cursor(cursor)?;tokio::time::timeout(Duration::from_secs(5),socket.send(Frame::Text(serde_json::to_string(&ClientEvent::Ack{cursor})?.into()))).await??;},
                                ServerEvent::Synced{cursor,gap}=>{store.set_cursor(cursor)?;socket.send(Frame::Text(serde_json::to_string(&ClientEvent::Ack{cursor})?.into())).await?;updates.send(Update::Status(ConnectionStatus::Connected)).await?;if gap {updates.send(Update::Gap).await?;}updates.send(Update::Notifications).await?;},
                                ServerEvent::Reset{cursor}=>store.set_cursor(cursor)?,
                                ServerEvent::Profiles{users}=>{store.update_users(&users)?;updates.send(Update::Changed).await?;},
                                ServerEvent::StatePart{profile:current,contacts,channels,first,last}=>{
                                    if first {parts=Some((current,Vec::new(),Vec::new()));}
                                    let Some((_own,people,rooms))=parts.as_mut() else {bail!("Ошибка последовательности состояния")};
                                    people.extend(contacts);rooms.extend(channels);
                                    if last {let (own,people,rooms)=parts.take().unwrap();let own=own.ok_or_else(||anyhow::anyhow!("Профиль отсутствует"))?;store.set_channels(&rooms)?;store.update_users(&people)?;store.update_users(std::slice::from_ref(&own))?;updates.send(Update::Profile(own)).await?;updates.send(Update::Contacts(people)).await?;updates.send(Update::Channels(rooms)).await?;}
                                },
                                ServerEvent::State{profile:current,contacts,channels}=>{store.set_channels(&channels)?;store.update_users(&contacts)?;store.update_users(std::slice::from_ref(&current))?;updates.send(Update::Profile(current)).await?;updates.send(Update::Contacts(contacts)).await?;updates.send(Update::Channels(channels)).await?;},
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
            if let Err(error) = &result {
                let _ = updates.send(Update::Diagnostic(error.to_string())).await;
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
pub fn network_reason(error: &(dyn std::error::Error + 'static)) -> String {
    let mut chain = Vec::new();
    let mut current = Some(error);
    while let Some(e) = current {
        chain.push(e.to_string());
        current = e.source();
    }
    let details = chain.join(": ");
    let lower = details.to_lowercase();
    let reason = if lower.contains("certificate") || lower.contains("cert") || lower.contains("tls")
    {
        "Ошибка проверки TLS-сертификата или имени сервера"
    } else if lower.contains("dns") || lower.contains("resolve") {
        "Не удалось определить адрес сервера"
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "Превышено время ожидания соединения"
    } else {
        "Сетевая ошибка"
    };
    format!("{reason}: {details}")
}

fn catalog_path(path: &str) -> bool {
    matches!(
        path,
        "v2/contacts"
            | "v2/channels"
            | "v2/conversations"
            | "v2/admin/users"
            | "v2/admin/channels"
            | "v2/admin/roles"
    ) || path.starts_with("v2/conversations/")
        && (path.ends_with("/participants") || path.ends_with("/e2ee-members"))
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
        let audience = format!("http://{address}");
        let members = vec![
            poknite_protocol::e2ee::Member {
                user_id: 1,
                device_id: 1,
            },
            poknite_protocol::e2ee::Member {
                user_id: 2,
                device_id: 2,
            },
        ];
        store
            .create_conversation_key(&audience, 1, &members)
            .unwrap();
        let encrypted = store
            .seal_publish(
                &audience,
                2,
                2,
                1,
                &members,
                &PublishRequest {
                    client_message_id: uuid::Uuid::new_v4().to_string(),
                    text: "Текст".into(),
                    mentions: vec![],
                },
            )
            .unwrap()
            .text;
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
                        kind: Default::default(),
                        closed: false,
                        actions: vec![Permission::Read],
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
                        text: encrypted.clone(),
                        created_at: 1,
                        expires_at: 2,
                        mentions: Vec::new(),
                        sender_color: poknite_protocol::default_color(),
                    };
                    socket
                        .send(Frame::Text(
                            serde_json::to_string(&ServerEvent::Message {
                                message: m,
                                replay: true,
                                notify: true,
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
            management_server: String::new(),
            management_ip: None,
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
