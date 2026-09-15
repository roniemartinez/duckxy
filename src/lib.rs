pub mod auth;
pub mod backend;
pub mod dataset;
mod encodings;
pub mod filters;
pub mod formats;
pub mod grammar;
pub mod handler;
pub mod parexp;
pub mod query;
pub mod render;
pub mod routes;
pub mod sql;
pub mod url;

#[derive(Debug)]
pub struct Fault {
    pub status: StatusCode,
    pub message: String,
}

impl Fault {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self::new(StatusCode::BAD_REQUEST, message))
    }

    pub fn unprocessable(message: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self::new(StatusCode::UNPROCESSABLE_ENTITY, message))
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Fault {}

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::FromRef;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use auth::Auth;
use dataset::{DatasetRoot, ResolveError};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CHANNEL_DEPTH: usize = 4;
const DEFAULT_BIND: &str = "0.0.0.0:5757";
const DEFAULT_DATA_ROOT: &str = "./data";

pub struct Config {
    pub bind: String,
    pub data_root: PathBuf,
    pub key: Option<String>,
    pub allow_insecure: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            bind: env_or("DUCKXY_BIND", DEFAULT_BIND),
            data_root: env_or("DUCKXY_DATA_ROOT", DEFAULT_DATA_ROOT).into(),
            key: std::env::var("DUCKXY_KEY").ok().filter(|k| !k.trim().is_empty()),
            allow_insecure: enabled(&env_or("DUCKXY_ALLOW_INSECURE", "false")),
        }
    }
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn enabled(value: &str) -> bool {
    matches!(value.trim(), "true" | "1")
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

pub fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(ErrorBody { error: message.into() })).into_response()
}

pub fn resolve_error(e: ResolveError) -> Response {
    match e {
        ResolveError::NotFound(_) | ResolveError::PathNotFound { .. } => error(StatusCode::NOT_FOUND, e.to_string()),
        ResolveError::EmptyArchive(_) => error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
        ResolveError::NotAnArchive(_) => error(StatusCode::BAD_REQUEST, e.to_string()),
        ResolveError::UnreadableArchive(_) => {
            tracing::error!(error = %e, "archive could not be read");
            error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
        // e.to_string() would leak the data root path
        ResolveError::InvalidRoot(_) => {
            tracing::error!(error = %e, "data root is unusable");
            error(StatusCode::INTERNAL_SERVER_ERROR, "data root is unusable")
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub root: DatasetRoot,
    pub auth: Auth,
    pub grammar: Arc<grammar::Grammar>,
    pub backend: Arc<backend::Backend>,
}

impl AppState {
    pub fn new(root: DatasetRoot, auth: Auth, grammar: grammar::Grammar) -> Self {
        Self { root, auth, grammar: Arc::new(grammar), backend: Arc::new(backend::Backend::default()) }
    }

    pub fn with_backend(mut self, backend: backend::Backend) -> Self {
        self.backend = Arc::new(backend);
        self
    }
}

impl FromRef<AppState> for Auth {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

#[cfg(test)]
pub(crate) fn ensure_spatial() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| query::install_extensions().expect("install spatial"));
}

#[cfg(test)]
mod tests {
    use super::enabled;
    use rstest::rstest;

    #[rstest]
    #[case("true", true)]
    #[case("1", true)]
    #[case("false", false)]
    #[case("yes", false)]
    fn enabled_env_values(#[case] value: &str, #[case] expected: bool) {
        assert_eq!(enabled(value), expected);
    }
}
