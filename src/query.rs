use crate::formats::Format;
use anyhow::{Context, Result};
use duckdb::Connection;
use sea_query::{Expr, Func, FunctionCall, PostgresQueryBuilder, Query};
use std::cell::RefCell;

const CHUNK_BYTES: usize = 64 * 1024;

thread_local! {
    static CONNECTION: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

#[derive(Debug)]
pub struct NoGeometry;

impl std::fmt::Display for NoGeometry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "source has no geometry column")
    }
}

impl std::error::Error for NoGeometry {}

pub fn install_extensions() -> Result<()> {
    let conn = Connection::open_in_memory().context("open duckdb")?;
    conn.execute_batch("INSTALL spatial; LOAD spatial;").context("install the spatial extension")?;
    match tuning() {
        "" => Ok(()),
        limits => conn.execute_batch(limits).context("apply the duckdb limits"),
    }
}

pub(crate) fn with_connection<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    CONNECTION.with(|cell| {
        if cell.borrow().is_none() {
            let conn = Connection::open_in_memory().context("open duckdb")?;
            conn.execute_batch(&format!("LOAD spatial; {}", tuning())).context("load the spatial extension")?;
            conn.register_table_function::<crate::parexp::Parexp>("parexp").context("register parexp")?;
            *cell.borrow_mut() = Some(conn);
        }
        let outcome = {
            let held = cell.borrow();
            f(held.as_ref().expect("just opened"))
        };
        if outcome.as_ref().is_err_and(database_is_invalidated) {
            tracing::warn!("discarding a duckdb connection whose database was invalidated");
            *cell.borrow_mut() = None;
        }
        outcome
    })
}

fn database_is_invalidated(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string();
        text.contains("database has been invalidated")
            || text.contains("FATAL Error")
            || text.contains("INTERNAL Error")
    })
}

fn tuning() -> &'static str {
    static TUNING: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TUNING.get_or_init(|| {
        let mut sql = String::new();
        if let Some(memory) = setting("DUCKXY_MEMORY_LIMIT", valid_memory_limit) {
            sql.push_str(&format!("SET memory_limit = '{memory}';"));
        }
        if let Some(threads) = setting("DUCKXY_THREADS", |v| v.parse::<u16>().is_ok_and(|n| n > 0)) {
            sql.push_str(&format!("SET threads = {threads};"));
        }
        sql
    })
}

fn setting(key: &str, valid: impl Fn(&str) -> bool) -> Option<String> {
    let value = std::env::var(key).ok()?;
    let value = value.trim();
    if valid(value) {
        return Some(value.to_string());
    }
    tracing::warn!(key, value, "ignoring unusable setting, keeping the duckdb default");
    None
}

fn valid_memory_limit(value: &str) -> bool {
    let size = value.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%');
    let unit = value[size.len()..].to_ascii_uppercase();
    !size.is_empty()
        && size.chars().all(|c| c.is_ascii_digit() || c == '.')
        && size.parse::<f64>().is_ok()
        && matches!(unit.as_str(), "" | "%" | "KB" | "MB" | "GB" | "TB" | "KIB" | "MIB" | "GIB" | "TIB")
}

pub fn read_source(source: &str, encoding: &str) -> FunctionCall {
    Func::cust("ST_Read")
        .arg(source)
        .arg(Expr::cust_with_exprs("open_options=list_value($1)", [Expr::val(format!("ENCODING={encoding}"))]))
}

pub fn run(
    source: &str,
    encoding: &str,
    format: Format,
    sql_for: impl FnOnce(&[(String, String)]) -> Result<String>,
    on_ready: impl FnOnce(),
    sink: &mut dyn FnMut(String) -> bool,
) -> Result<()> {
    with_connection(|conn| {
        let columns = describe(conn, source, encoding)?;
        geometry_of(&columns).ok_or(NoGeometry)?;
        let sql = sql_for(&columns)?;

        let mut stmt = conn.prepare(&sql).context("prepare query")?;
        let mut rows = stmt.query([]).context("run query")?;
        on_ready();

        let mut buf = String::with_capacity(CHUNK_BYTES * 2);
        let mut first = true;

        while let Some(row) = rows.next().context("read row")? {
            let Some(text) = row.get::<_, Option<String>>(0).context("read row")? else { continue };
            if !first {
                buf.push_str(format.separator());
            }
            first = false;
            buf.push_str(&text);
            if buf.len() >= CHUNK_BYTES && !sink(std::mem::take(&mut buf)) {
                return Ok(());
            }
        }

        if !buf.is_empty() {
            sink(buf);
        }
        Ok(())
    })
}

