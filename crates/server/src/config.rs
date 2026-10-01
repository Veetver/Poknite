use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub retention_seconds: i64,
    pub max_connections: usize,
    pub max_users: usize,
    pub max_devices_per_user: usize,
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub heartbeat_seconds: u64,
    pub pong_timeout_seconds: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8443".parse().unwrap(),
            data_dir: "./data".into(),
            certificate: "./cert.pem".into(),
            private_key: "./key.pem".into(),
            retention_seconds: 86400,
            max_connections: 60,
            max_users: 20,
            max_devices_per_user: 3,
            database_bytes: 32 * 1024 * 1024,
            wal_bytes: 4 * 1024 * 1024,
            heartbeat_seconds: 120,
            pong_timeout_seconds: 30,
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let result: Self = toml::from_str(
            &std::fs::read_to_string(path).context("Не удалось прочитать конфигурацию")?,
        )?;
        result.validate()?;
        Ok(result)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=2592000).contains(&self.retention_seconds),
            "retention_seconds: 1…2592000"
        );
        ensure!(
            (1..=300).contains(&self.max_connections),
            "max_connections: 1…300"
        );
        ensure!(
            (1..=100).contains(&self.max_users) && (1..=10).contains(&self.max_devices_per_user),
            "Недопустимые ограничения пользователей"
        );
        ensure!(
            self.database_bytes >= 131072 && self.database_bytes <= 1024 * 1024 * 1024,
            "Недопустимый размер базы"
        );
        ensure!(
            (65536..=64 * 1024 * 1024).contains(&self.wal_bytes)
                && (1..=3600).contains(&self.heartbeat_seconds)
                && (1..=300).contains(&self.pong_timeout_seconds),
            "Недопустимые сетевые ограничения"
        );
        Ok(())
    }
    pub fn database(&self) -> PathBuf {
        self.data_dir.join("server.db")
    }
    pub fn admin_socket(&self) -> PathBuf {
        self.data_dir.join("admin.sock")
    }
}
