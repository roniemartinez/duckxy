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
        {"type":"Feature","properties":{"id":1,"name":"alpha","pop":100,"code":"a_1"},
         "geometry":{"type":"Point","coordinates":[1,2]}},
        {"type":"Feature","properties":{"id":2,"name":"beta","pop":900,"code":"aX1"},
         "geometry":{"type":"Point","coordinates":[3,4]}}]}"#;

    const SHAPES: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"point"},"geometry":{"type":"Point","coordinates":[1,2]}},
        {"type":"Feature","properties":{"name":"bowtie"},
         "geometry":{"type":"Polygon","coordinates":[[[0,0],[1,1],[1,0],[0,1],[0,0]]]}},
        {"type":"Feature","properties":{"name":"ring"},
         "geometry":{"type":"LineString","coordinates":[[0,0],[1,0],[1,1],[0,0]]}},
        {"type":"Feature","properties":{"name":"line"},
         "geometry":{"type":"LineString","coordinates":[[0,0],[1,1]]}}]}"#;

    const NO_ID: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"alpha"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;

    fn state(tag: &str, allow_insecure: bool) -> AppState {
        crate::ensure_spatial();
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("duckxy-http-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pts.geojson"), POINTS).unwrap();
        fs::write(dir.join("noid.geojson"), NO_ID).unwrap();
        fs::write(dir.join("shapes.geojson"), SHAPES).unwrap();
        AppState {
            root: DatasetRoot::new(dir),
            auth: Auth::new(Some(KEY), allow_insecure).unwrap(),
            grammar: std::sync::Arc::new(crate::grammar::Grammar::core()),
        }
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
    #[case("/@ds:pts.geojson")]
    #[case("/@ds:pts/output.geojson")]
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

    #[rstest]
    #[case("/@dataset:pts.geojson", "pts.geojson")]
    #[case("/@dataset:pts/output.geojson", "pts.geojson")]
    #[case("/@dataset:pts/my.export.json", "pts.json")]
    #[tokio::test]
    async fn the_download_filename_comes_from_the_dataset(#[case] path: &str, #[case] expected: &str) {
        let s = state("download", false);
        let uri = signed(&s, path);
        let response = router(s).oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let disposition = response.headers().get(header::CONTENT_DISPOSITION).unwrap().to_str().unwrap();
        assert_eq!(disposition, format!("inline; filename=\"{expected}\""));
    }

    #[tokio::test]
    async fn health_needs_no_signature() {
        let (status, body) = get(state("health", false), "/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }

    #[rstest]
    #[case("/@dataset:pts,id:1.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,id:2.geojson", vec!["beta"])]
    #[case("/@dataset:pts,id:99.geojson", vec![])]
    #[case("/@ds:pts,id:2/export.geojson", vec!["beta"])]
    #[case("/@dataset:pts,enc:utf-8,id:1.geojson", vec!["alpha"])]
    #[case("/@dataset:pts.geojson", vec!["alpha", "beta"])]
    #[case("/@dataset:pts,id:~1~.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,id:01.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,id:(1,2).geojson", vec!["alpha", "beta"])]
    #[case("/@dataset:pts,id:(1,2,5,7).geojson", vec!["alpha", "beta"])]
    #[case("/@dataset:pts,id:(5,7,2).geojson", vec!["beta"])]
    #[case("/@dataset:pts,id:(5,7,11).geojson", vec![])]
    #[case("/@dataset:pts,id:(1..2).geojson", vec!["alpha", "beta"])]
    #[case("/@dataset:pts,id:(1..1).geojson", vec!["alpha"])]
    #[case("/@dataset:pts,id:(2..9).geojson", vec!["beta"])]
    #[case("/@dataset:pts,id:(90..99).geojson", vec![])]
    #[case("/@dataset:pts,id:(1..1000000).geojson", vec!["alpha", "beta"])]
    #[case("/@dataset:pts,id:1,id:2.geojson", vec![])]
    #[case("/@dataset:pts,id:gte:2.geojson", vec!["beta"])]
    #[case("/@dataset:pts,id:lt:2.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,id:in:(1..2).geojson", vec!["alpha", "beta"])]
    #[case("/@dataset:pts,id:gte:1,id:lte:1.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,id:ne:1.geojson", vec!["beta"])]
    #[tokio::test]
    async fn an_id_filter_returns_exactly_the_matching_features(#[case] path: &str, #[case] expected: Vec<&str>) {
        let s = state("filter", false);
        let (status, body) = get(s.clone(), &signed(&s, path)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{body} ({e})"));
        let names: Vec<&str> =
            parsed["features"].as_array().unwrap().iter().map(|f| f["properties"]["name"].as_str().unwrap()).collect();
        assert_eq!(names, expected, "{body}");
    }

    #[rstest]
    #[case("/@dataset:pts,prop:name:beta.geojson", vec!["beta"])]
    #[case("/@dataset:pts,prop:name:eq:alpha.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,prop:name:ne:alpha.geojson", vec!["beta"])]
    #[case("/@dataset:pts,prop:name:ieq:ALPHA.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,prop:pop:gt:500.geojson", vec!["beta"])]
    #[case("/@dataset:pts,prop:pop:lt:500.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,prop:pop:gte:900.geojson", vec!["beta"])]
    #[case("/@dataset:pts,prop:pop:lte:100.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,prop:name:gt:b.geojson", vec!["beta"])]
    #[case("/@dataset:pts,prop:name:lt:b.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,prop:name:sw:al.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,prop:name:ew:ta.geojson", vec!["beta"])]
    #[case("/@dataset:pts,prop:name:ct:et.geojson", vec!["beta"])]
    #[case("/@dataset:pts,prop:code:ct:a_1.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,prop:name:in:(alpha,beta).geojson", vec!["alpha", "beta"])]
    #[case("/@dataset:pts,prop:name:(alpha,beta).geojson", vec!["alpha", "beta"])]
    #[case("/@dataset:pts,prop:name:nin:(alpha).geojson", vec!["beta"])]
    #[case("/@dataset:pts,prop:pop:gt:500,prop:name:beta.geojson", vec!["beta"])]
    #[case("/@dataset:pts,enc:utf-8,prop:name:alpha.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,id:1,prop:name:alpha.geojson", vec!["alpha"])]
    #[case("/@dataset:pts,prop:name:~alpha~.geojson", vec!["alpha"])]
    #[tokio::test]
    async fn a_prop_filter_returns_exactly_the_matching_features(#[case] path: &str, #[case] expected: Vec<&str>) {
        let s = state("prop", false);
        let (status, body) = get(s.clone(), &signed(&s, path)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{body} ({e})"));
        let names: Vec<&str> =
            parsed["features"].as_array().unwrap().iter().map(|f| f["properties"]["name"].as_str().unwrap()).collect();
        assert_eq!(names, expected, "{body}");
    }

    #[rstest]
    #[case("/@dataset:shapes,type:Point.geojson", vec!["point"])]
    #[case("/@dataset:shapes,type:point.geojson", vec!["point"])]
    #[case("/@dataset:shapes,type:LineString.geojson", vec!["ring", "line"])]
    #[case("/@dataset:shapes,type:Polygon.geojson", vec!["bowtie"])]
    #[case("/@dataset:shapes,type:MultiPoint.geojson", vec![])]
    #[case("/@dataset:shapes,valid:false.geojson", vec!["bowtie"])]
    #[case("/@dataset:shapes,valid:true.geojson", vec!["point", "ring", "line"])]
    #[case("/@dataset:shapes,simple:false.geojson", vec!["bowtie"])]
    #[case("/@dataset:shapes,empty:false.geojson", vec!["point", "bowtie", "ring", "line"])]
    #[case("/@dataset:shapes,empty:true.geojson", vec![])]
    #[case("/@dataset:shapes,closed:true.geojson", vec!["ring"])]
    #[case("/@dataset:shapes,closed:false.geojson", vec!["line"])]
    #[case("/@dataset:shapes,closed:1.geojson", vec!["ring"])]
    #[case("/@dataset:shapes,type:LineString,closed:false.geojson", vec!["line"])]
    #[case("/@dataset:shapes,valid:true,prop:name:point.geojson", vec!["point"])]
    #[tokio::test]
    async fn a_geometry_predicate_returns_exactly_the_matching_features(
        #[case] path: &str,
        #[case] expected: Vec<&str>,
    ) {
        let s = state("shape", false);
        let (status, body) = get(s.clone(), &signed(&s, path)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{body} ({e})"));
        let names: Vec<&str> =
            parsed["features"].as_array().unwrap().iter().map(|f| f["properties"]["name"].as_str().unwrap()).collect();
        assert_eq!(names, expected, "{body}");
    }

    #[rstest]
    #[case("/@dataset:pts,zzz:1.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,prop:nosuch:1.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,prop:name:zzz:1.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,prop:pop:gt:abc.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,valid:yes.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,type:Banana.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,type:ST_Point.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,prop:pop:eq:abc.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,id:inf.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,id:NaN.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,id:1e400.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,id:(a,b).geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,id:file-(1..3).txt.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,prop:only.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,enc:utf-8,enc:latin1.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,id:1:2.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,id:~1,enc:latin1.geojson", StatusCode::BAD_REQUEST)]
    #[case("/@dataset:pts,id:1,enc:utf-8.geojson", StatusCode::BAD_REQUEST)]
    #[tokio::test]
    async fn bad_filters_are_client_errors(#[case] path: &str, #[case] expected: StatusCode) {
        let s = state("filter-err", false);
        let (status, body) = get(s.clone(), &signed(&s, path)).await;
        assert_eq!(status, expected, "{body}");
    }

    #[tokio::test]
    async fn an_id_filter_without_an_id_column_is_unprocessable() {
        let s = state("noid", false);
        let (status, body) = get(s.clone(), &signed(&s, "/@dataset:noid,id:1.geojson")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("id column"), "{body}");
    }
}
