use crate::config::Config;
use anyhow::{Context, Result, ensure};
use axum_server::tls_rustls::RustlsConfig;
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
};

fn bounded_read(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Файл сертификата или ключа слишком велик"
    );
    Ok(bytes)
}

pub async fn load(config: &Config) -> Result<RustlsConfig> {
    // Validate the same bytes that are installed, avoiding a file replacement race.
    let certificate = bounded_read(&config.certificate)?;
    let key = bounded_read(&config.private_key)?;
    let certificates = rustls_pemfile::certs(&mut Cursor::new(&certificate))
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(!certificates.is_empty(), "Цепочка сертификатов пуста");
    for cert in certificates {
        let (remaining, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
            .map_err(|_| anyhow::anyhow!("Недопустимый сертификат X.509"))?;
        ensure!(
            remaining.is_empty() && parsed.validity().is_valid(),
            "Сертификат истёк или ещё не действует"
        );
    }
    RustlsConfig::from_pem(certificate, key)
        .await
        .context("Сертификат и ключ несовместимы")
}
