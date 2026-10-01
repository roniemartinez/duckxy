use crate::grammar::{Action, Opt, Param, StageCtx, flag, opt};
use crate::url::Segment;
use sea_query::{Alias, Expr, ExprTrait, Func, Query, SimpleExpr};
use std::sync::LazyLock;

const ONE: &[&[Param]] = &[&[Param::Token]];
const TWO: &[&[Param]] = &[&[Param::Token, Param::Token]];
const FOUR: &[&[Param]] = &[&[Param::Token, Param::Token, Param::Token, Param::Token]];
const SIX: &[&[Param]] = &[&[Param::Token, Param::Token, Param::Token, Param::Token, Param::Token, Param::Token]];
const UP_TO_ONE: &[&[Param]] = &[&[], &[Param::Token]];
const SOURCE: &[&[Param]] = &[&[Param::Source]];
const TOLERANCE_AND_SOURCE: &[&[Param]] = &[&[Param::Token, Param::Source]];
const COUNT: &str = "count";
pub const UNION: &str = "ST_Union_Agg";
const MEMBERS: &str = "members";

pub struct Unary {
    pub name: &'static str,
    pub short: &'static str,
    pub call: &'static str,
}

pub const UNARY: &[Unary] = &[
    Unary { name: "bbox", short: "bb", call: "ST_Envelope" },
    Unary { name: "boundary", short: "bnd", call: "ST_Boundary" },
    Unary { name: "buildarea", short: "ba", call: "ST_BuildArea" },
    Unary { name: "centroid", short: "ctr", call: "ST_Centroid" },
    Unary { name: "convexhull", short: "cvh", call: "ST_ConvexHull" },
    Unary { name: "endpoint", short: "edp", call: "ST_EndPoint" },
    Unary { name: "exteriorring", short: "er", call: "ST_ExteriorRing" },
    Unary { name: "flipcoordinates", short: "fc", call: "ST_FlipCoordinates" },
    Unary { name: "force2d", short: "f2d", call: "ST_Force2D" },
    Unary { name: "linemerge", short: "lm", call: "ST_LineMerge" },
    Unary { name: "makepolygon", short: "mp", call: "ST_MakePolygon" },
    Unary { name: "makevalid", short: "mv", call: "ST_MakeValid" },
    Unary { name: "multi", short: "m", call: "ST_Multi" },
    Unary { name: "node", short: "nd", call: "ST_Node" },
    Unary { name: "normalize", short: "norm", call: "ST_Normalize" },
    Unary { name: "orientedenvelope", short: "oe", call: "ST_MinimumRotatedRectangle" },
    Unary { name: "pointonsurface", short: "pos", call: "ST_PointOnSurface" },
    Unary { name: "points", short: "pts", call: "ST_Points" },
    Unary { name: "reverse", short: "rev", call: "ST_Reverse" },
    Unary { name: "startpoint", short: "stp", call: "ST_StartPoint" },
    Unary { name: "voronoidiagram", short: "vd", call: "ST_VoronoiDiagram" },
];

pub struct Parameterised {
    pub name: &'static str,
    pub short: &'static str,
    pub call: &'static str,
    pub shapes: &'static [&'static [Param]],
    pub args: fn(&'static str, &[String], &StageCtx) -> anyhow::Result<Vec<SimpleExpr>>,
}

pub const PARAMETERISED: &[Parameterised] = &[
    Parameterised { name: "affine", short: "aff", call: "ST_Affine", shapes: SIX, args: numbers },
    Parameterised { name: "buffer", short: "b", call: "ST_Buffer", shapes: ONE, args: distances },
    Parameterised { name: "clip", short: "cl", call: "ST_Intersection", shapes: SOURCE, args: against },
    Parameterised { name: "closestpoint", short: "cp", call: "ST_ClosestPoint", shapes: SOURCE, args: against },
    Parameterised { name: "diff", short: "df", call: "ST_Difference", shapes: SOURCE, args: against },
    Parameterised { name: "expand", short: "exp", call: "ST_Expand", shapes: ONE, args: distances },
    Parameterised { name: "extract", short: "ex", call: "ST_CollectionExtract", shapes: ONE, args: dimension },
    Parameterised { name: "force3dm", short: "f3dm", call: "ST_Force3DM", shapes: ONE, args: numbers },
    Parameterised { name: "force3dz", short: "f3dz", call: "ST_Force3DZ", shapes: ONE, args: numbers },
    Parameterised { name: "force4d", short: "f4d", call: "ST_Force4D", shapes: TWO, args: numbers },
    Parameterised { name: "interiorringn", short: "irn", call: "ST_InteriorRingN", shapes: ONE, args: integers },
    Parameterised {
        name: "lineinterpolatepoint",
        short: "lip",
        call: "ST_LineInterpolatePoint",
        shapes: ONE,
        args: fractions,
    },
    Parameterised {
        name: "lineinterpolatepoints",
        short: "lips",
        call: "ST_LineInterpolatePoints",
        shapes: ONE,
        args: fractions_repeating,
    },
    Parameterised {
        name: "linesubstring",
        short: "lss",
        call: "ST_LineSubstring",
        shapes: TWO,
        args: rising_fractions,
    },
    Parameterised { name: "pointn", short: "pn", call: "ST_PointN", shapes: ONE, args: integers },
    Parameterised { name: "reduceprecision", short: "rp", call: "ST_ReducePrecision", shapes: ONE, args: tolerances },
    Parameterised {
        name: "removerepeatedpoints",
        short: "removepoints",
        call: "ST_RemoveRepeatedPoints",
        shapes: UP_TO_ONE,
        args: tolerances,
    },
    Parameterised { name: "rotate", short: "rot", call: "ST_Rotate", shapes: ONE, args: numbers },
    Parameterised { name: "rotatex", short: "rotx", call: "ST_RotateX", shapes: ONE, args: numbers },
    Parameterised { name: "rotatey", short: "roty", call: "ST_RotateY", shapes: ONE, args: numbers },
    Parameterised { name: "rotatez", short: "rotz", call: "ST_RotateZ", shapes: ONE, args: numbers },
    Parameterised { name: "scale", short: "sc", call: "ST_Scale", shapes: TWO, args: numbers },
    Parameterised { name: "shortestline", short: "shl", call: "ST_ShortestLine", shapes: SOURCE, args: against },
    Parameterised { name: "simplify", short: "s", call: "ST_Simplify", shapes: ONE, args: tolerances },
    Parameterised {
        name: "simplifypreservetopology",
        short: "spt",
        call: "ST_SimplifyPreserveTopology",
        shapes: ONE,
        args: tolerances,
    },
    Parameterised { name: "snap", short: "sn", call: "ST_Snap", shapes: TOLERANCE_AND_SOURCE, args: snapping },
    Parameterised { name: "symdiff", short: "sd", call: "ST_SymDifference", shapes: SOURCE, args: against },
    Parameterised { name: "translate", short: "tl", call: "ST_Translate", shapes: TWO, args: distances },
    Parameterised { name: "transscale", short: "ts", call: "ST_TransScale", shapes: FOUR, args: numbers },
];

