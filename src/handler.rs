use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use crate::auth::SignedPath;
use crate::{AppState, CHANNEL_DEPTH, error, query, resolve_error, url};

const GEOMETRY_FAILURES: [&str; 3] = ["TopologyException", "IllegalArgumentException", "AssertionFailedException"];
const UNREADABLE_SOURCE: &str = "Could not open GDAL dataset";

pub async fn dataset(State(state): State<AppState>, SignedPath(path): SignedPath) -> Response {
    let parsed = match url::parse(&path, &state.grammar) {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let root = state.root;
    let format = parsed.format;
    let render = crate::render::Render::of(format);
    let download = format!("{}.{}", parsed.dataset, parsed.extension);
    let dataset = parsed.dataset;
    let encoding = parsed.encoding;

    let name = dataset.clone();
    let filters = parsed.filters;
    let selected = parsed.path;
    let source = match tokio::task::spawn_blocking(move || root.resolve(&name, selected.as_deref())).await {
        Ok(Ok(source)) => source,
        Ok(Err(e)) => return resolve_error(e),
        Err(e) => {
            tracing::error!(dataset, error = ?e, "resolve failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "dataset lookup failed");
        }
    };

    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), (StatusCode, String)>>();
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(CHANNEL_DEPTH);

    tokio::task::spawn_blocking(move || {
        if tx.blocking_send(Ok(Bytes::from_static(render.header.as_bytes()))).is_err() {
            return;
        }
        let mut ready = Some(ready_tx);
        let sent = query::run(
            &source,
            &encoding,
            render.separator,
            |columns, crs| crate::sql::build_sql(&source, &encoding, &filters, format, columns, crs),
            || {
                if let Some(ready_tx) = ready.take() {
                    let _ = ready_tx.send(Ok(()));
                }
            },
            &mut |chunk| tx.blocking_send(Ok(Bytes::from(chunk))).is_ok(),
        );
        match sent {
            Ok(()) => {
                let _ = tx.blocking_send(Ok(Bytes::from_static(render.footer.as_bytes())));
            }
            Err(e) => {
                tracing::error!(dataset, error = ?e, "query failed");
                match ready.take() {
                    Some(ready_tx) => {
                        let _ = ready_tx.send(Err(error_status(&e, &encoding)));
                    }
                    None => {
                        let _ = tx.blocking_send(Err(std::io::Error::other(format!("{e:#}"))));
                    }
                }
            }
        }
    });

    // status is committed before the first byte, so wait for a successful prepare
    match ready_rx.await {
        Ok(Ok(())) => {}
        Ok(Err((status, message))) => return error(status, message),
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "query failed"),
    }

    (
        [
            (header::CONTENT_TYPE, render.content_type.to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename=\"{download}\"")),
        ],
        Body::from_stream(ReceiverStream::new(rx)),
    )
        .into_response()
}

fn error_status(e: &anyhow::Error, encoding: &str) -> (StatusCode, String) {
    if let Some(no_geometry) = e.downcast_ref::<query::NoGeometry>() {
        return (StatusCode::UNPROCESSABLE_ENTITY, no_geometry.to_string());
    }
    if let Some(filter_error) = e.downcast_ref::<crate::filters::FilterError>() {
        let status = match filter_error {
            crate::filters::FilterError::NoIdColumn => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::BAD_REQUEST,
        };
        return (status, filter_error.to_string());
    }
    let reported = e.chain().map(|cause| cause.to_string()).collect::<Vec<_>>().join("; ");
    if GEOMETRY_FAILURES.iter().any(|marker| reported.contains(marker)) {
        return (StatusCode::UNPROCESSABLE_ENTITY, "source geometry could not be processed".to_string());
    }
    if reported.contains(UNREADABLE_SOURCE) {
        return (StatusCode::UNPROCESSABLE_ENTITY, "source could not be opened as a geospatial dataset".to_string());
    }
    if reported.contains("Invalid Input Error") {
        return (StatusCode::UNPROCESSABLE_ENTITY, format!("source could not be read with encoding {encoding}"));
    }
    (StatusCode::INTERNAL_SERVER_ERROR, "query failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_geometry_column_is_a_client_error() {
        let (status, message) = error_status(&anyhow::Error::new(query::NoGeometry), "UTF-8");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(message, "source has no geometry column");
    }

    #[test]
    fn an_unreadable_source_names_the_encoding_and_leaks_no_path() {
        let raw = anyhow::anyhow!(
            "Invalid Input Error: Malformed JSON at byte 0 of input. Input: /vsizip//srv/data/secret.zip/x.shp"
        );
        let (status, message) = error_status(&raw, "ISO-8859-1");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(message, "source could not be read with encoding ISO-8859-1");
        assert!(!message.contains("/srv/data"), "the data root leaked: {message}");
    }

    #[test]
    fn a_geometry_failure_is_not_blamed_on_the_encoding() {
        let raw = anyhow::anyhow!("Invalid Input Error: TopologyException: side location conflict at 0.5 0.5");
        let (status, message) = error_status(&raw, "ISO-8859-1");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(message, "source geometry could not be processed");
        assert!(!message.contains("encoding"), "a geometry failure blamed the encoding: {message}");
    }

    #[test]
    fn a_source_gdal_cannot_open_is_a_client_error() {
        let raw = anyhow::anyhow!(
            "IO Error: Could not open GDAL dataset at: /vsizip//srv/data/gbr.zip/geoBoundaries-GBR-ADM2-metaData.json"
        );
        let (status, message) = error_status(&raw, "UTF-8");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(message, "source could not be opened as a geospatial dataset");
        assert!(!message.contains("/srv/data"), "the data root leaked: {message}");
    }

    #[test]
    fn any_other_query_failure_stays_a_server_error() {
        let (status, message) = error_status(&anyhow::anyhow!("disk on fire"), "UTF-8");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(message, "query failed");
    }

    #[test]
    fn a_malformed_filter_is_a_client_error() {
        let raw = anyhow::Error::new(crate::filters::FilterError::BadParams("id".to_string()));
        let (status, message) = error_status(&raw, "UTF-8");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(message, "filter has the wrong number of parameters: id");
    }

    #[test]
    fn a_missing_id_column_is_unprocessable() {
        let raw = anyhow::Error::new(crate::filters::FilterError::NoIdColumn);
        let (status, message) = error_status(&raw, "UTF-8");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(message.contains("id column"), "{message}");
    }
}
