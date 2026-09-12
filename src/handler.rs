use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use crate::auth::SignedPath;
use crate::dataset::DatasetRoot;
use crate::formats::Format;
use crate::{CHANNEL_DEPTH, error, query, resolve_error, url};
use sea_query::{Alias, CommonTableExpression, Expr, Func, PostgresQueryBuilder, Query, WithClause};

const GEOMETRY_FAILURES: [&str; 3] = ["TopologyException", "IllegalArgumentException", "AssertionFailedException"];

pub async fn dataset(State(root): State<DatasetRoot>, SignedPath(path): SignedPath) -> Response {
    let parsed = match url::parse(&path) {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let format = parsed.format;
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
        if tx.blocking_send(Ok(Bytes::from_static(format.header().as_bytes()))).is_err() {
            return;
        }
        let mut ready = Some(ready_tx);
        let sent = query::run(
            &source,
            &encoding,
            format,
            |columns| build_sql(&source, &encoding, &filters, format, columns),
            || {
                if let Some(ready_tx) = ready.take() {
                    let _ = ready_tx.send(Ok(()));
                }
            },
            &mut |chunk| tx.blocking_send(Ok(Bytes::from(chunk))).is_ok(),
        );
        match sent {
            Ok(()) => {
                let _ = tx.blocking_send(Ok(Bytes::from_static(format.footer().as_bytes())));
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
            (header::CONTENT_TYPE, format.content_type().to_string()),
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
    if reported.contains("Invalid Input Error") {
        return (StatusCode::UNPROCESSABLE_ENTITY, format!("source could not be read with encoding {encoding}"));
    }
    (StatusCode::INTERNAL_SERVER_ERROR, "query failed".to_string())
}

pub fn build_sql(
    source: &str,
    encoding: &str,
    filters: &[url::Segment],
    format: Format,
    columns: &[(String, String)],
) -> anyhow::Result<String> {
    let geometry = query::geometry_of(columns).expect("checked by run");
    let mut ctes = WithClause::new();
    let mut input = Alias::new("source");
    ctes.cte(
        CommonTableExpression::new()
            .query(
                Query::select().expr(Expr::cust("*")).from_function(query::read_source(source, encoding), "src").take(),
            )
            .table_name(input.clone())
            .to_owned(),
    );

    if let Some(predicate) = crate::filters::condition(filters, columns)? {
        let filtered = Query::select().expr(Expr::cust("*")).from(input.clone()).and_where(predicate).take();
        input = Alias::new("filtered");
        ctes.cte(CommonTableExpression::new().query(filtered).table_name(input.clone()).to_owned());
    }

    let replaced = Query::select()
        .expr(Expr::cust_with_exprs(
            "* REPLACE ($1 AS $2)",
            [Func::cust("ST_AsGeoJSON").arg(Expr::col(Alias::new(geometry))).into(), Expr::col(Alias::new(geometry))],
        ))
        .from(input.clone())
        .take();
    input = Alias::new("step_1");
    ctes.cte(CommonTableExpression::new().query(replaced).table_name(input.clone()).to_owned());

    Ok(match format {
        Format::GeoJson => Query::select()
            .expr(Func::cast_as(
                Func::cust("json_object").args([
                    Expr::val("type"),
                    Expr::val("Feature"),
                    Expr::val("properties"),
                    Func::cust("json_merge_patch")
                        .arg(Func::cust("to_json").arg(Expr::col(input.clone())))
                        .arg(Func::cust("json_object").args([Expr::val(geometry), Expr::cust("NULL")]))
                        .into(),
                    Expr::val("geometry"),
                    Func::cast_as(Expr::col(Alias::new(geometry)), "JSON").into(),
                ]),
                "VARCHAR",
            ))
            .from(input)
            .to_owned()
            .with(ctes)
            .to_string(PostgresQueryBuilder),
    })
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
        assert_eq!(message, "filter takes exactly one value: id");
    }

    #[test]
    fn a_missing_id_column_is_unprocessable() {
        let raw = anyhow::Error::new(crate::filters::FilterError::NoIdColumn);
        let (status, message) = error_status(&raw, "UTF-8");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(message.contains("id column"), "{message}");
    }

    #[test]
    fn an_unfiltered_request_adds_no_cte() {
        let columns = vec![("name".to_string(), "VARCHAR".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let out = build_sql("/x.geojson", "UTF-8", &[], Format::GeoJson, &columns).unwrap();
        assert!(!out.contains("filtered"), "an empty filter list added a cte: {out}");
    }

    #[test]
    fn a_filtered_request_adds_one_cte() {
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let filters = vec![url::Segment { name: "id".to_string(), params: vec!["7".to_string()] }];
        let out = build_sql("/x.geojson", "UTF-8", &filters, Format::GeoJson, &columns).unwrap();
        assert!(out.contains("\"filtered\""), "{out}");
        assert!(out.contains("CAST(\"id\" AS VARCHAR) = '7'"), "{out}");
    }
}
