use std::path::{Path, PathBuf};

const MAX_CANDIDATES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    GeoJson,
    Gpkg,
    Fgb,
    Kml,
    Gml,
    Shp,
    Gdb,
    Json,
}

impl Source {
    const ALL: [Source; 8] =
        [Source::GeoJson, Source::Gpkg, Source::Fgb, Source::Kml, Source::Gml, Source::Shp, Source::Gdb, Source::Json];

    fn extension(self) -> &'static str {
        match self {
            Source::GeoJson => "geojson",
            Source::Gpkg => "gpkg",
            Source::Fgb => "fgb",
            Source::Kml => "kml",
            Source::Gml => "gml",
            Source::Shp => "shp",
            Source::Gdb => "gdb",
            Source::Json => "json",
        }
    }

    fn is_directory(self) -> bool {
        matches!(self, Source::Gdb)
    }

    fn of(path: &str) -> Option<Source> {
        let lower = path.to_ascii_lowercase();
        Source::ALL.into_iter().find(|s| lower.strip_suffix(s.extension()).is_some_and(|rest| rest.ends_with('.')))
    }
}

enum Candidate {
    Plain(Source),
    Zipped(Source),
    Archive,
}

impl Candidate {
    fn all() -> impl Iterator<Item = Candidate> {
        Source::ALL
            .into_iter()
            .map(Candidate::Plain)
            .chain(Source::ALL.into_iter().map(Candidate::Zipped))
            .chain(std::iter::once(Candidate::Archive))
    }

    fn filename(&self, name: &str) -> String {
        match self {
            Candidate::Plain(s) => format!("{name}.{}", s.extension()),
            Candidate::Zipped(s) => format!("{name}.{}.zip", s.extension()),
            Candidate::Archive => format!("{name}.zip"),
        }
    }

    fn expects_directory(&self) -> bool {
        matches!(self, Candidate::Plain(s) if s.is_directory())
    }

    fn is_archive(&self) -> bool {
        !matches!(self, Candidate::Plain(_))
    }
}

pub fn looks_like_source(segment: &str) -> bool {
    Source::of(segment).is_some()
}

#[derive(Debug, PartialEq)]
pub enum ResolveError {
    NotFound(String),
    InvalidRoot(PathBuf),
    EmptyArchive(String),
    UnreadableArchive(String),
    PathNotFound { path: String, candidates: Vec<String> },
    NotAnArchive(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NotFound(n) => write!(f, "dataset not found: {n}"),
            ResolveError::InvalidRoot(p) => write!(f, "data root is not a directory: {}", p.display()),
            ResolveError::EmptyArchive(n) => write!(f, "{n} contains nothing duckxy can read"),
            ResolveError::UnreadableArchive(n) => write!(f, "{n} could not be read"),
            ResolveError::PathNotFound { path, candidates } => {
                write!(f, "no such path in archive: {path}. available: {}", candidates.join(", "))
            }
            ResolveError::NotAnArchive(n) => write!(f, "{n} is not an archive, so it has no paths to select"),
        }
    }
}

impl std::error::Error for ResolveError {}

#[derive(Debug, Clone)]
pub struct DatasetRoot {
    root: PathBuf,
}

struct Entry {
    name: String,
    is_dir: bool,
}

impl DatasetRoot {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve(&self, name: &str, path: Option<&str>) -> Result<String, ResolveError> {
        let invalid = || ResolveError::InvalidRoot(self.root.clone());
        if !self.root.is_dir() {
            return Err(invalid());
        }
        let root = self.root.canonicalize().map_err(|_| invalid())?;
        let entries = read_entries(&root).ok_or_else(invalid)?;

        for candidate in Candidate::all() {
            let Some(entry) = pick_entry(&entries, &candidate.filename(name)) else {
                continue;
            };
            if entry.is_dir != candidate.expects_directory() {
                continue;
            }
            return gdal_path(&root.join(&entry.name), candidate.is_archive(), path);
        }
        Err(ResolveError::NotFound(name.to_string()))
    }
}

fn read_entries(root: &Path) -> Option<Vec<Entry>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let (Ok(name), Ok(kind)) = (entry.file_name().into_string(), entry.file_type()) else {
            continue;
        };
        let is_dir = match kind.is_symlink() {
            true => std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()),
            false => kind.is_dir(),
        };
        out.push(Entry { name, is_dir });
    }
    Some(out)
}

