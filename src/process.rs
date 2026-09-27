use crate::grammar::{Action, Opt, Param, StageCtx, flag, opt};
use crate::url::Segment;
use std::sync::LazyLock;

pub struct Unary {
    pub name: &'static str,
    pub short: &'static str,
    pub call: &'static str,
}

pub const UNARY: &[Unary] = &[
    Unary { name: "bbox", short: "bb", call: "ST_Envelope" },
    Unary { name: "boundary", short: "bnd", call: "ST_Boundary" },
    Unary { name: "centroid", short: "ctr", call: "ST_Centroid" },
    Unary { name: "convexhull", short: "cvh", call: "ST_ConvexHull" },
    Unary { name: "endpoint", short: "edp", call: "ST_EndPoint" },
    Unary { name: "exteriorring", short: "er", call: "ST_ExteriorRing" },
    Unary { name: "flipcoordinates", short: "fc", call: "ST_FlipCoordinates" },
    Unary { name: "force2d", short: "f2d", call: "ST_Force2D" },
    Unary { name: "linemerge", short: "lm", call: "ST_LineMerge" },
    Unary { name: "makevalid", short: "mv", call: "ST_MakeValid" },
    Unary { name: "multi", short: "m", call: "ST_Multi" },
    Unary { name: "node", short: "nd", call: "ST_Node" },
    Unary { name: "normalize", short: "norm", call: "ST_Normalize" },
    Unary { name: "orientedenvelope", short: "oe", call: "ST_MinimumRotatedRectangle" },
    Unary { name: "pointonsurface", short: "pos", call: "ST_PointOnSurface" },
    Unary { name: "points", short: "pts", call: "ST_Points" },
    Unary { name: "reverse", short: "rev", call: "ST_Reverse" },
    Unary { name: "startpoint", short: "stp", call: "ST_StartPoint" },
];

static OPTIONS: LazyLock<Vec<Opt>> = LazyLock::new(|| {
    std::iter::once(opt("reproject", "r", &[&[Param::Token], &[Param::Token, Param::Token]]))
        .chain(UNARY.iter().map(|op| flag(op.name, op.short)))
        .collect()
});

pub struct Process;

impl Action for Process {
    fn name(&self) -> &'static str {
        "process"
    }

    fn short(&self) -> &'static str {
        "p"
    }

    fn options(&self) -> &'static [Opt] {
        &OPTIONS
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
                other => {
                    let Some(op) = UNARY.iter().find(|op| op.name == other) else {
                        anyhow::bail!("option {other:?} is not handled");
                    };
                    let geometry = ctx.call(op.call, vec![ctx.geom()])?;
                    ctx.replace_geometry(geometry);
                }
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
        plan(&grammar, &parsed, "/x.geojson", &columns, crs, &[], backend).unwrap()
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

    #[rstest]
    #[case("bbox", "bb", "ST_Envelope")]
    #[case("boundary", "bnd", "ST_Boundary")]
    #[case("centroid", "ctr", "ST_Centroid")]
    #[case("convexhull", "cvh", "ST_ConvexHull")]
    #[case("endpoint", "edp", "ST_EndPoint")]
    #[case("exteriorring", "er", "ST_ExteriorRing")]
    #[case("flipcoordinates", "fc", "ST_FlipCoordinates")]
    #[case("force2d", "f2d", "ST_Force2D")]
    #[case("linemerge", "lm", "ST_LineMerge")]
    #[case("makevalid", "mv", "ST_MakeValid")]
    #[case("multi", "m", "ST_Multi")]
    #[case("node", "nd", "ST_Node")]
    #[case("normalize", "norm", "ST_Normalize")]
    #[case("orientedenvelope", "oe", "ST_MinimumRotatedRectangle")]
    #[case("pointonsurface", "pos", "ST_PointOnSurface")]
    #[case("points", "pts", "ST_Points")]
    #[case("reverse", "rev", "ST_Reverse")]
    #[case("startpoint", "stp", "ST_StartPoint")]
    fn an_operation_wraps_the_geometry_in_its_function(#[case] name: &str, #[case] short: &str, #[case] call: &str) {
        let long = planned(&format!("/@dataset:x/@process/{name}.geojson"), Some(WGS84));
        let brief = planned(&format!("/@dataset:x/@p/{short}.geojson"), Some(WGS84));
        assert!(long.contains(&format!("{call}(\"geom\")")), "{long}");
        assert_eq!(long, brief, "{name} and {short} planned differently");
    }

    #[test]
    fn every_operation_is_tested() {
        assert_eq!(super::UNARY.len(), 18);
    }

    #[rstest]
    #[case("/@dataset:x/@process/banana.geojson")]
    #[case("/@dataset:x/@process/ctr:1.geojson")]
    #[case("/@dataset:x/@process/centroid:1:2.geojson")]
    fn an_unknown_or_misshapen_operation_is_refused_by_the_parser(#[case] url: &str) {
        assert!(crate::url::parse(url, &Grammar::core()).is_err(), "{url} parsed");
    }

    #[rstest]
    #[case("mv,ctr", "ST_MakeValid", "ST_Centroid")]
    #[case("ctr,mv", "ST_Centroid", "ST_MakeValid")]
    #[case("r:3857,bb", "ST_Transform", "ST_Envelope")]
    fn operations_apply_left_to_right(#[case] ops: &str, #[case] first: &str, #[case] second: &str) {
        let out = planned(&format!("/@dataset:x/@process/{ops}.geojson"), Some(WGS84));
        let (Some(inner), Some(outer)) = (out.find(first), out.find(second)) else {
            panic!("{ops} lost an operation: {out}");
        };
        assert!(inner < outer, "{ops} applied {second} before {first}: {out}");
    }

    #[test]
    fn every_operation_takes_exactly_one_geometry_in_duckdb() {
        crate::ensure_spatial();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch("LOAD spatial;").unwrap();
        let mut unary = conn
            .prepare(
                "SELECT count(*) FROM duckdb_functions() \
                 WHERE function_name = ? AND parameter_types = ['GEOMETRY'] AND return_type = 'GEOMETRY'",
            )
            .unwrap();
        let missing: Vec<&str> = super::UNARY
            .iter()
            .map(|op| op.call)
            .filter(|call| unary.query_row([call], |r| r.get::<_, i64>(0)).unwrap() == 0)
            .collect();
        assert!(missing.is_empty(), "no GEOMETRY to GEOMETRY overload: {missing:?}");
    }

    #[test]
    fn a_reproject_step_is_named_after_its_action() {
        let out = planned("/@dataset:x/@process/r:3857.geojson", Some("EPSG:25832"));
        assert!(out.contains("\"process_1\" AS"), "{out}");
    }
}
