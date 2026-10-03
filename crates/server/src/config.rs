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
    pub management: Management,
    pub data_dir: PathBuf,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub retention_seconds: i64,
    pub e2ee_required: bool,
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
            management: Management::default(),
            data_dir: "./data".into(),
            certificate: "./cert.pem".into(),
            private_key: "./key.pem".into(),
            retention_seconds: 86400,
            e2ee_required: true,
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
        self.management.validate()?;
        ensure!(
            !self.management.enabled || self.management.listen != self.listen,
            "Адреса переписки и управления должны различаться"
        );
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

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Management {
    pub enabled: bool,
    pub listen: SocketAddr,
    pub allowed_subnets: Vec<String>,
}
impl Default for Management {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "127.0.0.1:8444".parse().unwrap(),
            allowed_subnets: vec!["127.0.0.0/8".into(), "::1/128".into()],
        }
    }
}
impl Management {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.listen.ip().is_unspecified() && self.listen.port() != 0,
            "Управление требует конкретного адреса прослушивания и порта"
        );
        ensure!(
            !self.allowed_subnets.is_empty() && self.allowed_subnets.len() <= 64,
            "Укажите 1…64 доверенные подсети"
        );
        for subnet in &self.allowed_subnets {
            let _ = Cidr::parse(subnet)?;
        }
        ensure!(
            !self.enabled || self.trusted(self.listen.ip()),
            "Адрес управления должен входить в доверенную подсеть"
        );
        Ok(())
    }
    pub fn trusted(&self, ip: std::net::IpAddr) -> bool {
        self.allowed_subnets
            .iter()
            .any(|s| Cidr::parse(s).is_ok_and(|c| c.contains(ip)))
    }
}
#[derive(Clone, Debug)]
struct Cidr {
    ip: std::net::IpAddr,
    prefix: u32,
}
impl Cidr {
    fn parse(s: &str) -> Result<Self> {
        let (ip, prefix) = s
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("Подсеть должна иметь формат IPv4/IPv6 CIDR"))?;
        let ip = ip.parse::<std::net::IpAddr>()?;
        let prefix = prefix.parse::<u32>()?;
        ensure!(
            prefix > 0 && prefix <= if ip.is_ipv4() { 32 } else { 128 },
            "Некорректная длина CIDR"
        );
        Ok(Self { ip, prefix })
    }
    fn contains(&self, ip: std::net::IpAddr) -> bool {
        let ip = match ip {
            std::net::IpAddr::V6(v) => v.to_ipv4_mapped().map(std::net::IpAddr::V4).unwrap_or(ip),
            _ => ip,
        };
        match (self.ip, ip) {
            (std::net::IpAddr::V4(a), std::net::IpAddr::V4(b)) => {
                u32::from(a) >> (32 - self.prefix) == u32::from(b) >> (32 - self.prefix)
            }
            (std::net::IpAddr::V6(a), std::net::IpAddr::V6(b)) => {
                u128::from(a) >> (128 - self.prefix) == u128::from(b) >> (128 - self.prefix)
            }
            _ => false,
        }
    }
}
#[cfg(test)]
mod management_tests {
    use super::*;
    #[test]
    fn trusted_networks_are_explicit_and_dual_stack() {
        let m = Management::default();
        assert!(!m.enabled);
        assert!(m.trusted("127.0.0.1".parse().unwrap()));
        assert!(m.trusted("::1".parse().unwrap()));
        assert!(!m.trusted("192.168.1.1".parse().unwrap()));
        let m = Management {
            enabled: true,
            listen: "192.168.10.2:8444".parse().unwrap(),
            allowed_subnets: vec!["192.168.10.0/24".into(), "fd00:abcd::/64".into()],
        };
        assert!(m.validate().is_ok());
        assert!(m.trusted("192.168.10.254".parse().unwrap()));
        assert!(m.trusted("fd00:abcd::2".parse().unwrap()));
        assert!(!m.trusted("fd00:abce::2".parse().unwrap()));
        assert!(!m.trusted("::ffff:192.168.11.1".parse().unwrap()));
        assert!(
            Management {
                listen: "0.0.0.0:8444".parse().unwrap(),
                ..m
            }
            .validate()
            .is_err()
        );
        assert!(Cidr::parse("192.168.1.0/33").is_err());
        assert!(Cidr::parse("0.0.0.0/0").is_err());
    }
}
