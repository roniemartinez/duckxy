use axum::{Json, Router, http::StatusCode, http::header, response::IntoResponse, routing::get};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_BIND: &str = "0.0.0.0:5757";

#[derive(Serialize)]
struct ServiceInfo {
    service: &'static str,
    version: &'static str,
}

async fn root() -> impl IntoResponse {
    (StatusCode::OK, Json(ServiceInfo { service: "duckxy", version: VERSION }))
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

const FAVICON_ICO: &[u8] = include_bytes!("../favicon.ico");
const FAVICON_SVG: &[u8] = include_bytes!("../favicon.svg");
const ICON_CACHE_CONTROL: &str = "public, max-age=86400, immutable";

async fn favicon_ico() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/vnd.microsoft.icon"), (header::CACHE_CONTROL, ICON_CACHE_CONTROL)],
        FAVICON_ICO,
    )
}

async fn favicon_svg() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, ICON_CACHE_CONTROL)],
        FAVICON_SVG,
    )
}

fn router() -> Router {
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/favicon.ico", get(favicon_ico))
        .route("/favicon.svg", get(favicon_svg))
        .layer(TraceLayer::new_for_http())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate()).expect("install SIGTERM handler").recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("shutdown signal received");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))).init();

    let bind = std::env::var("DUCKXY_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let listener = TcpListener::bind(&bind).await?;
    info!(%bind, version = VERSION, "duckxy starting");

    axum::serve(listener, router()).with_graceful_shutdown(shutdown_signal()).await?;
    Ok(())
}
