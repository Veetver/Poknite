use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use poknite_server::{
    admin,
    config::Config,
    db,
    http::{self, App},
    tls,
};
use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help") {
        println!(
            "Poknite {}\npoknited --config FILE <serve|init|user NAME|channel NAME|grant USER CHANNEL|ungrant USER CHANNEL|invite USER|revoke DEVICE|status|cleanup>\nserve --dev-http: только loopback, для разработки",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let path = if let Some(index) = args.iter().position(|a| a == "--config") {
        ensure!(index + 1 < args.len(), "Нужен путь конфигурации");
        let path = PathBuf::from(args.remove(index + 1));
        args.remove(index);
        path
    } else {
        PathBuf::from("poknite.toml")
    };
    let config = Config::load(&path)?;
    if args[0] != "serve" {
        return admin::command(&config, &args);
    }
    let dev = args.iter().any(|a| a == "--dev-http");
    ensure!(
        !dev || config.listen.ip().is_loopback(),
        "HTTP разрешён только на loopback"
    );
    db::private_dir(&config.data_dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config.data_dir.join("server.lock"))?;
    lock.try_lock_exclusive().context("Сервер уже запущен")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let app = App::new(config.clone())?;
        let maintenance = app.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(60));
            loop {
                timer.tick().await;
                if let Ok(n) = maintenance.db.run(db::cleanup).await
                    && n > 0
                {
                    maintenance.wake();
                }
            }
        });
        let local = app.clone();
        tokio::spawn(async move {
            if http::listen_admin(local).await.is_err() {
                eprintln!("Сокет управления недоступен");
            }
        });
        let handle = axum_server::Handle::new();
        let shutdown = handle.clone();
        tokio::spawn(async move {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}};
            shutdown.graceful_shutdown(Some(Duration::from_secs(5)));
        });
        let service =
            http::router(app.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
        if dev {
            axum_server::bind(config.listen)
                .handle(handle)
                .serve(service)
                .await?;
        } else {
            let tls = tls::load(&config)
                .await
                .context("Не удалось загрузить сертификат HTTPS")?;
            let reload = tls.clone();
            let reload_app = app.clone();
            tokio::spawn(async move {
                let mut signal =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).unwrap();
                while signal.recv().await.is_some() {
                    match Config::load(&path) {
                        Ok(next) => {
                            if let Ok(new_tls) = tls::load(&next).await {
                                reload.reload_from_config(new_tls.get_inner());
                                reload_app
                                    .retention
                                    .store(next.retention_seconds, Ordering::Relaxed);
                                eprintln!("Сертификат и срок хранения обновлены");
                            } else {
                                eprintln!("Обновление сертификата отклонено; сохранён предыдущий");
                            }
                        }
                        Err(_) => eprintln!("Обновление конфигурации отклонено"),
                    }
                }
            });
            axum_server::bind_rustls(config.listen, tls)
                .handle(handle)
                .serve(service)
                .await?;
        }
        let _ = std::fs::remove_file(config.admin_socket());
        drop(lock);
        Ok::<_, anyhow::Error>(())
    })
}
