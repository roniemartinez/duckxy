use std::fmt;

pub const DEFAULT_ENCODING: &str = crate::encodings::UTF8;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedUrl {
    pub dataset: String,
    pub path: Option<String>,
    pub encoding: String,
    pub format: crate::formats::Format,
    pub extension: &'static str,
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    Empty,
    EmptyOptionValue(String),
    EmptySourceValue,
    InvalidName(String),
    MissingOutputSegment,
    MissingSourcePrefix,
    RepeatedOption(String),
    UnknownFormat(String),
    UnknownOption(String),
    UnknownSource(String),
    MalformedEncoding(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => write!(f, "empty path"),
            ParseError::EmptyOptionValue(s) => write!(f, "option has no value: {s}"),
            ParseError::EmptySourceValue => write!(f, "source value is empty"),
            ParseError::InvalidName(n) => write!(f, "invalid name: {n}"),
            ParseError::MissingOutputSegment => write!(f, "missing .<format> suffix"),
            ParseError::MissingSourcePrefix => write!(f, "path must start with @"),
            ParseError::RepeatedOption(s) => write!(f, "option given more than once: {s}"),
            ParseError::UnknownFormat(s) => write!(f, "unknown format: {s}"),
            ParseError::UnknownOption(s) => write!(f, "unknown option: {s}"),
            ParseError::UnknownSource(s) => write!(f, "unknown source type: {s}"),
            ParseError::MalformedEncoding(s) => write!(f, "encoding has characters that are not allowed: {s}"),
        }
    }
}

impl std::error::Error for ParseError {}

pub fn parse(url: &str) -> Result<ParsedUrl, ParseError> {
    let url = url.trim_start_matches('/');
    if url.is_empty() {
        return Err(ParseError::Empty);
    }
    let Some((source, extension, format)) = crate::formats::Format::split(url) else {
        return Err(match url.rsplit_once('.') {
            Some((_, ext)) => ParseError::UnknownFormat(ext.to_ascii_lowercase()),
            None => ParseError::MissingOutputSegment,
        });
    };

    let mut scan = Scan { text: source, at: 0 };
    scan.expect(b'@')?;
    let kind = scan.take_until(b":");
    if kind != "dataset" {
        return Err(ParseError::UnknownSource(kind.to_string()));
    }
    scan.expect(b':')?;

    let name = scan.take_until(b":,/");
    if name.is_empty() {
        return Err(ParseError::EmptySourceValue);
    }
    if !is_valid_name(name) {
        return Err(ParseError::InvalidName(name.to_string()));
    }

    let mut path = None;
    if scan.eat(b':') {
        let value = scan.take_until(b",");
        if value.is_empty() {
            return Err(ParseError::EmptySourceValue);
        }
        path = Some(trim_filename(value).to_string());
    }

    let mut encoding = None;
    while scan.eat(b',') {
        let key = scan.take_until(b":,/");
        if !scan.eat(b':') {
            return Err(ParseError::UnknownOption(key.to_string()));
        }
        let value = scan.take_until(b",/");
        let slot = match key {
            "enc" | "encoding" => &mut encoding,
            _ => return Err(ParseError::UnknownOption(key.to_string())),
        };
        if value.is_empty() {
            return Err(ParseError::EmptyOptionValue(key.to_string()));
        }
        if slot.is_some() {
            return Err(ParseError::RepeatedOption(key.to_string()));
        }
        *slot = Some(value.to_string());
    }

    let encoding = match encoding {
        Some(label) => resolve_encoding(&label).ok_or(ParseError::MalformedEncoding(label))?,
        None => DEFAULT_ENCODING.to_string(),
    };

    Ok(ParsedUrl { dataset: name.to_string(), path, encoding, format, extension })
}

struct Scan<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Scan<'a> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn expect(&mut self, byte: u8) -> Result<(), ParseError> {
        if self.eat(byte) {
            Ok(())
        } else if byte == b'@' {
            Err(ParseError::MissingSourcePrefix)
        } else {
            Err(ParseError::UnknownSource(self.text.to_string()))
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            return true;
        }
        false
    }

    fn take_until(&mut self, stops: &[u8]) -> &'a str {
        let from = self.at;
        while let Some(byte) = self.peek() {
            if stops.contains(&byte) {
                break;
            }
            self.at += 1;
        }
        &self.text[from..self.at]
    }
}

fn trim_filename(path: &str) -> &str {
    let mut at = 0;
    let mut readable = None;
    for part in path.split('/') {
        let end = at + part.len();
        if crate::dataset::looks_like_source(&path[at..end]) {
            readable = Some(end);
        }
        at = end + 1;
    }
    readable.map_or(path, |end| &path[..end])
}

