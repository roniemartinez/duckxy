use crate::grammar::{Grammar, StageCtx};
use crate::url::ParsedUrl;
use sea_query::{Alias, Expr, Func, Query, SelectStatement};

pub trait Output: Send + Sync {
    fn extensions(&self) -> &'static [&'static str];
    fn content_type(&self) -> &'static str;
    fn rows(&self, ctx: &mut StageCtx, parsed: &ParsedUrl) -> anyhow::Result<SelectStatement>;

    fn applies(&self, _parsed: &ParsedUrl) -> bool {
        true
    }

    fn overrides(&self) -> bool {
        false
    }

    fn header(&self) -> &'static str {
        ""
    }

    fn separator(&self) -> &'static str {
        ""
    }

    fn footer(&self) -> &'static str {
        ""
    }

    fn requires_wgs84(&self) -> bool {
        false
    }
}

impl std::fmt::Debug for dyn Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output").field("extensions", &self.extensions()).finish()
    }
}

impl PartialEq for dyn Output {
    fn eq(&self, other: &Self) -> bool {
        self.extensions() == other.extensions()
    }
}

impl Eq for dyn Output {}

pub struct GeoJson;

impl Output for GeoJson {
    fn extensions(&self) -> &'static [&'static str] {
        &["geojson", "json"]
    }

    fn content_type(&self) -> &'static str {
        "application/geo+json"
    }

    fn header(&self) -> &'static str {
        r#"{"type":"FeatureCollection","features":["#
    }

    fn separator(&self) -> &'static str {
        ","
    }

    fn footer(&self) -> &'static str {
        "]}"
    }

    fn requires_wgs84(&self) -> bool {
        true
    }

    fn rows(&self, ctx: &mut StageCtx, _parsed: &ParsedUrl) -> anyhow::Result<SelectStatement> {
        let geometry = ctx.geometry().to_string();
        let encoded = ctx.dialect().as_geojson(Expr::col(Alias::new(&geometry)));
        ctx.replace_geometry(encoded);
        let data = ctx.data();
        Ok(Query::select()
            .expr(Func::cast_as(
                Func::cust("json_object").args([
                    Expr::val("type"),
                    Expr::val("Feature"),
                    Expr::val("properties"),
                    Func::cust("json_merge_patch")
                        .arg(Func::cust("to_json").arg(Expr::col(data.clone())))
                        .arg(Func::cust("json_object").args([Expr::val(&geometry), Expr::cust("NULL")]))
                        .into(),
                    Expr::val("geometry"),
                    Func::cast_as(Expr::col(Alias::new(&geometry)), "JSON").into(),
                ]),
                "VARCHAR",
            ))
            .from(data)
            .take())
    }
}

pub fn split(segment: &str, grammar: &Grammar) -> Option<(usize, &'static str)> {
    let lower = segment.to_ascii_lowercase();
    grammar
        .outputs
        .iter()
        .flat_map(|output| output.extensions().iter().copied())
        .filter(|ext| lower.strip_suffix(ext).is_some_and(|stem| stem.ends_with('.')))
        .max_by_key(|ext| ext.len())
        .map(|ext| (segment.len() - ext.len() - 1, ext))
}

pub fn for_extension(ext: &str, grammar: &Grammar) -> Option<std::sync::Arc<dyn Output>> {
    let ext = ext.to_ascii_lowercase();
    grammar.outputs.iter().rev().find(|output| output.extensions().contains(&ext.as_str())).cloned()
}

pub fn claim(ext: &str, parsed: &ParsedUrl, grammar: &Grammar) -> Option<std::sync::Arc<dyn Output>> {
    let ext = ext.to_ascii_lowercase();
    grammar
        .outputs
        .iter()
        .rev()
        .find(|output| output.extensions().contains(&ext.as_str()) && output.applies(parsed))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    struct Doc;

    impl Output for Doc {
        fn extensions(&self) -> &'static [&'static str] {
            &["doc"]
        }
        fn content_type(&self) -> &'static str {
            "application/json"
        }
        fn rows(&self, ctx: &mut StageCtx, _: &ParsedUrl) -> anyhow::Result<SelectStatement> {
            Ok(Query::select().expr(Expr::cust("1")).from(ctx.data()).take())
        }
    }

    #[rstest]
    #[case("geojson")]
    #[case("json")]
    #[case("GeoJSON")]
    fn each_json_suffix_resolves_to_the_core_output(#[case] ext: &str) {
        let held = for_extension(ext, &Grammar::core()).expect("the core output claims it");
        assert_eq!(held.content_type(), "application/geo+json");
    }

    #[rstest]
    #[case("xml")]
    #[case("gml")]
    #[case("")]
    fn unknown_extensions_resolve_to_nothing(#[case] ext: &str) {
        assert!(for_extension(ext, &Grammar::core()).is_none());
    }

    #[test]
    fn the_core_output_frames_a_feature_collection() {
        let g = GeoJson;
        assert_eq!(g.content_type(), "application/geo+json");
        assert_eq!(g.header(), r#"{"type":"FeatureCollection","features":["#);
        assert_eq!(g.separator(), ",");
        assert_eq!(g.footer(), "]}");
        assert!(g.requires_wgs84());
        assert!(!g.overrides());
    }

    #[test]
    fn a_header_and_footer_alone_are_valid_json() {
        let body = format!("{}{}", GeoJson.header(), GeoJson.footer());
        serde_json::from_str::<serde_json::Value>(&body).unwrap();
    }

    #[test]
    fn an_output_that_declares_nothing_gets_empty_framing() {
        assert_eq!(Doc.header(), "");
        assert_eq!(Doc.separator(), "");
        assert_eq!(Doc.footer(), "");
        assert!(!Doc.requires_wgs84());
    }

    #[test]
    fn the_longest_matching_extension_wins() {
        let mut g = Grammar::default();
        g.register_output(Doc);
        struct Long;
        impl Output for Long {
            fn extensions(&self) -> &'static [&'static str] {
                &["shp.doc"]
            }
            fn content_type(&self) -> &'static str {
                "application/x-long"
            }
            fn rows(&self, ctx: &mut StageCtx, _: &ParsedUrl) -> anyhow::Result<SelectStatement> {
                Ok(Query::select().expr(Expr::cust("1")).from(ctx.data()).take())
            }
        }
        g.register_output(Long);
        let (cut, ext) = split("@dataset:x.shp.doc", &g).expect("a compound extension resolves");
        assert_eq!(&"@dataset:x.shp.doc"[..cut], "@dataset:x");
        assert_eq!(ext, "shp.doc");
    }

    #[test]
    fn nothing_is_split_off_without_a_dot() {
        assert!(split("@dataset:xgeojson", &Grammar::core()).is_none());
    }

    #[test]
    fn an_empty_registry_resolves_nothing() {
        assert!(for_extension("geojson", &Grammar::default()).is_none());
        assert!(split("@dataset:x.geojson", &Grammar::default()).is_none());
    }
}
