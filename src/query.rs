use anyhow::{Context, Result};
use duckdb::Connection;
use sea_query::{Alias, Expr, Func, FunctionCall, PostgresQueryBuilder, Query};
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
    static ONCE: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| install_once().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow::anyhow!(e.clone()))
        .copied()
}

fn install_once() -> Result<()> {
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

pub struct Resolved {
    pub raw: String,
    pub source: String,
    pub encoding: String,
}

pub struct Described {
    pub raw: String,
    pub source: String,
    pub encoding: String,
    pub columns: Vec<(String, String)>,
    pub crs: Option<String>,
}

fn describe_nested(conn: &Connection, nested: Vec<Resolved>) -> Result<Vec<Described>> {
    let mut described = Vec::with_capacity(nested.len());
    for held in nested {
        let columns = describe(conn, &held.source, &held.encoding)?;
        geometry_of(&columns).ok_or(NoGeometry)?;
        let crs = read_crs(conn, &held.source);
        described.push(Described { raw: held.raw, source: held.source, encoding: held.encoding, columns, crs });
    }
    Ok(described)
}

fn read_crs(conn: &Connection, source: &str) -> Option<String> {
    source_crs(conn, source).unwrap_or_else(|e| {
        tracing::warn!(error = ?e, "could not read the source crs, serving it unprojected");
        None
    })
}

struct TempFile {
    dir: std::path::PathBuf,
    file: std::path::PathBuf,
}

impl TempFile {
    fn new(extension: &str) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("duckxy-{}-{:x}", std::process::id(), nonce()));
        let mut building = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut building, 0o700);
        building.create(&dir).with_context(|| format!("make a private directory at {}", dir.display()))?;
        let file = dir.join(format!("out.{extension}"));
        Ok(TempFile { dir, file })
    }
}

fn nonce() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut held = std::collections::hash_map::RandomState::new().build_hasher();
    held.write_u64(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    held.finish()
}

impl Drop for TempFile {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = ?e, dir = %self.dir.display(), "could not remove the temporary response"),
        }
    }
}

pub struct Target<'a> {
    pub driver: crate::formats::Driver,
    pub layer: &'a str,
}

fn copy_statement(inner: &str, file: &std::path::Path, target: &Target<'_>) -> String {
    let quoted = file.display().to_string().replace('\'', "''");
    let layer = target.layer.replace('\'', "''");
    let mut copy = format!(
        "COPY ({inner}) TO '{quoted}' WITH (FORMAT GDAL, DRIVER '{}', LAYER_NAME '{layer}'",
        target.driver.name
    );
    if !target.driver.options.is_empty() {
        let held =
            target.driver.options.iter().map(|o| format!("'{}'", o.replace('\'', "''"))).collect::<Vec<_>>().join(", ");
        copy.push_str(&format!(", LAYER_CREATION_OPTIONS ({held})"));
    }
    copy.push(')');
    copy
}

const DEFAULT_EXPORT_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

fn export_limit() -> u64 {
    let Ok(value) = std::env::var("DUCKXY_MAX_EXPORT_BYTES") else {
        return DEFAULT_EXPORT_LIMIT;
    };
    parse_bytes(value.trim()).unwrap_or_else(|| {
        tracing::warn!(value, "ignoring an unusable export limit, keeping the default");
        DEFAULT_EXPORT_LIMIT
    })
}

fn parse_bytes(value: &str) -> Option<u64> {
    let digits = value.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let scale = match value[digits.len()..].to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "KB" | "KIB" => 1024,
        "MB" | "MIB" => 1024 * 1024,
        "GB" | "GIB" => 1024 * 1024 * 1024,
        _ => return None,
    };
    digits.parse::<u64>().ok().filter(|n| *n > 0)?.checked_mul(scale)
}

fn copy_bounded(conn: &Connection, statement: &str, file: &std::path::Path, limit: u64) -> Result<()> {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let interrupt = conn.interrupt_handle();
    let watched = file.to_path_buf();
    let watching = std::thread::spawn(move || {
        while stopped.recv_timeout(WATCH_EVERY) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            if std::fs::metadata(&watched).is_ok_and(|held| held.len() > limit) {
                interrupt.interrupt();
                return true;
            }
        }
        false
    });
    let written = conn.execute_batch(statement).context("write the response with gdal");
    drop(stop);
    let stopped = watching.join().unwrap_or_else(|_| {
        tracing::warn!("the export watcher panicked, falling back to the size of the finished file");
        false
    });
    if !stopped {
        written?;
        let size = std::fs::metadata(file).map(|held| held.len()).unwrap_or(0);
        if size <= limit {
            return Ok(());
        }
    }
    Err(crate::Fault::new(
        axum::http::StatusCode::PAYLOAD_TOO_LARGE,
        format!("the response passed the {limit} byte export limit; narrow it with a filter"),
    )
    .into())
}

