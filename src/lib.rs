pub mod auth;
pub mod dataset;
mod encodings;
pub mod formats;
pub mod handler;
pub mod query;
pub mod routes;
pub mod url;

use std::path::PathBuf;

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
        ResolveError::NotFound(_) | ResolveError::EmptyArchive(_) => error(StatusCode::NOT_FOUND, e.to_string()),
        ResolveError::PathNotFound { .. } | ResolveError::NotAnArchive(_) => {
            error(StatusCode::BAD_REQUEST, e.to_string())
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
}

impl FromRef<AppState> for DatasetRoot {
    fn from_ref(state: &AppState) -> Self {
        state.root.clone()
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