fn number(name: &'static str, value: &str) -> anyhow::Result<f64> {
    match value.parse::<f64>() {
        Ok(held) if held.is_finite() => Ok(held),
        Ok(_) => Err(crate::Fault::bad_request(format!("{name} needs a finite number, got {value:?}"))),
        Err(_) => Err(crate::Fault::bad_request(format!("{name} needs a number, got {value:?}"))),
    }
}

fn numbers(name: &'static str, params: &[String], _: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    params.iter().map(|held| number(name, held).map(Expr::val)).collect()
}

fn measured(name: &'static str, param: &str) -> anyhow::Result<crate::units::Measure> {
    crate::units::parse(param).map_err(|e| crate::Fault::bad_request(format!("{name} {e}")))
}

fn scaled(measure: crate::units::Measure, ctx: &StageCtx) -> SimpleExpr {
    match measure {
        crate::units::Measure::Units(value) => Expr::val(value),
        crate::units::Measure::Metres(metres) => {
            Expr::val(metres).div(ctx.dialect().metres_per_unit(ctx.geom(), ctx.crs()))
        }
    }
}

fn distances(name: &'static str, params: &[String], ctx: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    params.iter().map(|held| Ok(scaled(measured(name, held)?, ctx))).collect()
}

fn tolerance(name: &'static str, param: &str, ctx: &StageCtx) -> anyhow::Result<SimpleExpr> {
    let measure = measured(name, param)?;
    match measure.is_negative() {
        true => Err(crate::Fault::bad_request(format!("{name} needs a tolerance that is not negative, got {param:?}"))),
        false => Ok(scaled(measure, ctx)),
    }
}

fn tolerances(name: &'static str, params: &[String], ctx: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    params.iter().map(|held| tolerance(name, held, ctx)).collect()
}

fn integers(name: &'static str, params: &[String], _: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    params
        .iter()
        .map(|held| match held.parse::<i64>() {
            Ok(held) => Ok(Expr::val(held)),
            Err(_) => Err(crate::Fault::bad_request(format!("{name} needs a whole number, got {held:?}"))),
        })
        .collect()
}

fn fractions(name: &'static str, params: &[String], _: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    params
        .iter()
        .map(|held| {
            let held = number(name, held)?;
            match (0.0..=1.0).contains(&held) {
                true => Ok(Expr::val(held)),
                false => Err(crate::Fault::bad_request(format!("{name} needs a fraction from 0 to 1, got {held}"))),
            }
        })
        .collect()
}

fn rising_fractions(name: &'static str, params: &[String], ctx: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    let ordered = params.iter().map(|held| number(name, held)).collect::<anyhow::Result<Vec<f64>>>()?;
    match ordered.windows(2).all(|pair| pair[0] <= pair[1]) {
        true => fractions(name, params, ctx),
        false => Err(crate::Fault::bad_request(format!("{name} needs its fractions in order, got {ordered:?}"))),
    }
}

fn fractions_repeating(name: &'static str, params: &[String], ctx: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    let mut args = fractions(name, params, ctx)?;
    args.push(Expr::val(true));
    Ok(args)
}

fn gathered(name: &'static str, raw: &str, ctx: &StageCtx) -> anyhow::Result<SimpleExpr> {
    match ctx.nested(raw) {
        Some(held) => Ok(crate::sql::nested_geometry(held, ctx.crs(), ctx.dialect())),
        None => Err(crate::Fault::bad_request(format!("{name} could not resolve the source {raw}"))),
    }
}