pub fn write_with_driver(
    source: &str,
    encoding: &str,
    target: Target<'_>,
    nested: Vec<Resolved>,
    sql_for: impl FnOnce(&[(String, String)], Option<&str>, &[Described]) -> Result<String>,
    on_ready: impl FnOnce(),
    sink: &mut dyn FnMut(Vec<u8>) -> bool,
) -> Result<()> {
    let held = TempFile::new(target.driver.file)?;
    let file = &held.file;
    with_connection(|conn| {
        let columns = describe(conn, source, encoding)?;
        geometry_of(&columns).ok_or(NoGeometry)?;
        let crs = read_crs(conn, source);
        let described = describe_nested(conn, nested)?;
        let inner = sql_for(&columns, crs.as_deref(), &described)?;
        copy_bounded(conn, &copy_statement(&inner, file, &target), file, export_limit())
    })?;

    if file.is_dir() {
        anyhow::bail!(
            "the {} driver wrote a dataset of several files, which cannot be served alone",
            target.driver.name
        );
    }
    let mut reading = std::fs::File::open(file).context("open the written response")?;
    on_ready();
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        let read = std::io::Read::read(&mut reading, &mut buf).context("read the written response")?;
        if read == 0 || !sink(buf[..read].to_vec()) {
            return Ok(());
        }
    }
}

