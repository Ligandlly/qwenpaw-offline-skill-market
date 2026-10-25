use offline_market_backend::{admin, build_state, config::Config, db, log, router};
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
    axum::serve(listener, router(app))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutting down");
}
