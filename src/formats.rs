#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    GeoJson,
    Json,
}

const EXTENSIONS: &[(&str, Format)] = &[("geojson", Format::GeoJson), ("json", Format::Json)];

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

    pub fn requires_wgs84(self) -> bool {
        matches!(self, Self::GeoJson | Self::Json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("geojson", Format::GeoJson)]
    #[case("json", Format::Json)]
    fn each_json_suffix_maps_to_its_own_format(#[case] ext: &str, #[case] expected: Format) {
        assert_eq!(Format::from_extension(ext), Some(expected));
    }

    #[rstest]
    #[case("xml")]
    #[case("gml")]
    #[case("")]
    fn unknown_extensions_are_none(#[case] ext: &str) {
        assert!(Format::from_extension(ext).is_none());
    }
}
