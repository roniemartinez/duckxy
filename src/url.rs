use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedUrl {
    pub dataset: String,
    pub format: String,
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    Empty,
    MissingSourcePrefix,
    UnknownSource(String),
    EmptySourceValue,
    InvalidName(String),
    MissingOutputSegment,
    InvalidOutputSegment(String),
    UnknownFormat(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => write!(f, "empty path"),
            ParseError::MissingSourcePrefix => write!(f, "path must start with @"),
            ParseError::UnknownSource(s) => write!(f, "unknown source type: {s}"),
            ParseError::EmptySourceValue => write!(f, "source value is empty"),
            ParseError::InvalidName(n) => write!(f, "invalid name: {n}"),
            ParseError::MissingOutputSegment => write!(f, "missing /<name>.<format> segment"),
            ParseError::InvalidOutputSegment(s) => write!(f, "invalid output segment: {s}"),
            ParseError::UnknownFormat(s) => write!(f, "unknown format: {s}"),
        }
    }
}

impl std::error::Error for ParseError {}

pub fn parse(path: &str) -> Result<ParsedUrl, ParseError> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Err(ParseError::Empty);
    }

    let (source_part, output_part) = match path.rsplit_once('/') {
        Some((s, o)) => (s, o),
        None => return Err(ParseError::MissingOutputSegment),
    };

    let format = parse_output(output_part)?;
    let dataset = parse_source(source_part)?;
    Ok(ParsedUrl { dataset, format })
}

fn parse_output(seg: &str) -> Result<String, ParseError> {
    let (_filename, ext) = seg.rsplit_once('.').ok_or_else(|| ParseError::InvalidOutputSegment(seg.to_string()))?;
    let ext = ext.to_ascii_lowercase();
    if crate::formats::Format::from_extension(&ext).is_none() {
        return Err(ParseError::UnknownFormat(ext));
    }
    Ok(ext)
}

fn parse_source(seg: &str) -> Result<String, ParseError> {
    if !seg.starts_with('@') {
        return Err(ParseError::MissingSourcePrefix);
    }
    let (kind, value) = seg[1..].split_once(':').ok_or_else(|| ParseError::UnknownSource(seg.to_string()))?;
    if kind != "dataset" {
        return Err(ParseError::UnknownSource(kind.to_string()));
    }
    if value.is_empty() {
        return Err(ParseError::EmptySourceValue);
    }
    if !is_valid_name(value) {
        return Err(ParseError::InvalidName(value.to_string()));
    }
    Ok(value.to_string())
}

pub fn is_valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("/@dataset:plz-5stellig/output.json", "plz-5stellig", "json")]
    #[case("/@dataset:cities/output.geojson", "cities", "geojson")]
    #[case("/@dataset:cities/anything.geojson", "cities", "geojson")]
    #[case("/@dataset:cities/.geojson", "cities", "geojson")]
    #[case("/@dataset:cities/output.GeoJSON", "cities", "geojson")]
    fn parses_dataset_and_format(#[case] path: &str, #[case] dataset: &str, #[case] format: &str) {
        let p = parse(path).unwrap();
        assert_eq!(p.dataset, dataset);
        assert_eq!(p.format, format);
    }

    #[rstest]
    #[case("/", ParseError::Empty)]
    #[case("/@dataset:cities", ParseError::MissingOutputSegment)]
    #[case("/@dataset:cities/output.xml", ParseError::UnknownFormat("xml".to_string()))]
    fn rejects(#[case] path: &str, #[case] expected: ParseError) {
        assert_eq!(parse(path), Err(expected));
    }

    #[rstest]
    #[case("/@dataset:cities/noextension")]
    #[case("/@elsewhere:thing/output.json")]
    #[case("/@dataset:../etc/output.json")]
    #[case("/@dataset:/output.json")]
    #[case("/dataset:cities/output.json")]
    #[case("/@dataset:cities,id:UK/output.json")]
    fn rejects_malformed_paths(#[case] path: &str) {
        assert!(parse(path).is_err(), "expected {path} to be rejected");
    }
}
