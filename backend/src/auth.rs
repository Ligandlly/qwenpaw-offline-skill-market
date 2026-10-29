//! Two identity planes, deliberately not unified:
//!
//! * tenant plane — `X-Tenant-Id` header, trusted because the only caller is
//!   the QwenPaw plugin running inside that tenant's runtime (PLAN §1). It is
//!   NOT authentication; the backend is expected to be network-isolated.
//! * admin plane — username + Argon2 password, server-side session cookie.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use std::collections::HashMap;

use crate::api::ApiError;
use crate::db::now;

pub const SESSION_COOKIE: &str = "market_admin";
pub const SESSION_TTL_SECS: i64 = 12 * 3600;

#[derive(Clone, Debug)]
pub struct TenantId(pub String);

impl TenantId {
    pub fn actor(&self) -> String {
        format!("tenant:{}", self.0)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for TenantId {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let raw = parts
            .headers
            .get("x-tenant-id")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .unwrap_or_default();
        if raw.is_empty() {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "missing_tenant",
                "缺少 X-Tenant-Id 请求头（插件需要转发 QWENPAW_TENANT_ID）",
            ));
        }
        let ok = raw.len() <= 128
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        if !ok {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_tenant",
                "X-Tenant-Id 含有不支持的字符",
            ));
        }
        Ok(TenantId(raw.to_string()))
    }
}

#[derive(Default)]
pub struct Sessions {
    inner: HashMap<String, Session>,
}

pub struct Session {
    pub username: String,
    pub expires_at: i64,
}

impl Sessions {
    pub fn issue(&mut self, username: &str) -> String {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill(&mut bytes);
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        self.inner.insert(
            token.clone(),
            Session {
                username: username.to_string(),
                expires_at: now() + SESSION_TTL_SECS,
            },
        );
        self.purge();
        token
    }

    pub fn username_for(&mut self, token: &str) -> Option<String> {
        self.purge();
        self.inner
            .get(token)
            .map(|session| session.username.clone())
    }

    pub fn revoke(&mut self, token: &str) {
        self.inner.remove(token);
    }

    fn purge(&mut self) {
        let cutoff = now();
        self.inner.retain(|_, session| session.expires_at > cutoff);
    }
}

/// Pull the session token out of the raw `Cookie` header (no cookie crate).
pub fn session_token(cookie_header: Option<&str>) -> Option<String> {
    cookie_header?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_string())
}

pub fn set_cookie(token: &str) -> String {
    format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_TTL_SECS}")
}

pub fn clear_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| err.to_string())
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    match PasswordHash::new(stored) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Admin identity, extracted from the session cookie.
#[derive(Clone, Debug)]
pub struct Admin(pub String);

impl Admin {
    pub fn actor(&self) -> String {
        format!("admin:{}", self.0)
    }
}
