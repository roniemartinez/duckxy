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

pub async fn dataset(State(root): State<DatasetRoot>, SignedPath(path): SignedPath) -> Response {
    let parsed = match url::parse(&path) {
        Ok(p) => p,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let format = parsed.format;
    let source = match root.resolve(&parsed.dataset, parsed.path.as_deref()) {
        Ok(s) => s,
        Err(e) => return resolve_error(e),
    };

    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(CHANNEL_DEPTH);
    let download = format!("{}.{}", parsed.dataset, parsed.extension);
    let dataset = parsed.dataset;
    let encoding = parsed.encoding;

    tokio::task::spawn_blocking(move || {
        if tx.blocking_send(Ok(Bytes::from_static(format.header().as_bytes()))).is_err() {
            return;
        }
        let sent = query::run(
            &source,
            format,
            |geometry| build_sql(&source, &encoding, format, geometry),
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

    (
        [
            (header::CONTENT_TYPE, format.content_type().to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename=\"{download}\"")),
        ],
        Body::from_stream(ReceiverStream::new(rx)),
    )
        .into_response()
}

pub fn build_sql(source: &str, encoding: &str, format: Format, geometry: &str) -> String {
    let mut ctes = WithClause::new();
    let mut input = Alias::new("source");
    ctes.cte(
        CommonTableExpression::new()
            .query(
                Query::select()
                    .expr(Expr::cust("*"))
                    .from_function(
                        Func::cust("ST_Read")
                            .arg(source)
                            .arg(Expr::cust(format!("open_options=['ENCODING={encoding}']"))),
                        "src",
                    )
                    .take(),
            )
            .table_name(input.clone())
            .to_owned(),
    );

    let replaced = Query::select()
        .expr(Expr::cust_with_exprs(
            "* REPLACE ($1 AS $2)",
            [Func::cust("ST_AsGeoJSON").arg(Expr::col(Alias::new(geometry))).into(), Expr::col(Alias::new(geometry))],
        ))
        .from(input)
        .take();
    input = Alias::new("step_1");
    ctes.cte(CommonTableExpression::new().query(replaced).table_name(input.clone()).to_owned());

    match format {
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
    }
}
