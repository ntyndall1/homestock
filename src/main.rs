mod activity;
mod auth;
mod backup;
mod cli;
mod config;
mod db;
mod error;
mod photos;
mod repo;
mod routes;
mod search;
mod security;

use cli::Command;
use std::{net::SocketAddr, process::ExitCode, sync::Arc, time::Duration};

/// The release this binary was built for, shown in the page footer. Release builds set
/// HOMESTOCK_VERSION from the release tag (see the Dockerfile and release workflow); any other
/// build is marked as a development build.
pub const APP_VERSION: &str = match option_env!("HOMESTOCK_VERSION") {
    Some(version) => version,
    None => "0.0.0-dev",
};

#[derive(Clone)]
pub struct AppState {
    pub db: db::Db,
    pub cfg: Arc<config::Config>,
    pub activity: Arc<activity::Activity>,
    pub passwords: Arc<security::PasswordLimiter>,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let invocation = match cli::parse(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("homestock: {error}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let result = match invocation.command {
        Command::Help => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        Command::SchemaVersion => {
            println!("{}", db::CURRENT_SCHEMA_VERSION);
            Ok(())
        }
        Command::Serve => config::Config::load(&invocation.config).and_then(serve),
        command => config::Config::load(&invocation.config).and_then(|cfg| {
            println!("{}", cli::run(&cfg, command)?);
            Ok(())
        }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("homestock: {error}");
            ExitCode::FAILURE
        }
    }
}

// current_thread: one OS thread for async work; blocking work uses tokio's lazy blocking pool.
fn serve(cfg: config::Config) -> anyhow_free::Result {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run_server(cfg))
}

async fn run_server(cfg: config::Config) -> anyhow_free::Result {
    let cfg = Arc::new(cfg);
    std::fs::create_dir_all(cfg.data_dir.join("photos"))?;
    let db_path = cfg.database_path();
    if let Some(parent) = db_path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let db = db::Db::open(&db_path)?;

    let activity = Arc::new(activity::Activity::default());
    let state = AppState {
        db,
        cfg: cfg.clone(),
        activity: activity.clone(),
        passwords: Default::default(),
    };

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let backupper =
        backup::Backupper::new(cfg.backup.clone(), db_path.clone(), cfg.data_dir.clone())?;
    let backup_task = tokio::spawn(backup::run_loop(backupper, activity.clone(), stop_rx));

    let app = routes::router(state);
    let addr: SocketAddr = cfg.bind.parse()?;
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        if let Err(error) = shutdown_signal().await {
            tracing::error!("failed to install shutdown signal handler: {error}");
        }
        shutdown_handle.graceful_shutdown(Some(Duration::from_secs(30)));
    });

    match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(cert), Some(key)) => {
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
            tracing::info!("listening on https://{addr}");
            axum_server::tls_rustls::bind_rustls(addr, tls)
                .handle(handle)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
        }
        (None, None) => {
            tracing::info!("listening on http://{addr}");
            axum_server::bind(addr)
                .handle(handle)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
        }
        _ => return Err("tls_cert and tls_key must be set together".into()),
    }

    let _ = stop_tx.send(true); // final backup if dirty
    let _ = backup_task.await;
    Ok(())
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

/// Tiny boxed-error alias for `main`, avoiding an `anyhow` dependency.
mod anyhow_free {
    pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
}
