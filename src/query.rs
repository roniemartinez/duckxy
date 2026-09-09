use crate::formats::Format;
use anyhow::{Context, Result};
use duckdb::Connection;

const CHUNK_BYTES: usize = 64 * 1024;

pub fn install_extensions() -> Result<()> {
    let conn = Connection::open_in_memory().context("open duckdb")?;
    conn.execute_batch("INSTALL spatial; LOAD spatial;").context("install the spatial extension")
}

pub fn run(sql: &str, format: Format, on_ready: impl FnOnce(), sink: &mut dyn FnMut(String) -> bool) -> Result<()> {
    let conn = Connection::open_in_memory().context("open duckdb")?;
    conn.execute_batch("LOAD spatial;").context("load the spatial extension")?;

    let mut stmt = conn.prepare(sql).context("prepare query")?;
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
        run(&crate::handler::build_sql(source, f), f, || {}, &mut |chunk| {
            out.push_str(&chunk);
            true
        })
        .unwrap();
        out.push_str(f.footer());
        out
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
        // big enough that the in-loop sink fires before the tail flush
        crate::ensure_spatial();
        let src = fixture("stop", &many_features(2000));
        let f = Format::GeoJson;
        let sql = crate::handler::build_sql(&src, f);

        let mut stopped_after = 0;
        run(&sql, f, || {}, &mut |_| {
            stopped_after += 1;
            false
        })
        .unwrap();
        assert_eq!(stopped_after, 1, "scan continued after the sink asked it to stop");

        let mut chunks = 0;
        run(&sql, f, || {}, &mut |_| {
            chunks += 1;
            true
        })
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
}
