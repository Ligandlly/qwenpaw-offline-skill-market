//! Runtime configuration. Every knob is an environment variable (PLAN: 限额只用 env，改需重启）。

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub db_path: PathBuf,
    pub public_url: String,
    pub admin_user: String,
    pub admin_password: String,
    pub limits: Limits,
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_bundle_bytes: usize,
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_path_depth: usize,
}

impl Limits {
    fn from_env() -> Self {
        Self {
            max_bundle_bytes: num("MARKET_MAX_BUNDLE_BYTES", 2 * 1024 * 1024),
            max_files: num("MARKET_MAX_FILES", 100),
            max_file_bytes: num("MARKET_MAX_FILE_BYTES", 256 * 1024),
            max_path_depth: num("MARKET_MAX_PATH_DEPTH", 3),
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            bind: std::env::var("MARKET_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            db_path: std::env::var("MARKET_DB")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("data/market.sqlite3")),
            public_url: std::env::var("MARKET_PUBLIC_URL")
                .unwrap_or_else(|_| "http://localhost:8080".into())
                .trim_end_matches('/')
                .to_string(),
            admin_user: std::env::var("ADMIN_USER").unwrap_or_else(|_| "admin".into()),
            admin_password: std::env::var("ADMIN_PASSWORD").unwrap_or_default(),
            limits: Limits::from_env(),
        }
    }
}

fn num(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