fn pick_entry<'a>(entries: &'a [Entry], wanted: &str) -> Option<&'a Entry> {
    entries
        .iter()
        .find(|e| e.name == wanted)
        .or_else(|| entries.iter().filter(|e| e.name.eq_ignore_ascii_case(wanted)).min_by_key(|e| &e.name))
}

fn gdal_path(file: &Path, archive: bool, wanted: Option<&str>) -> Result<String, ResolveError> {
    let label = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let abs = file.to_str().ok_or_else(|| ResolveError::NotFound(label.clone()))?;

    if !archive {
        return match wanted {
            Some(_) => Err(ResolveError::NotAnArchive(label)),
            None => Ok(abs.to_string()),
        };
    }

    let paths = archive_paths(file)?;
    let chosen = match wanted {
        Some(want) => pick_member(&paths, want).ok_or_else(|| ResolveError::PathNotFound {
            path: want.to_string(),
            candidates: paths.iter().take(MAX_CANDIDATES).map(|(_, name)| name.clone()).collect(),
        })?,
        None => paths
            .iter()
            .min_by(|a, b| {
                (a.0 as usize)
                    .cmp(&(b.0 as usize))
                    .then_with(|| a.1.matches('/').count().cmp(&b.1.matches('/').count()))
                    .then_with(|| a.1.cmp(&b.1))
            })
            .ok_or(ResolveError::EmptyArchive(label))?,
    };
    Ok(format!("/vsizip/{abs}/{}", chosen.1))
}

fn pick_member<'a>(paths: &'a [(Source, String)], wanted: &str) -> Option<&'a (Source, String)> {
    paths
        .iter()
        .find(|(_, name)| name == wanted)
        .or_else(|| paths.iter().filter(|(_, name)| name.eq_ignore_ascii_case(wanted)).min_by_key(|(_, name)| name))
}

fn archive_paths(file: &Path) -> Result<Vec<(Source, String)>, ResolveError> {
    let label = || file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let handle = std::fs::File::open(file).map_err(|e| {
        tracing::warn!(archive = %file.display(), error = %e, "cannot open archive");
        ResolveError::UnreadableArchive(label())
    })?;
    let mut zip = zip::ZipArchive::new(handle).map_err(|e| {
        tracing::warn!(archive = %file.display(), error = %e, "cannot read archive");
        ResolveError::UnreadableArchive(label())
    })?;
    let mut out: Vec<(Source, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index_raw(i) else { continue };
        let name = entry.name();
        if name.starts_with("__MACOSX/") {
            continue;
        }
        let name = name.trim_end_matches('/');
        if name.rsplit('/').next().is_some_and(|base| base.starts_with("._")) {
            continue;
        }
        let Some((source, found)) = Source::of(name).map(|s| (s, name)).or_else(|| directory_prefix(name)) else {
            continue;
        };
        if seen.insert(found.to_string()) {
            out.push((source, found.to_string()));
        }
    }
    Ok(out)
}