fn resolve_encoding(label: &str) -> Option<String> {
    if let Some((_, name)) = crate::encodings::ALIASES.iter().find(|(alias, _)| alias.eq_ignore_ascii_case(label)) {
        return Some(name.to_string());
    }
    let safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':');
    (!label.is_empty() && label.chars().all(safe)).then(|| label.to_string())
}

pub fn is_valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("/@dataset:plz-5stellig.json", "plz-5stellig", "json")]
    #[case("/@dataset:cities.GeoJSON", "cities", "geojson")]
    #[case("/@dataset:my.data.geojson", "my.data", "geojson")]
    fn parses_dataset_and_format(#[case] url: &str, #[case] dataset: &str, #[case] format: &str) {
        let p = parse(url).unwrap();
        assert_eq!(p.dataset, dataset);
        assert_eq!(p.extension, format);
    }

    #[rstest]
    #[case("/@dataset:x.geojson", "UTF-8", None)]
    #[case("/@dataset:x,enc:csISOLatin1.geojson", "ISO-8859-1", None)]
    #[case("/@dataset:x,enc:KOI8-R.geojson", "KOI8-R", None)]
    #[case("/@dataset:x:a/b/c.shp,encoding:utf-8.geojson", "UTF-8", Some("a/b/c.shp"))]
    #[case("/@dataset:uk:gb.geojson.geojson", "UTF-8", Some("gb.geojson"))]
    fn parses_path_and_options(#[case] url: &str, #[case] encoding: &str, #[case] path: Option<&str>) {
        let p = parse(url).unwrap();
        assert_eq!(p.encoding, encoding);
        assert_eq!(p.path.as_deref(), path);
    }

    #[rstest]
    #[case("/@dataset:plz-5stellig/output.geojson", "plz-5stellig", None, "UTF-8")]
    #[case("/@dataset:plz-5stellig/map.geojson", "plz-5stellig", None, "UTF-8")]
    #[case("/@dataset:plz-5stellig.geojson", "plz-5stellig", None, "UTF-8")]
    #[case("/@dataset:a/b/c.json", "a", None, "UTF-8")]
    #[case("/@dataset:a/b,enc:iso-8859-1.json", "a", None, "UTF-8")]
    #[case("/@dataset:a/.json", "a", None, "UTF-8")]
    #[case("/@dataset:x,enc:iso-8859-1/output.geojson", "x", None, "ISO-8859-1")]
    #[case("/@dataset:x:a/b/c.shp,enc:iso-8859-1/output.geojson", "x", Some("a/b/c.shp"), "ISO-8859-1")]
    #[case("/@dataset:uk:gb.geojson/download.geojson", "uk", Some("gb.geojson"), "UTF-8")]
    #[case("/@dataset:uk:gb.geojson,enc:utf-8/download.geojson", "uk", Some("gb.geojson"), "UTF-8")]
    #[case("/@dataset:uk:ds/ds.gdb/download.geojson", "uk", Some("ds/ds.gdb"), "UTF-8")]
    #[case("/@dataset:uk:a/b.geojson/c.shp/name.geojson", "uk", Some("a/b.geojson/c.shp"), "UTF-8")]
    #[case("/@dataset:uk:nothing/readable.geojson", "uk", Some("nothing/readable"), "UTF-8")]
    fn the_filename_segment_is_discarded(
        #[case] url: &str,
        #[case] dataset: &str,
        #[case] path: Option<&str>,
        #[case] encoding: &str,
    ) {
        let p = parse(url).unwrap();
        assert_eq!(p.dataset, dataset);
        assert_eq!(p.path.as_deref(), path);
        assert_eq!(p.encoding, encoding);
    }

    #[rstest]
    #[case("/", ParseError::Empty)]
    #[case("/@dataset:cities", ParseError::MissingOutputSegment)]
    #[case("/@dataset:cities.xml", ParseError::UnknownFormat("xml".to_string()))]
    #[case("/@elsewhere:thing.json", ParseError::UnknownSource("elsewhere".to_string()))]
    #[case("/dataset:cities.json", ParseError::MissingSourcePrefix)]
    #[case("/@dataset:.json", ParseError::EmptySourceValue)]
    #[case("/@dataset:x,enc:KOI8@8.geojson", ParseError::MalformedEncoding("KOI8@8".to_string()))]
    #[case("/@dataset:x,enc:.geojson", ParseError::EmptyOptionValue("enc".to_string()))]
    #[case("/@dataset:x,enc:utf-8,encoding:latin1.geojson", ParseError::RepeatedOption("encoding".to_string()))]
    #[case("/@dataset:x,zzz:1.geojson", ParseError::UnknownOption("zzz".to_string()))]
    #[case("/@dataset:x,prop:state:CA.geojson", ParseError::UnknownOption("prop".to_string()))]
    fn rejects(#[case] url: &str, #[case] expected: ParseError) {
        assert_eq!(parse(url), Err(expected));
    }
}