pub fn describe(conn: &Connection, source: &str, encoding: &str) -> Result<Vec<(String, String)>> {
    let relation = Query::select()
        .expr(Expr::cust("*"))
        .from_function(read_source(source, encoding), "src")
        .to_string(PostgresQueryBuilder);
    let sql = format!("SELECT column_name, column_type FROM (DESCRIBE {relation})");
    let mut stmt = conn.prepare(&sql).context("describe source")?;
    let mut rows = stmt.query([]).context("describe source")?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().context("describe source")? {
        let name: Option<String> = row.get(0).context("describe source")?;
        let kind: Option<String> = row.get(1).context("describe source")?;
        if let (Some(name), Some(kind)) = (name, kind) {
            columns.push((name, kind));
        }
    }
    Ok(columns)
}

pub fn geometry_of(columns: &[(String, String)]) -> Option<&str> {
    columns.iter().find(|(_, kind)| kind.starts_with("GEOMETRY")).map(|(name, _)| name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::Format;
    use std::fs;

    const TWO: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"alpha"},"geometry":{"type":"Point","coordinates":[1,2]}},
        {"type":"Feature","properties":{"name":"beta"},"geometry":{"type":"Point","coordinates":[3,4]}}]}"#;

    const WITH_NULL: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"alpha"},"geometry":{"type":"Point","coordinates":[1,2]}},
        {"type":"Feature","properties":{"name":"nogeom"},"geometry":null}]}"#;

    fn fixture(tag: &str, body: &str) -> String {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("duckxy-q-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.geojson");
        fs::write(&path, body).unwrap();
        path.to_str().unwrap().to_string()
    }

    fn collect(source: &str, ext: &str) -> String {
        crate::ensure_spatial();
        let f = Format::from_extension(ext).unwrap();
        let mut out = String::from(f.header());
        run(
            source,
            crate::url::DEFAULT_ENCODING,
            f,
            |c| crate::sql::build_sql(source, crate::url::DEFAULT_ENCODING, &[], f, c),
            || {},
            &mut |chunk| {
                out.push_str(&chunk);
                true
            },
        )
        .unwrap();
        out.push_str(f.footer());
        out
    }

    #[test]
    fn a_source_whose_geometry_column_is_not_called_geom_still_renders() {
        crate::ensure_spatial();
        let dir = std::env::temp_dir().join(format!("duckxy-gdb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let gdb = dir.join("ds.gdb");

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("LOAD spatial;").unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT ST_Point(1, 2) AS geom, 7 AS id) TO '{}' \
             WITH (FORMAT GDAL, DRIVER 'OpenFileGDB', LAYER_NAME 'ds', GEOMETRY_TYPE 'Point')",
            gdb.display()
        ))
        .unwrap();

        let src = crate::dataset::DatasetRoot::new(dir).resolve("ds", None).unwrap();
        let mut out = String::from(Format::GeoJson.header());
        run(
            &src,
            crate::url::DEFAULT_ENCODING,
            Format::GeoJson,
            |c| crate::sql::build_sql(&src, crate::url::DEFAULT_ENCODING, &[], Format::GeoJson, c),
            || {},
            &mut |chunk| {
                out.push_str(&chunk);
                true
            },
        )
        .unwrap();
        out.push_str(Format::GeoJson.footer());

        assert!(out.contains(r#""coordinates":[1.0,2.0]"#), "{out}");
        assert!(!out.contains("SHAPE"), "the geometry column leaked into properties: {out}");
    }

    #[test]
    fn the_encoding_reaches_sql_as_an_escaped_literal() {
        let sql = Query::select()
            .expr(Expr::cust("*"))
            .from_function(read_source("/x.geojson", "UTF-8', bogus='1"), "src")
            .to_string(PostgresQueryBuilder);
        assert!(!sql.contains("bogus='1'"), "the encoding closed the literal: {sql}");
        assert!(sql.contains("open_options=list_value("), "{sql}");
    }

    #[rstest::rstest]
    #[case("1GB", true)]
    #[case("512MB", true)]
    #[case("1.5GB", true)]
    #[case("512MiB", true)]
    #[case("1073741824", true)]
    #[case("9ZZ", false)]
    #[case("1.5.5GB", false)]
    #[case("1..GB", false)]
    #[case("1GBB", false)]
    #[case("GB", false)]
    #[case("80%", true)]
    #[case("", false)]
    #[case("   ", false)]
    #[case("abc", false)]
    #[case("%", false)]
    #[case("-1GB", false)]
    fn only_a_number_with_a_unit_is_a_usable_memory_limit(#[case] value: &str, #[case] usable: bool) {
        assert_eq!(valid_memory_limit(value.trim()), usable);
    }

    #[test]
    fn a_source_without_a_geometry_column_is_reported_as_such() {
        crate::ensure_spatial();
        let dir = std::env::temp_dir().join(format!("duckxy-nogeom-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attrs.csv");
        fs::write(&path, "id,name\n1,alpha\n2,beta\n").unwrap();
        let src = path.to_str().unwrap().to_string();

        let err = run(
            &src,
            crate::url::DEFAULT_ENCODING,
            Format::GeoJson,
            |c| crate::sql::build_sql(&src, crate::url::DEFAULT_ENCODING, &[], Format::GeoJson, c),
            || panic!("a source with no geometry must not reach the ready signal"),
            &mut |_| true,
        )
        .unwrap_err();
        assert!(err.downcast_ref::<NoGeometry>().is_some(), "{err:#}");
    }

    fn marker_exists(conn: &Connection) -> bool {
        conn.prepare("SELECT 1 FROM duckdb_tables() WHERE table_name = 'marker'")
            .and_then(|mut s| s.query([]).and_then(|mut r| r.next().map(|row| row.is_some())))
            .unwrap_or(false)
    }

    #[rstest::rstest]
    #[case("FATAL Error: Failed: database has been invalidated because of a previous fatal error.", true)]
    #[case("FATAL Error: Failed to create checkpoint because of error: Checkpoint aborted before truncate", true)]
    #[case("INTERNAL Error: Attempted to dereference unique_ptr that is NULL", true)]
    #[case("Invalid Input Error: TopologyException: side location conflict at 0.5 0.5", false)]
    #[case("Invalid Input Error: AssertionFailedException: Should never reach here", false)]
    #[case("Out of Memory Error: failed to allocate data of size 128.0 MiB", false)]
    #[case("Conversion Error: Could not convert string to double", false)]
    fn only_an_invalidated_database_is_detected(#[case] message: &str, #[case] expected: bool) {
        assert_eq!(database_is_invalidated(&anyhow::anyhow!(message.to_string())), expected);
    }

    #[test]
    fn an_invalidated_database_is_replaced_on_the_next_query() {
        crate::ensure_spatial();
        with_connection(|c| c.execute_batch("CREATE TABLE marker (x INTEGER)").context("marker")).unwrap();
        assert!(
            with_connection(|c| Ok(marker_exists(c))).unwrap(),
            "marker should be visible on the cached connection"
        );

        let err = with_connection(|_| -> Result<()> {
            Err(anyhow::anyhow!(
                "FATAL Error: Failed: database has been invalidated because of a previous fatal error."
            ))
        })
        .unwrap_err();
        assert!(database_is_invalidated(&err));

        assert!(!with_connection(|c| Ok(marker_exists(c))).unwrap(), "the invalidated database was reused");
    }

    #[test]
    fn an_ordinary_error_keeps_the_connection() {
        crate::ensure_spatial();
        with_connection(|c| c.execute_batch("CREATE TABLE keeper (x INTEGER)").context("keeper")).unwrap();
        let err = with_connection(|_| -> Result<()> { Err(anyhow::anyhow!("Invalid Input Error: TopologyException")) })
            .unwrap_err();
        assert!(!database_is_invalidated(&err));
        let kept = with_connection(|c| {
            Ok(c.prepare("SELECT 1 FROM duckdb_tables() WHERE table_name = 'keeper'")
                .and_then(|mut s| s.query([]).and_then(|mut r| r.next().map(|row| row.is_some())))
                .unwrap_or(false))
        })
        .unwrap();
        assert!(kept, "an ordinary error must not discard the connection");
    }

    #[test]
    fn renders_every_feature_as_valid_json() {
        let body = collect(&fixture("two", TWO), "geojson");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{body} ({e})"));
        let names: Vec<&str> =
            v["features"].as_array().unwrap().iter().map(|f| f["properties"]["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["alpha", "beta"]);
    }

    #[test]
    fn a_null_geometry_survives_as_json_null() {
        let body = collect(&fixture("null", WITH_NULL), "geojson");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{body} ({e})"));
        let features = v["features"].as_array().unwrap();
        assert_eq!(features.len(), 2, "a row was dropped: {body}");
        let null_one = features.iter().find(|f| f["properties"]["name"] == "nogeom").unwrap();
        assert!(null_one["geometry"].is_null(), "{null_one}");
    }

    #[test]
    fn an_empty_source_still_produces_valid_output() {
        let empty = r#"{"type":"FeatureCollection","features":[]}"#;
        let body = collect(&fixture("empty", empty), "geojson");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{body} ({e})"));
        assert!(v["features"].as_array().unwrap().is_empty(), "{body}");
    }

    fn many_features(n: usize) -> String {
        let features: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    r#"{{"type":"Feature","properties":{{"name":"f{i}","pad":"{}"}},"geometry":{{"type":"Point","coordinates":[1,2]}}}}"#,
                    "x".repeat(200)
                )
            })
            .collect();
        format!(r#"{{"type":"FeatureCollection","features":[{}]}}"#, features.join(","))
    }

    #[test]
    fn a_sink_returning_false_stops_the_scan() {
        crate::ensure_spatial();
        let src = fixture("stop", &many_features(2000));
        let f = Format::GeoJson;
        let mut stopped_after = 0;
        run(
            &src,
            crate::url::DEFAULT_ENCODING,
            f,
            |c| crate::sql::build_sql(&src, crate::url::DEFAULT_ENCODING, &[], f, c),
            || {},
            &mut |_| {
                stopped_after += 1;
                false
            },
        )
        .unwrap();
        assert_eq!(stopped_after, 1, "scan continued after the sink asked it to stop");

        let mut chunks = 0;
        run(
            &src,
            crate::url::DEFAULT_ENCODING,
            f,
            |c| crate::sql::build_sql(&src, crate::url::DEFAULT_ENCODING, &[], f, c),
            || {},
            &mut |_| {
                chunks += 1;
                true
            },
        )
        .unwrap();
        assert!(chunks > 1, "fixture too small to exercise chunking: {chunks} chunk");
    }

    #[test]
    fn a_source_column_named_like_the_geometry_alias_does_not_hijack_it() {
        let body = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"name":"a","geometry_json":"KEEPME"},
             "geometry":{"type":"Point","coordinates":[1,2]}}]}"#;
        let out = collect(&fixture("collide", body), "geojson");
        let v: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("collision produced invalid json: {out} ({e})"));
        let f = &v["features"][0];
        assert_eq!(f["geometry"]["type"], "Point", "geometry slot hijacked: {f}");
        assert_eq!(f["properties"]["geometry_json"], "KEEPME", "source attribute lost: {f}");
    }

    #[test]
    fn describe_returns_every_column_with_its_type() {
        crate::ensure_spatial();
        let src = fixture("describe", TWO);
        let columns = with_connection(|conn| describe(conn, &src, crate::url::DEFAULT_ENCODING)).unwrap();
        let names: Vec<&str> = columns.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"name"), "{names:?}");
        assert_eq!(geometry_of(&columns), Some("geom"));
    }

    #[test]
    fn geometry_of_returns_none_without_a_geometry_column() {
        let columns = vec![("id".to_string(), "BIGINT".to_string())];
        assert_eq!(geometry_of(&columns), None);
    }
}