fn against(name: &'static str, params: &[String], ctx: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    let [raw] = params else {
        return Err(crate::Fault::bad_request(format!("{name} takes one source")));
    };
    Ok(vec![gathered(name, raw, ctx)?])
}

fn snapping(name: &'static str, params: &[String], ctx: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    let [within, raw] = params else {
        return Err(crate::Fault::bad_request(format!("{name} takes a tolerance and a source")));
    };
    let held = tolerance(name, within, ctx)?;
    Ok(vec![gathered(name, raw, ctx)?, held])
}

fn dimension(name: &'static str, params: &[String], _: &StageCtx) -> anyhow::Result<Vec<SimpleExpr>> {
    let held = params.first().and_then(|held| held.parse::<i32>().ok()).filter(|held| (1..=3).contains(held));
    match held {
        Some(held) => Ok(vec![Expr::val(held)]),
        None => Err(crate::Fault::bad_request(format!(
            "{name} needs 1 for points, 2 for lines or 3 for polygons, got {:?}",
            params.first().map(String::as_str).unwrap_or_default()
        ))),
    }
}

fn dissolve(ctx: &mut StageCtx, params: &[String]) -> anyhow::Result<()> {
    let geometry = ctx.geometry().to_string();
    let carried: Vec<(String, String)> = ctx.columns().iter().filter(|(name, _)| name != &geometry).cloned().collect();

    let grouping = match params {
        [] => None,
        [name] => {
            if [COUNT, MEMBERS].iter().any(|held| name.eq_ignore_ascii_case(held)) {
                return Err(crate::Fault::bad_request(format!(
                    "dissolve cannot group by {name:?}, which is the name it gives its own column"
                )));
            }
            match carried.iter().find(|(held, _)| held == name) {
                Some(held) => Some(held.clone()),
                None => return Err(anyhow::Error::new(crate::filters::FilterError::UnknownColumn(name.clone()))),
            }
        }
        _ => return Err(crate::Fault::bad_request("dissolve takes at most one column".to_string())),
    };

    let mut members: Vec<SimpleExpr> = Vec::new();
    for (name, _) in &carried {
        members.push(Expr::val(name.as_str()));
        members.push(Expr::col(Alias::new(name.as_str())));
    }
    let holding = Func::cust("json_group_array").arg(Func::cust("json_object").args(members));

    let mut select = Query::select();
    if let Some((name, _)) = &grouping {
        select.expr_as(Expr::col(Alias::new(name.as_str())), Alias::new(name.as_str()));
    }
    select
        .expr_as(Func::count(Expr::cust("*")), Alias::new(COUNT))
        .expr_as(holding, Alias::new(MEMBERS))
        .expr_as(ctx.call(UNION, vec![ctx.geom()])?, Alias::new(geometry.as_str()))
        .from(ctx.data());
    if let Some((name, _)) = &grouping {
        select.add_group_by([Expr::col(Alias::new(name.as_str()))]);
    }

    let mut columns = Vec::new();
    if let Some(held) = grouping {
        columns.push(held);
    }
    columns.push((COUNT.to_string(), "BIGINT".to_string()));
    columns.push((MEMBERS.to_string(), "JSON".to_string()));
    columns.push((geometry, "GEOMETRY".to_string()));
    ctx.regroup(select.take(), columns);
    Ok(())
}

