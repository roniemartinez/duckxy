#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    GeoJson,
}

const EXTENSIONS: &[(&str, Format)] = &[("geojson", Format::GeoJson), ("json", Format::GeoJson)];

impl Format {
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.to_ascii_lowercase();
        EXTENSIONS.iter().find(|(known, _)| *known == ext).map(|(_, format)| *format)
    }

    pub fn split(segment: &str) -> Option<(&str, &'static str, Format)> {
        let lower = segment.to_ascii_lowercase();
        EXTENSIONS
            .iter()
            .filter(|(ext, _)| lower.strip_suffix(ext).is_some_and(|stem| stem.ends_with('.')))
            .max_by_key(|(ext, _)| ext.len())
            .map(|(ext, format)| (&segment[..segment.len() - ext.len() - 1], *ext, *format))
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::GeoJson => "application/geo+json",
        }
    }

    pub fn header(self) -> &'static str {
        match self {
            Self::GeoJson => r#"{"type":"FeatureCollection","features":["#,
        }
    }

    pub fn footer(self) -> &'static str {
        match self {
            Self::GeoJson => "]}",
        }
    }

    pub fn separator(self) -> &'static str {
        match self {
            Self::GeoJson => ",",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("json")]
    #[case("geojson")]
    fn both_extensions_produce_geojson(#[case] ext: &str) {
        assert_eq!(Format::from_extension(ext), Some(Format::GeoJson));
    }

    #[rstest]
    #[case("xml")]
    #[case("gml")]
    #[case("")]
    fn unknown_extensions_are_none(#[case] ext: &str) {
        assert!(Format::from_extension(ext).is_none());
    }

    #[test]
    fn header_and_footer_alone_are_valid_json() {
        let f = Format::GeoJson;
        let body = format!("{}{}", f.header(), f.footer());
        serde_json::from_str::<serde_json::Value>(&body).unwrap();
    }
}
