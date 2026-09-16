use crate::grammar::{Action, Opt, Param, StageCtx, opt};
use crate::url::Segment;

pub struct Process;

impl Action for Process {
    fn name(&self) -> &'static str {
        "process"
    }

    fn short(&self) -> &'static str {
        "p"
    }

    fn options(&self) -> &'static [Opt] {
        const OPTIONS: &[Opt] = &[opt("reproject", "r", &[&[Param::Token], &[Param::Token, Param::Token]])];
        OPTIONS
    }

    fn run(&self, ctx: &mut StageCtx, segments: &[Segment]) -> anyhow::Result<()> {
        for segment in segments {
            match segment.name.as_str() {
                "reproject" => {
                    let Some(target) = target_crs(&segment.params) else {
                        anyhow::bail!("reproject takes one or two parameters, got {}", segment.params.len());
                    };
                    let geometry = ctx.transform(&target);
                    ctx.replace_geometry(geometry);
                    ctx.set_crs(target);
                }
                other => anyhow::bail!("option {other:?} is not handled"),
            }
        }
        Ok(())
    }
}

fn target_crs(params: &[String]) -> Option<String> {
    match params {
        [code] => Some(format!("EPSG:{}", code.to_uppercase())),
        [authority, code] => Some(format!("{}:{}", authority.to_uppercase(), code.to_uppercase())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::grammar::Grammar;
    use crate::sql::{WGS84, plan};
    use rstest::rstest;
    use std::sync::Arc;

    fn planned(url: &str, crs: Option<&str>) -> String {
        let grammar = Grammar::core();
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse(url, &grammar).unwrap();
        let backend = Arc::new(crate::backend::Backend::default());
        plan(&grammar, &parsed, "/x.geojson", &columns, crs, backend).unwrap()
    }

    #[rstest]
    #[case("/@dataset:x/@process/r:3857.geojson", "'EPSG:3857'")]
    #[case("/@dataset:x/@p/reproject:3857.geojson", "'EPSG:3857'")]
    #[case("/@dataset:x/@process/r:esri:54030.geojson", "'ESRI:54030'")]
    #[case("/@dataset:x/@process/r:ogc:crs84.geojson", "'OGC:CRS84'")]
    fn a_target_is_normalised_to_an_uppercase_authority_and_code(#[case] url: &str, #[case] expected: &str) {
        assert!(planned(url, Some("EPSG:25832")).contains(expected), "{}", planned(url, Some("EPSG:25832")));
    }

    #[test]
    fn the_source_crs_reaches_the_transform() {
        let out = planned("/@dataset:x/@process/r:3857.geojson", Some("epsg:25832"));
        assert!(out.contains("'EPSG:25832', 'EPSG:3857'"), "the source crs did not reach the transform: {out}");
    }

    #[test]
    fn a_reproject_updates_the_crs_the_output_converts_from() {
        let out = planned("/@dataset:x/@process/r:3857.geojson", Some("EPSG:25832"));
        assert!(out.contains("'EPSG:3857', 'EPSG:4326'"), "the output did not convert back from 3857: {out}");
        assert_eq!(out.matches("ST_Transform").count(), 2, "expected the explicit and the implicit step: {out}");
    }

    #[test]
    fn a_redundant_reproject_emits_no_transform() {
        let out = planned("/@dataset:x/@process/r:4326.geojson", Some(WGS84));
        assert!(!out.contains("ST_Transform"), "a same-crs reproject wrapped the geometry: {out}");
    }

    #[rstest]
    #[case("EPSG:4326")]
    #[case("OGC:CRS84")]
    #[case("CRS84")]
    #[case("crs84")]
    fn a_source_already_in_wgs84_adds_no_output_step(#[case] crs: &str) {
        let out = planned("/@dataset:x.geojson", Some(crs));
        assert!(!out.contains("ST_Transform"), "{crs} was not recognised as wgs84: {out}");
    }

    #[test]
    fn a_second_reproject_reads_the_crs_the_first_one_set() {
        let out = planned("/@dataset:x/@process/r:3857,r:25832.geojson", Some("EPSG:25832"));
        assert!(out.contains("'EPSG:25832', 'EPSG:3857'"), "the first leg is wrong: {out}");
        assert!(out.contains("'EPSG:3857', 'EPSG:25832'"), "the second leg did not read the crs the first set: {out}");
        assert!(out.contains("'EPSG:25832', 'EPSG:4326'"), "the output did not convert from the last crs: {out}");
    }

    #[test]
    fn a_reproject_step_is_named_after_its_action() {
        let out = planned("/@dataset:x/@process/r:3857.geojson", Some("EPSG:25832"));
        assert!(out.contains("\"process_1\" AS"), "{out}");
    }
}
