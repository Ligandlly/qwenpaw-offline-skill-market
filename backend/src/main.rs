use offline_market_backend::{admin, build_state, config::Config, counters, db, log, router};
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::from_env();
    log::init(&cfg.log_level);

    let conn = db::open(&cfg.db_path)?;
    let app = build_state(cfg.clone(), conn);

    if app.cfg.admin_password.is_empty() {
        if let Some(generated) = admin::ensure_admin(&app) {
            println!("==============================================================");
            println!("  first run: generated admin credentials");
            println!("  username: {}", app.cfg.admin_user);
            println!("  password: {generated}");
            println!("  (store it now; it is not shown again)");
            println!("==============================================================");
        }
    } else {
        admin::ensure_admin(&app);
    }

    // Download counts and `last_seen` beacons accumulate in memory and land
    // here; without this task they would never reach the database.
    let (stopping, stop) = tokio::sync::watch::channel(false);
    tokio::spawn(counters::flush_loop(app.clone(), stop));

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    info!(
        bind = %cfg.bind,
        db = %cfg.db_path.display(),
        public_url = %cfg.public_url,
        log_level = %cfg.log_level,
        "offline-market listening"
    );
    info!(
        max_bundle_bytes = cfg.limits.max_bundle_bytes,
        max_files = cfg.limits.max_files,
        max_file_bytes = cfg.limits.max_file_bytes,
        "publish limits"
    );
    axum::serve(listener, router(app.clone()))
        .with_graceful_shutdown(shutdown())
        .await?;

    // The last batch must not wait for the next tick.
    let _ = stopping.send(true);
    if let Err(err) = counters::flush(&app) {
        info!(code = err.code, "final counter flush failed");
    }
    Ok(())
}

/// Waits for the first stop request.
///
/// `docker stop`, `systemctl stop` and Kubernetes all send SIGTERM, not SIGINT;
/// listening only for Ctrl-C meant those stops killed the process outright and
/// the pending counter batch was lost.
async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = terminate.recv() => info!(signal = "SIGTERM", "shutting down"),
                    _ = tokio::signal::ctrl_c() => info!(signal = "SIGINT", "shutting down"),
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "SIGTERM handler unavailable, waiting for Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                info!(signal = "SIGINT", "shutting down");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!(signal = "SIGINT", "shutting down");
    }
}
