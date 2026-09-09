use std::path::{Path, PathBuf};

const EXTENSIONS: &[&str] = &["geojson", "json", "fgb", "gpkg", "kml", "shp", "shp.zip", "zip"];

#[derive(Debug, PartialEq)]
pub enum ResolveError {
    NotFound(String),
    InvalidRoot(PathBuf),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NotFound(n) => write!(f, "dataset not found: {n}"),
            ResolveError::InvalidRoot(p) => write!(f, "data root is not a directory: {}", p.display()),
        }
    }
}

impl std::error::Error for ResolveError {}

#[derive(Debug, Clone)]
pub struct DatasetRoot {
    root: PathBuf,
}

impl DatasetRoot {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve(&self, name: &str) -> Result<String, ResolveError> {
        if !self.root.is_dir() {
            return Err(ResolveError::InvalidRoot(self.root.clone()));
        }
        let canonical_root = self.root.canonicalize().map_err(|_| ResolveError::InvalidRoot(self.root.clone()))?;

        for ext in EXTENSIONS {
            let candidate = canonical_root.join(format!("{name}.{ext}"));
            if candidate.is_file() {
                return Ok(gdal_path(&candidate, ext));
            }
        }
        Err(ResolveError::NotFound(name.to_string()))
    }
}

fn gdal_path(file: &Path, ext: &str) -> String {
    let abs = file.display().to_string();
    match ext {
        "shp.zip" | "zip" => format!("/vsizip/{abs}"),
        _ => abs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;

    fn tempdir() -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("duckxy-test-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_a_plain_file() {
        let dir = tempdir();
        fs::write(dir.join("cities.geojson"), b"x").unwrap();
        let resolved = DatasetRoot::new(dir).resolve("cities").unwrap();
        assert!(resolved.ends_with("cities.geojson"));
        assert!(!resolved.starts_with("/vsizip/"));
    }

    #[test]
    fn extension_priority_prefers_geojson() {
        let dir = tempdir();
        fs::write(dir.join("ds.gpkg"), b"x").unwrap();
        fs::write(dir.join("ds.geojson"), b"x").unwrap();
        let resolved = DatasetRoot::new(dir).resolve("ds").unwrap();
        assert!(resolved.ends_with("ds.geojson"), "{resolved}");
    }

    #[rstest]
    #[case("ds.parquet")]
    #[case("ds.csv")]
    fn formats_st_read_cannot_open_are_not_advertised(#[case] filename: &str) {
        let dir = tempdir();
        fs::write(dir.join(filename), b"x").unwrap();
        assert!(matches!(DatasetRoot::new(dir).resolve("ds"), Err(ResolveError::NotFound(_))));
    }

    #[test]
    fn missing_dataset_returns_not_found() {
        let dir = tempdir();
        let root = DatasetRoot::new(dir);
        assert!(matches!(root.resolve("nope"), Err(ResolveError::NotFound(_))));
    }

    #[test]
    fn invalid_root_returns_invalid_root() {
        let root = DatasetRoot::new(PathBuf::from("/nonexistent/duckxy-root-xyz"));
        assert!(matches!(root.resolve("x"), Err(ResolveError::InvalidRoot(_))));
    }

    use duckdb::Connection;
    use std::io::Write;

    const ARCHIVE_POINTS: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"alpha"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;

    fn zip_containing(dir: &std::path::Path, archive: &str, member: &str) {
        let file = std::fs::File::create(dir.join(archive)).unwrap();
        let mut w = zip::ZipWriter::new(file);
        w.start_file(member, zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(ARCHIVE_POINTS.as_bytes()).unwrap();
        w.finish().unwrap();
    }

    fn archive_tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("duckxy-arch-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn features_at(path: &str) -> Result<i64, String> {
        crate::ensure_spatial();
        let c = Connection::open_in_memory().map_err(|e| e.to_string())?;
        c.execute_batch("LOAD spatial;").map_err(|e| e.to_string())?;
        let mut stmt = c.prepare(&format!("SELECT count(*) FROM ST_Read('{path}')")).map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        rows.next().map_err(|e| e.to_string())?.unwrap().get(0).map_err(|e| e.to_string())
    }

    #[rstest]
    #[case("cities.zip", "somethingelse.geojson")]
    #[case("cities.shp.zip", "somethingelse.geojson")]
    fn gdal_opens_what_the_resolver_returns(#[case] archive: &str, #[case] member: &str) {
        let dir = archive_tempdir(archive);
        zip_containing(&dir, archive, member);

        let resolved = DatasetRoot::new(dir).resolve("cities").expect("resolve");
        assert!(resolved.starts_with("/vsizip/"), "{resolved}");

        match features_at(&resolved) {
            Ok(n) => assert_eq!(n, 1, "wrong feature count via {resolved}"),
            Err(e) => panic!("GDAL could not open {resolved}: {e}"),
        }
    }

    #[test]
    fn a_plain_file_is_returned_unwrapped() {
        let dir = archive_tempdir("plain");
        std::fs::write(dir.join("cities.geojson"), ARCHIVE_POINTS).unwrap();
        let resolved = DatasetRoot::new(dir).resolve("cities").expect("resolve");
        assert!(!resolved.starts_with("/vsizip/"), "{resolved}");
        assert_eq!(features_at(&resolved).expect("GDAL open"), 1);
    }
}
