//! Runtime configuration. Every knob is an environment variable (PLAN: 限额只用 env，改需重启）。

use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub db_path: PathBuf,
    pub public_url: String,
    pub admin_user: String,
    pub admin_password: String,
    pub log_level: String,
    /// Hard deadline for one request, so a stalled handler cannot pin a
    /// connection (and a worker thread) forever.
    pub request_timeout: Duration,
    pub limits: Limits,
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_bundle_bytes: usize,
    pub max_files: usize,
    pub max_file_bytes: usize,
}

impl Limits {
    fn from_env() -> Self {
        Self {
            max_bundle_bytes: num("MARKET_MAX_BUNDLE_BYTES", 2 * 1024 * 1024),
            max_files: num("MARKET_MAX_FILES", 100),
            max_file_bytes: num("MARKET_MAX_FILE_BYTES", 256 * 1024),
        }
    }
}

impl Config {
    /// Reads the environment and validates it. A bad value stops the process at
    /// start-up instead of turning into a confusing 4xx (or an `install_url`
    /// clients cannot fetch) later on.
    pub fn from_env() -> Result<Self, String> {
        let cfg = Self {
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
            log_level: std::env::var("MARKET_LOG").unwrap_or_else(|_| "info".into()),
            request_timeout: Duration::from_millis(
                num("MARKET_REQUEST_TIMEOUT_MS", 30_000).max(1) as u64,
            ),
            limits: Limits::from_env(),
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Every rule here is one that only shows up as a broken market: a zero
    /// limit rejects every publish, a relative `MARKET_PUBLIC_URL` produces
    /// links nobody can follow, a bad bind address fails on the first request.
    /// None of them can be fixed without a restart anyway.
    pub fn validate(&self) -> Result<(), String> {
        if self.limits.max_bundle_bytes == 0 {
            return Err("MARKET_MAX_BUNDLE_BYTES 必须大于 0（0 会让所有发布被拒）".into());
        }
        if self.limits.max_files == 0 {
            return Err("MARKET_MAX_FILES 必须大于 0（0 会让所有发布被拒）".into());
        }
        if self.limits.max_file_bytes == 0 {
            return Err("MARKET_MAX_FILE_BYTES 必须大于 0（0 会让所有发布被拒）".into());
        }
        validate_public_url(&self.public_url)?;
        validate_bind(&self.bind)?;
        if !self.admin_password.is_empty() && self.admin_password.chars().count() < 8 {
            return Err(
                "ADMIN_PASSWORD 至少 8 个字符（留空则首次启动生成随机密码并打印一次）".into(),
            );
        }
        crate::log::parse_level(&self.log_level).map_err(|err| format!("MARKET_LOG：{err}"))?;
        Ok(())
    }
}

/// It ends up in every `install_url` and `bundle_url`, so a relative value turns
/// into a link the host cannot fetch.
fn validate_public_url(url: &str) -> Result<(), String> {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return Err(format!(
            "MARKET_PUBLIC_URL={url} 必须以 http:// 或 https:// 开头"
        ));
    };
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(format!("MARKET_PUBLIC_URL={url} 缺少主机名"));
    }
    Ok(())
}

fn validate_bind(bind: &str) -> Result<(), String> {
    let Some((host, port)) = bind.rsplit_once(':') else {
        return Err(format!("MARKET_BIND={bind} 需要 host:port 形式，例如 0.0.0.0:8080"));
    };
    let port: u16 = port
        .parse()
        .map_err(|_| format!("MARKET_BIND={bind} 的端口必须是 0-65535 的数字"))?;
    if port == 0 {
        return Err(format!("MARKET_BIND={bind} 的端口不能是 0"));
    }
    if host.is_empty() {
        return Err(format!("MARKET_BIND={bind} 缺少主机地址，例如 0.0.0.0:8080"));
    }
    Ok(())
}

fn num(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sane() -> Config {
        Config {
            bind: "0.0.0.0:8080".into(),
            db_path: PathBuf::from("data/market.sqlite3"),
            public_url: "http://localhost:8080".into(),
            admin_user: "admin".into(),
            admin_password: String::new(),
            log_level: "info".into(),
            request_timeout: Duration::from_secs(30),
            limits: Limits {
                max_bundle_bytes: 2 * 1024 * 1024,
                max_files: 100,
                max_file_bytes: 256 * 1024,
            },
        }
    }

    #[test]
    fn accepts_a_sane_configuration() {
        assert_eq!(sane().validate(), Ok(()));
    }

    #[test]
    fn rejects_limits_that_would_reject_every_publish() {
        for zero in 0..3 {
            let mut cfg = sane();
            match zero {
                0 => cfg.limits.max_bundle_bytes = 0,
                1 => cfg.limits.max_files = 0,
                _ => cfg.limits.max_file_bytes = 0,
            }
            let err = cfg.validate().expect_err("a zero limit is refused");
            assert!(err.contains("MARKET_MAX_"), "{err}");
        }
    }

    #[test]
    fn rejects_a_public_url_clients_cannot_fetch() {
        for url in ["localhost:8080", "/market", "ftp://market", "http://", "http://a b"] {
            let mut cfg = sane();
            cfg.public_url = url.into();
            assert!(cfg.validate().is_err(), "{url} must be refused");
        }
        for url in ["http://market", "https://market.example.com:8443/sub"] {
            let mut cfg = sane();
            cfg.public_url = url.into();
            assert_eq!(cfg.validate(), Ok(()), "{url}");
        }
    }

    #[test]
    fn rejects_a_bind_address_that_cannot_listen() {
        for bind in ["8080", "0.0.0.0:", "0.0.0.0:0", "0.0.0.0:http", ":8080"] {
            let mut cfg = sane();
            cfg.bind = bind.into();
            assert!(cfg.validate().is_err(), "{bind} must be refused");
        }
        for bind in ["0.0.0.0:8080", "127.0.0.1:1", "[::]:8080"] {
            let mut cfg = sane();
            cfg.bind = bind.into();
            assert_eq!(cfg.validate(), Ok(()), "{bind}");
        }
    }

    #[test]
    fn rejects_a_guessable_admin_password_but_allows_the_generated_one() {
        let mut cfg = sane();
        cfg.admin_password = "short".into();
        let err = cfg.validate().expect_err("a five-character password is refused");
        assert!(err.contains("ADMIN_PASSWORD"), "{err}");

        cfg.admin_password = "long-enough".into();
        assert_eq!(cfg.validate(), Ok(()));

        cfg.admin_password = String::new();
        cfg.validate()
            .expect("an empty password means: generate one and print it");
    }

    #[test]
    fn rejects_an_unknown_log_level() {
        let mut cfg = sane();
        cfg.log_level = "loud".into();
        let err = cfg.validate().expect_err("an unknown level is refused");
        assert!(err.contains("MARKET_LOG"), "{err}");
        for level in ["off", "debug", "TRACE"] {
            cfg.log_level = level.into();
            assert_eq!(cfg.validate(), Ok(()), "{level}");
        }
    }
}