static OPTIONS: LazyLock<Vec<Opt>> = LazyLock::new(|| {
    std::iter::once(opt("reproject", "r", &[&[Param::Token], &[Param::Token, Param::Token]]))
        .chain(std::iter::once(opt("dissolve", "d", &[&[], &[Param::Column]])))
        .chain(UNARY.iter().map(|op| flag(op.name, op.short)))
        .chain(PARAMETERISED.iter().map(|op| opt(op.name, op.short, op.shapes)))
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
                "dissolve" => dissolve(ctx, &segment.params)?,
                other => {
                    if let Some(op) = UNARY.iter().find(|op| op.name == other) {
                        let geometry = ctx.call(op.call, vec![ctx.geom()])?;
                        ctx.replace_geometry(geometry);
                        continue;
                    }
                    let Some(op) = PARAMETERISED.iter().find(|op| op.name == other) else {
                        anyhow::bail!("option {other:?} is not handled");
                    };
                    let mut args = vec![ctx.geom()];
                    args.extend((op.args)(op.name, &segment.params, ctx)?);
                    let geometry = ctx.call(op.call, args)?;
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
    #[case("buildarea", "ba", "ST_BuildArea")]
    #[case("makepolygon", "mp", "ST_MakePolygon")]
    #[case("voronoidiagram", "vd", "ST_VoronoiDiagram")]
    fn an_operation_wraps_the_geometry_in_its_function(#[case] name: &str, #[case] short: &str, #[case] call: &str) {
        let long = planned(&format!("/@dataset:x/@process/{name}.geojson"), Some(WGS84));
        let brief = planned(&format!("/@dataset:x/@p/{short}.geojson"), Some(WGS84));
        assert!(long.contains(&format!("{call}(\"geom\")")), "{long}");
        assert_eq!(long, brief, "{name} and {short} planned differently");
    }

    #[test]
    fn every_operation_is_tested() {
        assert_eq!(super::UNARY.len(), 21);
    }

    fn failed(url: &str) -> String {
        let grammar = Grammar::core();
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse(url, &grammar).unwrap();
        let backend = Arc::new(crate::backend::Backend::default());
        format!("{:#}", plan(&grammar, &parsed, "/x.geojson", &columns, Some(WGS84), &[], backend).unwrap_err())
    }

    #[rstest]
    #[case("buffer:5", "b:5", "ST_Buffer")]
    #[case("extract:3", "ex:3", "ST_CollectionExtract")]
    #[case("lineinterpolatepoint:0.5", "lip:0.5", "ST_LineInterpolatePoint")]
    #[case("lineinterpolatepoints:0.25", "lips:0.25", "ST_LineInterpolatePoints")]
    #[case("linesubstring:0.1:0.9", "lss:0.1:0.9", "ST_LineSubstring")]
    #[case("reduceprecision:0.001", "rp:0.001", "ST_ReducePrecision")]
    #[case("removerepeatedpoints", "removepoints", "ST_RemoveRepeatedPoints")]
    #[case("removerepeatedpoints:0.5", "removepoints:0.5", "ST_RemoveRepeatedPoints")]
    #[case("rotate:0.785", "rot:0.785", "ST_Rotate")]
    #[case("scale:2:3", "sc:2:3", "ST_Scale")]
    #[case("simplify:0.01", "s:0.01", "ST_Simplify")]
    #[case("translate:10:20", "tl:10:20", "ST_Translate")]
    #[case("affine:1:0:0:1:0:0", "aff:1:0:0:1:0:0", "ST_Affine")]
    #[case("expand:5", "exp:5", "ST_Expand")]
    #[case("force3dm:0", "f3dm:0", "ST_Force3DM")]
    #[case("force3dz:0", "f3dz:0", "ST_Force3DZ")]
    #[case("force4d:0:0", "f4d:0:0", "ST_Force4D")]
    #[case("interiorringn:1", "irn:1", "ST_InteriorRingN")]
    #[case("pointn:1", "pn:1", "ST_PointN")]
    #[case("rotatex:0.785", "rotx:0.785", "ST_RotateX")]
    #[case("rotatey:0.785", "roty:0.785", "ST_RotateY")]
    #[case("rotatez:0.785", "rotz:0.785", "ST_RotateZ")]
    #[case("simplifypreservetopology:0.01", "spt:0.01", "ST_SimplifyPreserveTopology")]
    #[case("transscale:1:2:3:4", "ts:1:2:3:4", "ST_TransScale")]
    fn a_scalar_operation_calls_its_function(#[case] long: &str, #[case] short: &str, #[case] call: &str) {
        let spelled = planned(&format!("/@dataset:x/@process/{long}.geojson"), Some(WGS84));
        let brief = planned(&format!("/@dataset:x/@p/{short}.geojson"), Some(WGS84));
        assert!(spelled.contains(&format!("{call}(\"geom\"")), "{spelled}");
        assert_eq!(spelled, brief, "{long} and {short} planned differently");
    }

    #[rstest]
    #[case("buffer:5", "5")]
    #[case("lineinterpolatepoint:0.5", "0.5")]
    #[case("linesubstring:0.1:0.9", "0.1, 0.9")]
    #[case("scale:2:3", "2, 3")]
    #[case("translate:10:20", "10, 20")]
    fn a_parameter_reaches_the_call_in_order(#[case] op: &str, #[case] expected: &str) {
        let out = planned(&format!("/@dataset:x/@process/{op}.geojson"), Some(WGS84));
        assert!(out.contains(expected), "expected {expected} in {out}");
    }

    #[test]
    fn the_plural_interpolation_asks_for_every_point() {
        let out = planned("/@dataset:x/@process/lips:0.25.geojson", Some(WGS84));
        assert!(out.contains("ST_LineInterpolatePoints(\"geom\", 0.25, TRUE)"), "{out}");
    }

    #[test]
    fn a_scalar_operation_without_a_parameter_keeps_the_shorter_call() {
        let out = planned("/@dataset:x/@process/removepoints.geojson", Some(WGS84));
        assert!(out.contains("ST_RemoveRepeatedPoints(\"geom\")"), "{out}");
    }

    #[test]
    fn operations_chain_left_to_right() {
        let out = planned("/@dataset:x/@process/mv,s:0.01,ctr.geojson", Some(WGS84));
        let valid = out.find("ST_MakeValid").expect("makevalid is missing");
        let simplify = out.find("ST_Simplify").expect("simplify is missing");
        let centroid = out.find("ST_Centroid").expect("centroid is missing");
        assert!(valid < simplify && simplify < centroid, "the steps are out of order: {out}");
    }

    #[rstest]
    #[case("/@dataset:x/@process/b:banana.geojson", "buffer needs a number")]
    #[case("/@dataset:x/@process/s:none.geojson", "simplify needs a number")]
    #[case("/@dataset:x/@process/lip:2.geojson", "lineinterpolatepoint needs a fraction from 0 to 1")]
    #[case("/@dataset:x/@process/lss:0:1.5.geojson", "linesubstring needs a fraction from 0 to 1")]
    #[case("/@dataset:x/@process/ex:9.geojson", "extract needs 1 for points")]
    #[case("/@dataset:x/@process/ex:point.geojson", "extract needs 1 for points")]
    fn an_unusable_parameter_is_refused(#[case] url: &str, #[case] expected: &str) {
        let held = failed(url);
        assert!(held.contains(expected), "expected {expected:?} in {held:?}");
    }

    #[rstest]
    #[case("/@dataset:x/@process/b.geojson")]
    #[case("/@dataset:x/@process/b:1:2.geojson")]
    #[case("/@dataset:x/@process/sc:1.geojson")]
    #[case("/@dataset:x/@process/removepoints:1:2.geojson")]
    fn a_scalar_operation_with_the_wrong_arity_is_refused_by_the_parser(#[case] url: &str) {
        assert!(crate::url::parse(url, &Grammar::core()).is_err(), "{url} parsed");
    }

    #[test]
    fn a_bare_distance_stays_in_the_unit_of_the_crs() {
        let out = planned("/@dataset:x/@process/b:1000.geojson", Some("EPSG:25832"));
        assert!(out.contains("ST_Buffer(\"geom\", 1000)"), "{out}");
        assert!(!out.contains("ST_Distance_Spheroid"), "a bare number was converted: {out}");
    }

    #[test]
    fn a_distance_with_a_unit_converts_into_the_unit_of_the_crs() {
        let out = planned("/@dataset:x/@process/b:1km.geojson", Some("EPSG:25832"));
        assert!(out.contains("ST_Buffer(\"geom\", 1000 / ("), "{out}");
        assert!(out.contains("ST_Distance_Spheroid"), "{out}");
        assert!(out.contains("'EPSG:25832', 'EPSG:4326'"), "the measurement did not start at the current crs: {out}");
        assert!(out.contains("ST_Centroid"), "the measurement did not use the feature: {out}");
    }

    #[rstest]
    #[case("1km", "1000m")]
    #[case("1km", "100000cm")]
    #[case("1mi", "1609.344m")]
    fn the_same_distance_written_two_ways_plans_the_same(#[case] one: &str, #[case] two: &str) {
        let first = planned(&format!("/@dataset:x/@process/b:{one}.geojson"), Some(WGS84));
        let second = planned(&format!("/@dataset:x/@process/b:{two}.geojson"), Some(WGS84));
        assert_eq!(first, second, "{one} and {two} planned differently");
    }

    #[test]
    fn a_unit_reads_the_crs_a_reproject_set() {
        let out = planned("/@dataset:x/@process/r:3857,b:1km.geojson", Some("EPSG:25832"));
        assert!(out.contains("'EPSG:3857', 'EPSG:4326'"), "the measurement ignored the reproject: {out}");
    }

    #[rstest]
    #[case("simplify:1m", "ST_Simplify")]
    #[case("translate:1km:0", "ST_Translate")]
    #[case("expand:1km", "ST_Expand")]
    #[case("simplifypreservetopology:1m", "ST_SimplifyPreserveTopology")]
    #[case("reduceprecision:1m", "ST_ReducePrecision")]
    #[case("removepoints:1m", "ST_RemoveRepeatedPoints")]
    fn every_distance_parameter_takes_a_unit(#[case] op: &str, #[case] call: &str) {
        let out = planned(&format!("/@dataset:x/@process/{op}.geojson"), Some("EPSG:25832"));
        assert!(out.contains(call), "{out}");
        assert!(out.contains("ST_Distance_Spheroid"), "{op} did not convert its unit: {out}");
    }

    #[rstest]
    #[case("/@dataset:x/@process/b:1banana.geojson", "does not know the unit")]
    #[case("/@dataset:x/@process/s:5furlongs.geojson", "does not know the unit")]
    #[case("/@dataset:x/@process/tl:1km:2parsec.geojson", "does not know the unit")]
    fn an_unknown_unit_is_refused(#[case] url: &str, #[case] expected: &str) {
        let held = failed(url);
        assert!(held.contains(expected), "expected {expected:?} in {held:?}");
    }

    #[rstest]
    #[case("rp:0.5")]
    #[case("sc:2:3")]
    #[case("rot:0.785")]
    #[case("lip:0.5")]
    #[case("ex:2")]
    fn a_parameter_that_is_not_a_distance_never_measures_the_crs(#[case] op: &str) {
        let out = planned(&format!("/@dataset:x/@process/{op}.geojson"), Some("EPSG:25832"));
        assert!(!out.contains("ST_Distance_Spheroid"), "{op} measured the crs: {out}");
    }

    #[rstest]
    #[case("/@dataset:x/@process/rot:inf.geojson", "rotate needs a finite number")]
    #[case("/@dataset:x/@process/rot:nan.geojson", "rotate needs a finite number")]
    #[case("/@dataset:x/@process/rot:1e400.geojson", "rotate needs a finite number")]
    #[case("/@dataset:x/@process/sc:inf:1.geojson", "scale needs a finite number")]
    #[case("/@dataset:x/@process/f3dz:nan.geojson", "force3dz needs a finite number")]
    #[case("/@dataset:x/@process/aff:inf:0:0:1:0:0.geojson", "affine needs a finite number")]
    #[case("/@dataset:x/@process/lip:nan.geojson", "lineinterpolatepoint needs a finite number")]
    fn a_number_that_is_not_finite_is_refused(#[case] url: &str, #[case] expected: &str) {
        let held = failed(url);
        assert!(held.contains(expected), "expected {expected:?} in {held:?}");
    }

    #[rstest]
    #[case("sc:1km:2", "scale needs a number")]
    #[case("rot:45deg", "rotate needs a number")]
    fn a_unit_on_a_parameter_that_is_not_a_distance_is_refused(#[case] op: &str, #[case] expected: &str) {
        let held = failed(&format!("/@dataset:x/@process/{op}.geojson"));
        assert!(held.contains(expected), "expected {expected:?} in {held:?}");
    }

    #[rstest]
    #[case("/@dataset:x/@process/s:-0.5.geojson", "simplify needs a tolerance that is not negative")]
    #[case("/@dataset:x/@process/s:-1m.geojson", "simplify needs a tolerance that is not negative")]
    #[case("/@dataset:x/@process/spt:-0.5.geojson", "simplifypreservetopology needs a tolerance")]
    #[case("/@dataset:x/@process/rp:-1.geojson", "reduceprecision needs a tolerance")]
    #[case("/@dataset:x/@process/removepoints:-1.geojson", "removerepeatedpoints needs a tolerance")]
    fn a_negative_tolerance_is_refused(#[case] url: &str, #[case] expected: &str) {
        let held = failed(url);
        assert!(held.contains(expected), "expected {expected:?} in {held:?}");
    }

    #[rstest]
    #[case("b:-1", "ST_Buffer")]
    #[case("b:-1km", "ST_Buffer")]
    #[case("exp:-5", "ST_Expand")]
    #[case("tl:-1km:0", "ST_Translate")]
    #[case("rot:-0.785", "ST_Rotate")]
    #[case("sc:-1:1", "ST_Scale")]
    #[case("rotx:-0.785", "ST_RotateX")]
    #[case("pn:-1", "ST_PointN")]
    fn a_negative_stays_allowed_where_it_has_a_meaning(#[case] op: &str, #[case] call: &str) {
        let out = planned(&format!("/@dataset:x/@process/{op}.geojson"), Some(WGS84));
        assert!(out.contains(call), "{out}");
    }

    #[test]
    fn a_substring_that_runs_backwards_is_refused() {
        let held = failed("/@dataset:x/@process/lss:0.9:0.1.geojson");
        assert!(held.contains("needs its fractions in order"), "{held}");
    }

    fn spellings() -> Vec<&'static str> {
        let mut held: Vec<&'static str> = vec!["process", "p", "reproject", "r"];
        for op in super::UNARY {
            held.extend([op.name, op.short]);
        }
        for op in super::PARAMETERISED {
            held.extend([op.name, op.short]);
        }
        held
    }

    #[test]
    fn no_two_operations_share_a_spelling() {
        let held = spellings();
        let mut seen: Vec<&str> = Vec::new();
        let mut twice: Vec<&str> = Vec::new();
        for name in held {
            match seen.contains(&name) {
                true => twice.push(name),
                false => seen.push(name),
            }
        }
        assert!(twice.is_empty(), "these spellings are claimed twice: {twice:?}");
    }

    #[test]
    fn the_operation_tables_stay_in_order() {
        for held in [
            super::UNARY.iter().map(|op| op.name).collect::<Vec<_>>(),
            super::PARAMETERISED.iter().map(|op| op.name).collect::<Vec<_>>(),
        ] {
            let mut sorted = held.clone();
            sorted.sort_unstable();
            assert_eq!(held, sorted, "the table is out of order, which makes a 20 row table unreadable");
        }
    }

    #[rstest]
    #[case("inf")]
    #[case("-inf")]
    #[case("Inf")]
    #[case("infinity")]
    #[case("nan")]
    #[case("NaN")]
    #[case("1e400")]
    #[case("-1e400")]
    fn no_operation_accepts_a_value_that_is_not_a_number(#[case] value: &str) {
        for op in super::PARAMETERISED {
            let arity = op.shapes.iter().map(|shape| shape.len()).max().unwrap_or(1).max(1);
            let params = vec![value; arity].join(":");
            let url = format!("/@dataset:x/@process/{}:{params}.geojson", op.name);
            let Ok(parsed) = crate::url::parse(&url, &Grammar::core()) else {
                continue;
            };
            let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
            let backend = Arc::new(crate::backend::Backend::default());
            let held = plan(&Grammar::core(), &parsed, "/x.geojson", &columns, Some(WGS84), &[], backend);
            assert!(held.is_err(), "{} accepted {value:?}: {:?}", op.name, held.map(|out| out.len()));
        }
    }

    fn described(raw: &str, source: &str, crs: Option<&str>) -> crate::query::Described {
        crate::query::Described {
            raw: raw.to_string(),
            source: source.to_string(),
            encoding: "UTF-8".to_string(),
            columns: vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())],
            crs: crs.map(str::to_string),
        }
    }

    fn planned_against(url: &str, crs: Option<&str>, nested: &[crate::query::Described]) -> String {
        let grammar = Grammar::core();
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse(url, &grammar).unwrap();
        let backend = Arc::new(crate::backend::Backend::default());
        plan(&grammar, &parsed, "/x.geojson", &columns, crs, nested, backend).unwrap()
    }

    #[rstest]
    #[case("clip", "cl", "ST_Intersection")]
    #[case("closestpoint", "cp", "ST_ClosestPoint")]
    #[case("diff", "df", "ST_Difference")]
    #[case("shortestline", "shl", "ST_ShortestLine")]
    #[case("symdiff", "sd", "ST_SymDifference")]
    fn an_operation_against_a_source_unions_it(#[case] long: &str, #[case] short: &str, #[case] call: &str) {
        let nested = [described("(@dataset:zones)", "/zones.geojson", None)];
        let spelled = planned_against(&format!("/@dataset:x/@process/{long}:(@dataset:zones).geojson"), None, &nested);
        let brief = planned_against(&format!("/@dataset:x/@p/{short}:(@dataset:zones).geojson"), None, &nested);
        assert!(spelled.contains(call), "{spelled}");
        assert!(spelled.contains("ST_Union_Agg"), "the source was not gathered: {spelled}");
        assert!(spelled.contains("\"nested_1\""), "the source built no cte: {spelled}");
        assert_eq!(spelled, brief, "{long} and {short} planned differently");
    }

    #[test]
    fn snap_takes_its_tolerance_after_the_source() {
        let nested = [described("(@dataset:zones)", "/zones.geojson", None)];
        let out = planned_against("/@dataset:x/@process/sn:0.5:(@dataset:zones).geojson", None, &nested);
        assert!(out.contains("ST_Snap"), "{out}");
        let at = out.find("ST_Snap").unwrap();
        let tail = &out[at..];
        assert!(tail.contains("0.5"), "the tolerance did not reach the call: {tail}");
        assert!(
            tail.find("ST_Union_Agg").unwrap() < tail.find("0.5").unwrap(),
            "the arguments are the wrong way round: {tail}"
        );
    }

    #[test]
    fn a_source_in_another_crs_is_transformed_before_it_is_gathered() {
        let nested = [described("(@dataset:zones)", "/zones.geojson", Some("EPSG:3857"))];
        let out = planned_against("/@dataset:x/@process/cl:(@dataset:zones).geojson", Some("EPSG:25832"), &nested);
        assert!(out.contains("'EPSG:3857', 'EPSG:25832'"), "the source was not brought into the pipeline crs: {out}");
    }

    #[test]
    fn a_source_already_in_the_pipeline_crs_is_not_transformed() {
        let nested = [described("(@dataset:zones)", "/zones.geojson", Some("EPSG:25832"))];
        let out = planned_against("/@dataset:x/@process/cl:(@dataset:zones).geojson", Some("EPSG:25832"), &nested);
        assert!(!out.contains("'EPSG:25832', 'EPSG:25832'"), "the source was transformed to its own crs: {out}");
    }

    #[test]
    fn an_operation_against_a_source_can_follow_another_operation() {
        let nested = [described("(@dataset:zones)", "/zones.geojson", None)];
        let out = planned_against("/@dataset:x/@process/mv,cl:(@dataset:zones).geojson", None, &nested);
        assert!(out.find("ST_MakeValid").unwrap() < out.find("ST_Intersection").unwrap(), "out of order: {out}");
    }

    #[rstest]
    #[case("/@dataset:x/@process/cl.geojson")]
    #[case("/@dataset:x/@process/sn:(@dataset:zones).geojson")]
    #[case("/@dataset:x/@process/sn:0.5.geojson")]
    fn a_source_operation_without_its_source_is_refused_by_the_parser(#[case] url: &str) {
        assert!(crate::url::parse(url, &Grammar::core()).is_err(), "{url} parsed");
    }

    #[test]
    fn a_source_that_is_not_a_source_is_refused_by_the_planner() {
        let grammar = Grammar::core();
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse("/@dataset:x/@process/cl:zones.geojson", &grammar).unwrap();
        let backend = Arc::new(crate::backend::Backend::default());
        let held = plan(&grammar, &parsed, "/x.geojson", &columns, None, &[], backend).unwrap_err();
        assert!(format!("{held:#}").contains("expected a nested source"), "{held:#}");
    }

    #[test]
    fn a_snap_tolerance_takes_a_unit() {
        let nested = [described("(@dataset:zones)", "/zones.geojson", None)];
        let out = planned_against("/@dataset:x/@process/sn:100m:(@dataset:zones).geojson", Some("EPSG:4326"), &nested);
        assert!(out.contains("ST_Snap"), "{out}");
        assert!(out.contains("ST_Distance_Spheroid"), "the tolerance did not convert its unit: {out}");
        assert!(out.contains("100 / ("), "the metres did not reach the conversion: {out}");
    }

    #[test]
    fn a_bare_snap_tolerance_stays_in_the_crs_unit() {
        let nested = [described("(@dataset:zones)", "/zones.geojson", None)];
        let out = planned_against("/@dataset:x/@process/sn:0.5:(@dataset:zones).geojson", Some("EPSG:4326"), &nested);
        assert!(!out.contains("ST_Distance_Spheroid"), "a bare tolerance was converted: {out}");
    }

    #[test]
    fn a_negative_snap_tolerance_is_refused() {
        let nested = [described("(@dataset:zones)", "/zones.geojson", None)];
        let grammar = Grammar::core();
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse("/@dataset:x/@process/sn:-1:(@dataset:zones).geojson", &grammar).unwrap();
        let backend = Arc::new(crate::backend::Backend::default());
        let held = plan(&grammar, &parsed, "/x.geojson", &columns, None, &nested, backend).unwrap_err();
        assert!(format!("{held:#}").contains("not negative"), "{held:#}");
    }

    fn failed_with(url: &str, columns: Vec<(String, String)>) -> String {
        let grammar = Grammar::core();
        let parsed = crate::url::parse(url, &grammar).unwrap();
        let backend = Arc::new(crate::backend::Backend::default());
        format!("{:#}", plan(&grammar, &parsed, "/x.geojson", &columns, Some(WGS84), &[], backend).unwrap_err())
    }

    fn planned_with(url: &str, columns: Vec<(String, String)>) -> String {
        let grammar = Grammar::core();
        let parsed = crate::url::parse(url, &grammar).unwrap();
        let backend = Arc::new(crate::backend::Backend::default());
        plan(&grammar, &parsed, "/x.geojson", &columns, Some(WGS84), &[], backend).unwrap()
    }

    #[test]
    fn a_dissolve_unions_every_row_into_one() {
        let out = planned("/@dataset:x/@process/d.geojson", Some(WGS84));
        assert!(out.contains("ST_Union_Agg"), "{out}");
        assert!(out.contains("COUNT(*)"), "{out}");
        assert!(out.contains("json_group_array"), "{out}");
        assert!(!out.contains("GROUP BY"), "an unkeyed dissolve grouped by something: {out}");
    }

    #[test]
    fn a_dissolve_keeps_every_property_inside_members() {
        let out = planned("/@dataset:x/@process/d.geojson", Some(WGS84));
        assert!(out.contains("json_object('id', \"id\")"), "the source columns are not carried: {out}");
    }

    #[test]
    fn a_dissolve_by_column_groups_by_it_and_keeps_it() {
        let out = planned_with(
            "/@dataset:x/@process/d:name.geojson",
            vec![
                ("name".to_string(), "VARCHAR".to_string()),
                ("pop".to_string(), "BIGINT".to_string()),
                ("geom".to_string(), "GEOMETRY".to_string()),
            ],
        );
        assert!(out.contains("GROUP BY \"name\""), "{out}");
        assert!(out.contains("json_object('name', \"name\", 'pop', \"pop\")"), "{out}");
        assert!(out.contains("'properties', json_object('name', \"name\", 'count', \"count\", 'members'"), "{out}");
    }

    #[test]
    fn a_dissolve_hands_the_output_only_what_survived() {
        let out = planned("/@dataset:x/@process/d.geojson", Some(WGS84));
        let tail = &out[out.rfind("SELECT CAST(json_object").expect("the output select is missing")..];
        assert!(tail.contains("'count', \"count\""), "{tail}");
        assert!(tail.contains("'members', \"members\""), "{tail}");
        assert!(!tail.contains("'id', \"id\""), "a column that no longer exists reached the output: {tail}");
    }

    #[test]
    fn a_dissolve_of_a_source_with_only_a_geometry_still_counts() {
        let out = planned_with("/@dataset:x/@process/d.geojson", vec![("geom".to_string(), "GEOMETRY".to_string())]);
        assert!(out.contains("json_group_array(json_object())"), "{out}");
        assert!(out.contains("COUNT(*)"), "{out}");
    }

    #[test]
    fn a_dissolve_chains_with_the_operations_around_it() {
        let out = planned("/@dataset:x/@process/mv,d,ctr.geojson", Some(WGS84));
        let valid = out.find("ST_MakeValid").expect("makevalid missing");
        let union = out.find("ST_Union_Agg").expect("dissolve missing");
        let centre = out.find("ST_Centroid").expect("centroid missing");
        assert!(valid < union && union < centre, "the steps are out of order: {out}");
    }

    #[test]
    fn a_dissolve_reads_the_crs_a_reproject_set() {
        let out = planned("/@dataset:x/@process/r:3857,d.geojson", Some("EPSG:25832"));
        assert!(out.contains("'EPSG:3857', 'EPSG:4326'"), "the output did not convert from the last crs: {out}");
    }

    #[rstest]
    #[case("/@dataset:x/@process/d:nope.geojson", "source has no column: nope")]
    #[case("/@dataset:x/@process/d:count.geojson", "cannot group by \"count\"")]
    #[case("/@dataset:x/@process/d:members.geojson", "cannot group by \"members\"")]
    #[case("/@dataset:x/@process/d:Count.geojson", "cannot group by \"Count\"")]
    #[case("/@dataset:x/@process/d:COUNT.geojson", "cannot group by \"COUNT\"")]
    #[case("/@dataset:x/@process/d:Members.geojson", "cannot group by \"Members\"")]
    #[case("/@dataset:x/@process/d:MEMBERS.geojson", "cannot group by \"MEMBERS\"")]
    fn a_dissolve_that_cannot_group_says_why(#[case] url: &str, #[case] expected: &str) {
        let held = failed(url);
        assert!(held.contains(expected), "expected {expected:?} in {held:?}");
    }

    #[test]
    fn a_dissolve_refuses_a_column_duckdb_would_read_as_its_own() {
        let held = failed_with(
            "/@dataset:x/@process/d:Count.geojson",
            vec![("Count".to_string(), "VARCHAR".to_string()), ("geom".to_string(), "GEOMETRY".to_string())],
        );
        assert!(held.contains("cannot group by"), "a case that duckdb folds together was allowed: {held:?}");
    }

    #[test]
    fn a_dissolve_takes_at_most_one_column() {
        assert!(crate::url::parse("/@dataset:x/@process/d:a:b.geojson", &Grammar::core()).is_err());
    }

    #[test]
    fn every_parameterised_operation_is_tested() {
        assert_eq!(super::PARAMETERISED.len(), 29);
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
