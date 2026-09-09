use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use sea_query::{Alias, Expr, Func, PostgresQueryBuilder, Query};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use crate::auth::SignedPath;
use crate::dataset::DatasetRoot;
use crate::formats::Format;
use crate::{CHANNEL_DEPTH, error, query, resolve_error, url};

pub fn build_sql(source: &str, format: Format) -> String {
    let relation = Query::select()
        .expr(Expr::cust("*"))
        .from_function(Func::cust(Alias::new("ST_Read")).arg(source), Alias::new("src"))
        .take();

    match format {
        Format::GeoJson => {
            // REPLACE avoids adding a column; a source attribute named geom still
            // collides inside ST_Read itself, which we cannot rename away here
            let rendered = Query::select()
                .expr(Expr::cust("* REPLACE (ST_AsGeoJSON(geom) AS geom)"))
                .from_subquery(relation, Alias::new("src"))
                .take();
            Query::select()
                .expr(Expr::cust(
                    r#"json_object('type', 'Feature', 'properties', json_merge_patch(to_json(s), '{"geom": null}'), 'geometry', geom::JSON)::VARCHAR"#,
                ))
                .from_subquery(rendered, Alias::new("s"))
                .to_string(PostgresQueryBuilder)
        }
    }
}

pub async fn dataset(State(root): State<DatasetRoot>, SignedPath(path): SignedPath) -> Response {
    let parsed = match url::parse(&path) {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let Some(format) = Format::from_extension(&parsed.format) else {
        return error(StatusCode::BAD_REQUEST, format!("unknown format: {}", parsed.format));
    };

    let source = match root.resolve(&parsed.dataset) {
        Ok(s) => s,
        Err(e) => return resolve_error(e),
    };

    let sql = build_sql(&source, format);
    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(CHANNEL_DEPTH);
    let dataset = parsed.dataset;

    tokio::task::spawn_blocking(move || {
        if tx.blocking_send(Ok(Bytes::from_static(format.header().as_bytes()))).is_err() {
            return;
        }
        let sent = query::run(
            &sql,
            format,
            || {
                let _ = ready_tx.send(());
            },
            &mut |chunk| tx.blocking_send(Ok(Bytes::from(chunk))).is_ok(),
        );
        match sent {
            Ok(()) => {
                let _ = tx.blocking_send(Ok(Bytes::from_static(format.footer().as_bytes())));
            }
            Err(e) => {
                tracing::error!(dataset, error = ?e, "query failed");
                let _ = tx.blocking_send(Err(std::io::Error::other(format!("{e:#}"))));
            }
        }
    });

    // status is committed before the first byte, so wait for a successful prepare
    if ready_rx.await.is_err() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "query failed");
    }

    ([(header::CONTENT_TYPE, format.content_type())], Body::from_stream(ReceiverStream::new(rx))).into_response()
}
