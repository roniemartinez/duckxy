use crate::formats::Format;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Render {
    pub content_type: &'static str,
    pub header: &'static str,
    pub footer: &'static str,
    pub separator: &'static str,
}

impl Render {
    pub const fn of(format: Format) -> Render {
        match format {
            Format::GeoJson | Format::Json => Render {
                content_type: "application/geo+json",
                header: r#"{"type":"FeatureCollection","features":["#,
                footer: "]}",
                separator: ",",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(Format::GeoJson)]
    #[case(Format::Json)]
    fn the_core_render_frames_a_feature_collection(#[case] format: Format) {
        let r = Render::of(format);
        assert_eq!(r.content_type, "application/geo+json");
        assert_eq!(r.header, r#"{"type":"FeatureCollection","features":["#);
        assert_eq!(r.footer, "]}");
        assert_eq!(r.separator, ",");
    }

    #[rstest]
    #[case(Format::GeoJson)]
    #[case(Format::Json)]
    fn a_header_and_footer_alone_are_valid_json(#[case] format: Format) {
        let r = Render::of(format);
        let body = format!("{}{}", r.header, r.footer);
        serde_json::from_str::<serde_json::Value>(&body).unwrap();
    }
}
