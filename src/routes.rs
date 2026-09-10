use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::{Router, routing::get};
use tower_http::trace::TraceLayer;

use crate::AppState;
use crate::handler;

const FAVICON_ICO: &[u8] = include_bytes!("../favicon.ico");
const ICON_CACHE: &str = "public, max-age=86400, immutable";

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn favicon_ico() -> impl IntoResponse {
    let headers = [(header::CONTENT_TYPE, "image/vnd.microsoft.icon"), (header::CACHE_CONTROL, ICON_CACHE)];
    (StatusCode::OK, headers, FAVICON_ICO)
}

fn request_span(request: &Request) -> tracing::Span {
    let path = request.uri().path();
    let redacted = match path.trim_start_matches('/').split_once('/') {
        Some((_signature, rest)) => format!("/<redacted>/{rest}"),
        None => path.to_string(),
    };
    tracing::info_span!("request", method = %request.method(), path = %redacted)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/favicon.ico", get(favicon_ico))
        .route("/{signature}/{*path}", get(handler::dataset))
        .layer(TraceLayer::new_for_http().make_span_with(request_span))
}

pub fn router(state: AppState) -> Router {
    routes().with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Auth;
    use crate::dataset::DatasetRoot;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use rstest::rstest;
    use std::fs;
    use tower::ServiceExt;

    const KEY: &str = "00112233445566778899aabbccddeeff";
    const POINTS: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"alpha"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;

    fn state(tag: &str, allow_insecure: bool) -> AppState {
        crate::ensure_spatial();
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("duckxy-http-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pts.geojson"), POINTS).unwrap();
        AppState { root: DatasetRoot::new(dir), auth: Auth::new(Some(KEY), allow_insecure).unwrap() }
    }

    async fn get(state: AppState, uri: &str) -> (StatusCode, String) {
        let response = router(state).oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    fn signed(state: &AppState, path: &str) -> String {
        format!("/{}{}", state.auth.sign(path).unwrap(), path)
    }

    #[rstest]
    #[case("/@dataset:pts.geojson")]
    #[case("/@dataset:pts.json")]
    #[case("/@dataset:pts,enc:utf-8.geojson")]
    #[tokio::test]
    async fn a_signed_request_is_served(#[case] path: &str) {
        let s = state("ok", false);
        let (status, body) = get(s.clone(), &signed(&s, path)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("alpha"), "{body}");
    }

    #[tokio::test]
    async fn an_unsigned_request_is_forbidden() {
        let s = state("unsigned", false);
        let (status, _) = get(s, "/@dataset:pts/output.geojson").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_signature_for_another_path_is_forbidden() {
        let s = state("wrong", false);
        let sig = s.auth.sign("/@dataset:other/output.geojson").unwrap();
        let (status, _) = get(s, &format!("/{sig}/@dataset:pts/output.geojson")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn insecure_works_only_when_enabled() {
        let path = "/@dataset:pts.geojson";
        let (status, _) = get(state("ins-off", false), &format!("/insecure{path}")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, body) = get(state("ins-on", true), &format!("/insecure{path}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("alpha"), "{body}");
    }

    #[rstest]
    #[case("/@dataset:pts.xml", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:nope.geojson", StatusCode::NOT_FOUND)]
    #[tokio::test]
    async fn errors_keep_their_status_behind_a_valid_signature(#[case] path: &str, #[case] expected: StatusCode) {
        let s = state("err", false);
        let (status, _) = get(s.clone(), &signed(&s, path)).await;
        assert_eq!(status, expected);
    }

    #[tokio::test]
    async fn health_needs_no_signature() {
        let (status, body) = get(state("health", false), "/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }
}
