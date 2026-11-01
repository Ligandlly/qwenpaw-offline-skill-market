//! Shared application state.

use crate::auth::{LoginLimiter, Sessions};
use crate::config::Config;
use rusqlite::Connection;
use std::sync::{Arc, Mutex, MutexGuard};

pub struct App {
    pub db: Mutex<Connection>,
    pub sessions: Mutex<Sessions>,
    pub logins: Mutex<LoginLimiter>,
    pub cfg: Config,
}

pub type Shared = Arc<App>;

impl App {
    pub fn db(&self) -> MutexGuard<'_, Connection> {
        self.db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn sessions(&self) -> MutexGuard<'_, Sessions> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn logins(&self) -> MutexGuard<'_, LoginLimiter> {
        self.logins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
