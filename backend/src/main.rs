use offline_market_backend::{admin, build_state, config::Config, db, router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::from_env();
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
    println!("offline-market listening on http://{}", cfg.bind);
    println!("  db: {}", cfg.db_path.display());
    println!(
        "  limits: bundle {} B / files {} / file {} B / depth {}",
        cfg.limits.max_bundle_bytes,
        cfg.limits.max_files,
        cfg.limits.max_file_bytes,
        cfg.limits.max_path_depth
    );
    axum::serve(listener, router(app))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    println!("\nshutting down");
}
