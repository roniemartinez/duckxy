#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    GeoJson,
}

impl Format {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            // only @info makes .json differ from .geojson
            "json" | "geojson" => Some(Self::GeoJson),
            _ => None,
        }
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