fn directory_prefix(name: &str) -> Option<(Source, &str)> {
    let mut end = 0;
    for part in name.split('/') {
        let prefix = &name[..end + part.len()];
        end += part.len() + 1;
        if let Some(source) = Source::of(prefix).filter(|s| s.is_directory()) {
            return Some((source, prefix));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;

    #[test]
    fn a_plain_file_wins_over_other_extensions_and_is_not_wrapped() {
        let dir = archive_tempdir("plain");
        fs::write(dir.join("ds.gpkg"), b"x").unwrap();
        fs::write(dir.join("ds.geojson"), ARCHIVE_POINTS).unwrap();
        let resolved = DatasetRoot::new(dir).resolve("ds", None).unwrap();
        assert!(resolved.ends_with("ds.geojson"), "{resolved}");
        assert!(!resolved.starts_with("/vsizip/"), "{resolved}");
        assert_eq!(features_at(&resolved).expect("GDAL open"), 1);
    }

    #[rstest]
    #[case("ds.tar.zip", true)]
    #[case("ds.parquet", false)]
    #[case("", false)]
    fn nothing_resolves_to_a_dataset(#[case] filename: &str, #[case] archive: bool) {
        let dir = archive_tempdir(if filename.is_empty() { "none" } else { filename });
        if archive {
            zip_named(&dir, filename, &["thing.geojson"]);
        } else if !filename.is_empty() {
            fs::write(dir.join(filename), b"x").unwrap();
        }
        assert!(matches!(DatasetRoot::new(dir).resolve("ds", None), Err(ResolveError::NotFound(_))));
    }

    #[test]
    fn invalid_root_returns_invalid_root() {
        let root = DatasetRoot::new(PathBuf::from("/nonexistent/duckxy-root-xyz"));
        assert!(matches!(root.resolve("x", None), Err(ResolveError::InvalidRoot(_))));
    }

    use duckdb::Connection;
    use std::io::Write;

    const ARCHIVE_POINTS: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"alpha"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;

    fn archive_tempdir(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tag: String = tag.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let dir = std::env::temp_dir().join(format!("duckxy-arch-{}-{n}-{tag}", std::process::id()));
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

    fn zip_named(dir: &std::path::Path, archive: &str, entries: &[&str]) {
        let file = std::fs::File::create(dir.join(archive)).unwrap();
        let mut w = zip::ZipWriter::new(file);
        for entry in entries {
            if let Some(dir) = entry.strip_suffix('/') {
                w.add_directory(dir, zip::write::SimpleFileOptions::default()).unwrap();
                continue;
            }
            w.start_file(*entry, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(ARCHIVE_POINTS.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    #[rstest]
    #[case("ds.zip", &["thing.geojson"], "thing.geojson")]
    #[case("ds.kml.zip", &["__MACOSX/._thing.geojson", "meta.json", "a/b/c/thing.geojson"], "a/b/c/thing.geojson")]
    #[case("ds.fgb.zip", &["._thing.geojson", "thing.geojson"], "thing.geojson")]
    #[case("ds.gml.zip", &["a/._thing.geojson", "a/thing.geojson"], "a/thing.geojson")]
    fn resolves_and_opens_a_path_inside_an_archive(
        #[case] archive: &str,
        #[case] entries: &[&str],
        #[case] expected: &str,
    ) {
        let dir = archive_tempdir(archive);
        zip_named(&dir, archive, entries);

        let resolved = DatasetRoot::new(dir).resolve("ds", None).expect(archive);
        assert!(resolved.ends_with(&format!("/{expected}")), "{resolved}");
        assert_eq!(features_at(&resolved).expect("GDAL open"), 1);
    }

    #[test]
    fn two_members_of_the_same_kind_are_picked_by_name() {
        let dir = archive_tempdir("tiebreak");
        zip_named(&dir, "ds.zip", &["zebra.geojson", "alpha.geojson", "deep/aaa.geojson"]);
        let resolved = DatasetRoot::new(dir).resolve("ds", None).expect("resolve");
        assert!(resolved.ends_with("/alpha.geojson"), "{resolved}");
    }

    #[test]
    fn an_unreadable_archive_is_not_reported_as_empty() {
        let dir = archive_tempdir("corrupt");
        fs::write(dir.join("ds.zip"), b"this is not a zip file at all").unwrap();
        let err = DatasetRoot::new(dir).resolve("ds", None).unwrap_err();
        assert!(matches!(err, ResolveError::UnreadableArchive(_)), "{err:?}");
    }

    #[test]
    fn an_archive_with_nothing_readable_is_reported() {
        let dir = archive_tempdir("sidecars");
        zip_named(&dir, "ds.zip", &["ds.dbf", "ds.prj", "ds.shx"]);
        assert!(matches!(DatasetRoot::new(dir).resolve("ds", None), Err(ResolveError::EmptyArchive(_))));
    }

    #[rstest]
    #[case(Some("A/B/TWO.GeoJSON"), "a/b/two.geojson")]
    #[case(None, "one.geojson")]
    fn an_explicit_path_overrides_the_priority_order(#[case] want: Option<&str>, #[case] expected: &str) {
        let dir = archive_tempdir(&format!("pick-{}", want.unwrap_or("none").replace('/', "_")));
        zip_named(&dir, "ds.zip", &["one.geojson", "a/b/two.geojson"]);
        let resolved = DatasetRoot::new(dir).resolve("ds", want).expect("resolve");
        assert!(resolved.ends_with(&format!("/{expected}")), "{resolved}");
    }

    #[rstest]
    #[case("a.geojson", "a.geojson")]
    #[case("A.geojson", "A.geojson")]
    #[case("A.GEOJSON", "A.geojson")]
    fn an_exact_archive_member_wins_over_a_case_variant(#[case] want: &str, #[case] expected: &str) {
        let dir = archive_tempdir(&format!("member-{want}"));
        zip_named(&dir, "ds.zip", &["A.geojson", "a.geojson"]);
        let resolved = DatasetRoot::new(dir).resolve("ds", Some(want)).expect(want);
        assert!(resolved.ends_with(&format!("/{expected}")), "{resolved}");
    }

    #[test]
    fn an_unknown_path_lists_what_the_archive_holds() {
        let dir = archive_tempdir("nofile");
        zip_named(&dir, "ds.zip", &["one.geojson"]);
        let err = DatasetRoot::new(dir).resolve("ds", Some("nope.shp")).unwrap_err();
        let ResolveError::PathNotFound { candidates, .. } = &err else { panic!("{err:?}") };
        assert_eq!(candidates, &["one.geojson"]);
    }

    #[test]
    fn selecting_a_path_inside_a_plain_dataset_is_rejected() {
        let dir = archive_tempdir("plainfile");
        std::fs::write(dir.join("ds.geojson"), ARCHIVE_POINTS).unwrap();
        let err = DatasetRoot::new(dir).resolve("ds", Some("x.shp")).unwrap_err();
        assert!(matches!(err, ResolveError::NotAnArchive(_)), "{err:?}");
    }

    #[rstest]
    #[case("explicit", &["ds/ds.gdb/", "ds/ds.gdb/a00000001.gdbtable"])]
    #[case("implicit", &["ds/ds.gdb/a00000001.gdbtable", "ds/ds.gdb/a00000001.gdbtablx"])]
    fn a_geodatabase_directory_inside_an_archive_is_selectable(#[case] tag: &str, #[case] entries: &[&str]) {
        let dir = archive_tempdir(&format!("gdb-{tag}"));
        zip_named(&dir, "ds.zip", entries);
        let resolved = DatasetRoot::new(dir).resolve("ds", None).expect("resolve");
        assert!(resolved.ends_with("/ds/ds.gdb"), "{resolved}");
    }

    #[test]
    fn a_geodatabase_appears_once_however_many_members_it_holds() {
        let dir = archive_tempdir("gdbonce");
        zip_named(&dir, "ds.zip", &["ds.gdb/a00000001.gdbtable", "ds.gdb/a00000002.gdbtable", "ds.gdb/timestamps"]);
        let err = DatasetRoot::new(dir).resolve("ds", Some("nope")).unwrap_err();
        let ResolveError::PathNotFound { candidates, .. } = &err else { panic!("{err:?}") };
        assert_eq!(candidates, &["ds.gdb"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_geodatabase_resolves() {
        let dir = archive_tempdir("symlink");
        let target = dir.join("elsewhere").join("real.gdb");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("ds.gdb")).unwrap();
        let resolved = DatasetRoot::new(dir).resolve("ds", None).expect("resolve");
        assert!(resolved.ends_with("ds.gdb"), "{resolved}");
    }

    #[rstest]
    #[case("DS.GeoJSON")]
    #[case("ds.GEOJSON")]
    fn the_filename_case_does_not_have_to_match(#[case] filename: &str) {
        let dir = archive_tempdir(filename);
        std::fs::write(dir.join(filename), ARCHIVE_POINTS).unwrap();
        let resolved = DatasetRoot::new(dir).resolve("ds", None).expect(filename);
        assert_eq!(features_at(&resolved).expect("GDAL open"), 1);
    }

    #[test]
    fn an_exact_spelling_wins_over_a_case_variant() {
        let entries = vec![
            Entry { name: "ds.SHP".to_string(), is_dir: false },
            Entry { name: "ds.shp".to_string(), is_dir: false },
        ];
        assert_eq!(pick_entry(&entries, "ds.shp").map(|e| e.name.as_str()), Some("ds.shp"));
    }

    #[rstest]
    #[case(&["DS.GeoJSON", "ds.GEOJSON"])]
    #[case(&["ds.GEOJSON", "DS.GeoJSON"])]
    fn a_case_variant_is_picked_the_same_way_whatever_the_listing_order(#[case] names: &[&str]) {
        let entries: Vec<Entry> = names.iter().map(|n| Entry { name: n.to_string(), is_dir: false }).collect();
        assert_eq!(pick_entry(&entries, "ds.geojson").map(|e| e.name.as_str()), Some("DS.GeoJSON"));
    }
}