pub fn run(
    source: &str,
    encoding: &str,
    separator: &str,
    nested: Vec<Resolved>,
    sql_for: impl FnOnce(&[(String, String)], Option<&str>, &[Described]) -> Result<String>,
    on_ready: impl FnOnce(),
    sink: &mut dyn FnMut(String) -> bool,
) -> Result<()> {
    with_connection(|conn| {
        let columns = describe(conn, source, encoding)?;
        geometry_of(&columns).ok_or(NoGeometry)?;
        let crs = read_crs(conn, source);
        let described = describe_nested(conn, nested)?;
        let sql = sql_for(&columns, crs.as_deref(), &described)?;

        let mut stmt = conn.prepare(&sql).context("prepare query")?;
        let mut rows = stmt.query([]).context("run query")?;
        on_ready();

        let mut buf = String::with_capacity(CHUNK_BYTES * 2);
        let mut first = true;

        while let Some(row) = rows.next().context("read row")? {
            let Some(text) = row.get::<_, Option<String>>(0).context("read row")? else { continue };
            if !first {
                buf.push_str(separator);
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

fn source_crs(conn: &Connection, source: &str) -> Result<Option<String>> {
    let fields = Query::select()
        .expr_as(Expr::cust("unnest(layers).geometry_fields[1].crs"), Alias::new("crs"))
        .from_function(Func::cust("ST_Read_Meta").arg(source), "meta")
        .take();
    let authority = Expr::cust_with_exprs(
        "$1 || ':' || $2",
        [
            Func::cust("nullif").arg(Expr::cust("crs.auth_name")).arg("").into(),
            Func::cust("nullif").arg(Expr::cust("crs.auth_code")).arg("").into(),
        ],
    );
    let sql = Query::select()
        .expr(Func::coalesce([authority, Func::cust("nullif").arg(Expr::cust("crs.wkt")).arg("").into()]))
        .from_subquery(fields, "meta_crs")
        .limit(1)
        .to_string(PostgresQueryBuilder);

    let mut stmt = conn.prepare(&sql).context("read source crs")?;
    let mut rows = stmt.query([]).context("read source crs")?;
    match rows.next().context("read source crs")? {
        Some(row) => row.get::<_, Option<String>>(0).context("read source crs"),
        None => Ok(None),
    }
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
    use crate::formats::Output;

    fn geojson_output() -> std::sync::Arc<dyn Output> {
        std::sync::Arc::new(crate::formats::GeoJson)
    }

    fn parsed_for(ext: &str) -> crate::url::ParsedUrl {
        crate::url::parse(&format!("/@dataset:x.{ext}"), &crate::grammar::Grammar::core()).unwrap()
    }

    const PARCELS: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"inside"},"geometry":{"type":"Point","coordinates":[1,1]}},
        {"type":"Feature","properties":{"name":"outside"},"geometry":{"type":"Point","coordinates":[9,9]}},
        {"type":"Feature","properties":{"name":"edge"},"geometry":{"type":"Point","coordinates":[2,2]}}]}"#;

    const ZONES: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"flood"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[0,2],[2,2],[2,0],[0,0]]]}},
        {"type":"Feature","properties":{"name":"dry"},"geometry":{"type":"Polygon","coordinates":[[[20,20],[20,21],[21,21],[21,20],[20,20]]]}}]}"#;

    fn nested_names(url: &str, outer: &str, inner: &str) -> Vec<String> {
        crate::ensure_spatial();
        let g = crate::grammar::Grammar::core();
        let parsed = crate::url::parse(url, &g).unwrap();
        let f = parsed.output.clone();
        let resolved: Vec<Resolved> = parsed
            .nested
            .iter()
            .map(|held| Resolved { raw: held.raw.clone(), source: inner.to_string(), encoding: held.encoding.clone() })
            .collect();
        let mut out = String::from(f.header());
        run(
            outer,
            crate::url::DEFAULT_ENCODING,
            f.separator(),
            resolved,
            |c, crs, nested| crate::sql::plan(&g, &parsed, outer, c, crs, nested, backend()),
            || {},
            &mut |chunk| {
                out.push_str(&chunk);
                true
            },
        )
        .unwrap();
        out.push_str(f.footer());
        let held: serde_json::Value = serde_json::from_str(&out).expect(&out);
        held["features"]
            .as_array()
            .expect("features")
            .iter()
            .map(|feature| feature["properties"]["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    #[test]
    fn a_nested_filter_that_builds_a_side_table_still_runs() {
        crate::ensure_spatial();
        let mut g = crate::grammar::Grammar::core();
        g.register_filter(crate::filters::filter("sided", "sd", &[&[crate::grammar::Param::Value]], |ctx, _| {
            let held = ctx.cte_once("probe", |from| {
                sea_query::Query::select()
                    .expr_as(sea_query::Expr::cust("1"), sea_query::Alias::new("one"))
                    .from(from.clone())
                    .take()
            });
            let reading = sea_query::Query::select().expr(sea_query::Expr::cust("MAX(one)")).from(held).take();
            let sub =
                sea_query::SimpleExpr::SubQuery(None, Box::new(sea_query::SubQueryStatement::SelectStatement(reading)));
            Ok(sea_query::Expr::cust_with_exprs("$1 = 1", [sub]))
        }));
        let outer = fixture("parcels", PARCELS);
        let inner = fixture("zones", ZONES);
        let url = "/@dataset:parcels,ix:(@dataset:zones,sided:x).geojson";
        let parsed = crate::url::parse(url, &g).unwrap();
        let resolved: Vec<Resolved> = parsed
            .nested
            .iter()
            .map(|held| Resolved { raw: held.raw.clone(), source: inner.clone(), encoding: held.encoding.clone() })
            .collect();
        let mut out = String::new();
        let sent = run(
            &outer,
            crate::url::DEFAULT_ENCODING,
            ",",
            resolved,
            |c, crs, nested| crate::sql::plan(&g, &parsed, &outer, c, crs, nested, backend()),
            || {},
            &mut |chunk| {
                out.push_str(&chunk);
                true
            },
        );
        sent.expect("a nested filter that builds a side table produced unrunnable sql");
    }

    #[test]
    fn a_nested_source_filters_by_the_other_datasets_geometry() {
        let outer = fixture("parcels", PARCELS);
        let inner = fixture("zones", ZONES);
        let names = nested_names("/@dataset:parcels,ix:(@dataset:zones).geojson", &outer, &inner);
        assert_eq!(names, vec!["inside", "edge"], "the nested geometry did not narrow the outer rows");
    }

    #[test]
    fn a_nested_source_applies_its_own_filters_first() {
        let outer = fixture("parcels", PARCELS);
        let inner = fixture("zones", ZONES);
        let names = nested_names("/@dataset:parcels,ix:(@dataset:zones,prop:name:dry).geojson", &outer, &inner);
        assert!(names.is_empty(), "the inner filter was ignored: {names:?}");
    }

    #[test]
    fn a_copy_statement_quotes_the_path_and_lists_every_layer_option() {
        let driver = crate::formats::Driver { name: "KML", file: "kml", options: &["a=1", "b=2"] };
        let target = Target { driver, layer: "it's" };
        let held = copy_statement("SELECT 1", std::path::Path::new("/tmp/it's.kml"), &target);
        assert_eq!(
            held,
            "COPY (SELECT 1) TO '/tmp/it''s.kml' WITH (FORMAT GDAL, DRIVER 'KML', LAYER_NAME 'it''s', \
             LAYER_CREATION_OPTIONS ('a=1', 'b=2'))"
        );
    }

    #[test]
    fn a_copy_statement_without_layer_options_says_nothing_about_them() {
        let driver = crate::formats::Driver { name: "FlatGeobuf", file: "fgb", options: &[] };
        let target = Target { driver, layer: "x" };
        let held = copy_statement("SELECT 1", std::path::Path::new("/tmp/x.fgb"), &target);
        assert_eq!(held, "COPY (SELECT 1) TO '/tmp/x.fgb' WITH (FORMAT GDAL, DRIVER 'FlatGeobuf', LAYER_NAME 'x')");
    }

    #[test]
    fn a_temporary_response_lives_in_a_private_directory() {
        let held = TempFile::new("kml").unwrap();
        let shared = std::env::temp_dir();
        assert_eq!(held.file.parent(), Some(held.dir.as_path()));
        assert_ne!(held.file.parent(), Some(shared.as_path()), "anyone on the host could plant this path");
        #[cfg(unix)]
        {
            let allowed = fs::metadata(&held.dir).unwrap().permissions();
            let mode = std::os::unix::fs::PermissionsExt::mode(&allowed);
            assert_eq!(mode & 0o777, 0o700, "the temporary directory is not private: {mode:o}");
        }
        let dir = held.dir.clone();
        drop(held);
        assert!(!dir.exists(), "the temporary directory outlived the response");
    }

    #[test]
    fn two_temporary_responses_do_not_share_a_directory() {
        let a = TempFile::new("kml").unwrap();
        let b = TempFile::new("kml").unwrap();
        assert_ne!(a.dir, b.dir);
        assert!(a.dir.exists() && b.dir.exists());
    }

    #[rstest::rstest]
    #[case("512", 512)]
    #[case("64KB", 64 * 1024)]
    #[case("2mb", 2 * 1024 * 1024)]
    #[case("1GiB", 1024 * 1024 * 1024)]
    fn an_export_limit_reads_its_unit(#[case] value: &str, #[case] bytes: u64) {
        assert_eq!(parse_bytes(value), Some(bytes));
    }

    #[rstest::rstest]
    #[case("")]
    #[case("0")]
    #[case("-1")]
    #[case("2PB")]
    #[case("banana")]
    #[case("1.5GB")]
    fn an_unusable_export_limit_is_refused(#[case] value: &str) {
        assert_eq!(parse_bytes(value), None);
    }

    #[test]
    fn an_export_past_the_limit_is_refused_and_the_connection_survives() {
        crate::ensure_spatial();
        let held = TempFile::new("fgb").unwrap();
        let big = format!(
            "COPY (SELECT ST_Point(i, i) AS geom FROM range(4000000) t(i)) TO '{}' \
             WITH (FORMAT GDAL, DRIVER 'FlatGeobuf', LAYER_NAME 'big', LAYER_CREATION_OPTIONS ('SPATIAL_INDEX=NO'))",
            held.file.display()
        );
        let err = with_connection(|conn| copy_bounded(conn, &big, &held.file, 256 * 1024)).unwrap_err();
        let fault = err.downcast_ref::<crate::Fault>().unwrap_or_else(|| panic!("{err:#}"));
        assert_eq!(fault.status, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
        assert!(fault.message.contains("narrow it"), "{}", fault.message);
        let written = fs::metadata(&held.file).map(|held| held.len()).unwrap_or(0);
        assert!(written < 64 * 1024 * 1024, "the write ran on past the limit: {written} bytes");

        let after = TempFile::new("fgb").unwrap();
        let small = format!(
            "COPY (SELECT ST_Point(1, 2) AS geom) TO '{}' WITH (FORMAT GDAL, DRIVER 'FlatGeobuf', LAYER_NAME 'small')",
            after.file.display()
        );
        with_connection(|conn| copy_bounded(conn, &small, &after.file, DEFAULT_EXPORT_LIMIT))
            .expect("the connection did not survive the interrupt");
        assert!(after.file.exists(), "the export after the refusal wrote nothing");
    }

    #[test]
    fn installing_the_extension_twice_is_harmless() {
        install_extensions().unwrap();
        install_extensions().unwrap();
    }

    fn backend() -> std::sync::Arc<crate::backend::Backend> {
        std::sync::Arc::new(crate::backend::Backend::default())
    }
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
        let g = crate::grammar::Grammar::core();
        let parsed = crate::url::parse(&format!("/@dataset:x.{ext}"), &g).unwrap();
        let f = parsed.output.clone();
        let mut out = String::from(f.header());
        run(
            source,
            crate::url::DEFAULT_ENCODING,
            f.separator(),
            Vec::new(),
            |c, crs, nested| {
                crate::sql::plan(&crate::grammar::Grammar::core(), &parsed, source, c, crs, nested, backend())
            },
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
        let f = geojson_output();
        let mut out = String::from(f.header());
        run(
            &src,
            crate::url::DEFAULT_ENCODING,
            f.separator(),
            Vec::new(),
            |c, crs, nested| {
                crate::sql::plan(
                    &crate::grammar::Grammar::core(),
                    &parsed_for("geojson"),
                    &src,
                    c,
                    crs,
                    nested,
                    backend(),
                )
            },
            || {},
            &mut |chunk| {
                out.push_str(&chunk);
                true
            },
        )
        .unwrap();
        out.push_str(f.footer());

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
            geojson_output().separator(),
            Vec::new(),
            |c, crs, nested| {
                crate::sql::plan(
                    &crate::grammar::Grammar::core(),
                    &parsed_for("geojson"),
                    &src,
                    c,
                    crs,
                    nested,
                    backend(),
                )
            },
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
        let f = geojson_output();
        let mut stopped_after = 0;
        run(
            &src,
            crate::url::DEFAULT_ENCODING,
            f.separator(),
            Vec::new(),
            |c, crs, nested| {
                crate::sql::plan(
                    &crate::grammar::Grammar::core(),
                    &parsed_for("geojson"),
                    &src,
                    c,
                    crs,
                    nested,
                    backend(),
                )
            },
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
            f.separator(),
            Vec::new(),
            |c, crs, nested| {
                crate::sql::plan(
                    &crate::grammar::Grammar::core(),
                    &parsed_for("geojson"),
                    &src,
                    c,
                    crs,
                    nested,
                    backend(),
                )
            },
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

    fn shapefile(tag: &str, srs: Option<&str>) -> String {
        crate::ensure_spatial();
        let dir = std::env::temp_dir().join(format!("duckxy-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let shp = dir.join("f.shp");

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("LOAD spatial;").unwrap();
        let projection = srs.map(|s| format!(", SRS '{s}'")).unwrap_or_default();
        conn.execute_batch(&format!(
            "COPY (SELECT ST_Point(500000, 5761038.212466577) AS geom, 1 AS id) TO '{}' \
             WITH (FORMAT GDAL, DRIVER 'ESRI Shapefile'{projection})",
            shp.display()
        ))
        .unwrap();
        if srs.is_none() {
            let _ = fs::remove_file(dir.join("f.prj"));
        }
        shp.to_str().unwrap().to_string()
    }

    #[rstest::rstest]
    #[case("projected", Some("EPSG:25832"), 9.0, 52.0)]
    #[case("nocrs", None, 500000.0, 5761038.212466577)]
    fn a_source_reaches_geojson_in_wgs84_only_when_its_crs_is_known(
        #[case] tag: &str,
        #[case] srs: Option<&str>,
        #[case] lon: f64,
        #[case] lat: f64,
    ) {
        let body = collect(&shapefile(tag, srs), "geojson");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{body} ({e})"));
        let c = &v["features"][0]["geometry"]["coordinates"];
        assert!((c[0].as_f64().unwrap() - lon).abs() < 1e-6, "x is {c}, expected {lon}");
        assert!((c[1].as_f64().unwrap() - lat).abs() < 1e-6, "y is {c}, expected {lat}");
    }

    #[test]
    fn a_crs_without_an_authority_code_is_still_reprojected() {
        let src = shapefile("customprj", None);
        let prj = std::path::Path::new(&src).with_extension("prj");
        fs::write(
            &prj,
            "PROJCS[\"custom\",GEOGCS[\"WGS 84\",DATUM[\"WGS_1984\",SPHEROID[\"WGS 84\",6378137,298.257223563]],\
             PRIMEM[\"Greenwich\",0],UNIT[\"degree\",0.0174532925199433]],PROJECTION[\"Transverse_Mercator\"],\
             PARAMETER[\"latitude_of_origin\",0],PARAMETER[\"central_meridian\",9],PARAMETER[\"scale_factor\",0.9996],\
             PARAMETER[\"false_easting\",500000],PARAMETER[\"false_northing\",0],UNIT[\"metre\",1]]",
        )
        .unwrap();

        let body = collect(&src, "geojson");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{body} ({e})"));
        let c = &v["features"][0]["geometry"]["coordinates"];
        assert!((c[0].as_f64().unwrap() - 9.0).abs() < 1e-6, "not reprojected, x is {c}");
        assert!((c[1].as_f64().unwrap() - 52.0).abs() < 1e-6, "not reprojected, y is {c}");
    }
}
