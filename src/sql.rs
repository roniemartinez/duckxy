use crate::formats::Format;
use crate::grammar::{Grammar, StageCtx};
use crate::url::{ParsedUrl, Segment};
use crate::{filters, query};
use sea_query::{
    Alias, CommonTableExpression, Expr, Func, PostgresQueryBuilder, Query, SelectStatement, SimpleExpr, WithClause,
};

pub struct Pipeline {
    ctes: WithClause,
    input: Alias,
    input_name: String,
    steps: usize,
    geometry: String,
    side: Vec<(String, String, String)>,
}

impl Pipeline {
    pub fn source(source: &str, encoding: &str, columns: &[(String, String)]) -> anyhow::Result<Self> {
        let geometry = query::geometry_of(columns).ok_or(query::NoGeometry)?.to_string();
        let mut ctes = WithClause::new();
        let input = Alias::new("source");
        ctes.cte(
            CommonTableExpression::new()
                .query(
                    Query::select()
                        .expr(Expr::cust("*"))
                        .from_function(query::read_source(source, encoding), "src")
                        .take(),
                )
                .table_name(input.clone())
                .to_owned(),
        );
        Ok(Self { ctes, input, input_name: "source".to_string(), steps: 0, geometry, side: Vec::new() })
    }

    pub fn filter(&mut self, filters: &[Segment], columns: &[(String, String)]) -> anyhow::Result<()> {
        if let Some(predicate) = filters::condition(filters, columns, &self.geometry)? {
            let filtered = Query::select().expr(Expr::cust("*")).from(self.input.clone()).and_where(predicate).take();
            self.step(filtered);
        }
        Ok(())
    }

    pub fn reproject(&mut self, crs: Option<&str>, format: Format) {
        let Some(from) = crs.filter(|c| format.requires_wgs84() && !matches!(*c, "EPSG:4326" | "OGC:CRS84")) else {
            return;
        };
        let reprojected = Query::select()
            .expr(Expr::cust_with_exprs(
                "* REPLACE ($1 AS $2)",
                [
                    Func::cust("ST_Transform")
                        .arg(Expr::col(Alias::new(&self.geometry)))
                        .arg(from)
                        .arg("EPSG:4326")
                        .arg(Expr::cust("always_xy := true"))
                        .into(),
                    Expr::col(Alias::new(&self.geometry)),
                ],
            ))
            .from(self.input.clone())
            .take();
        self.step(reprojected);
    }

    pub fn step(&mut self, select: SelectStatement) {
        self.steps += 1;
        let name = format!("step_{}", self.steps);
        let alias = Alias::new(&name);
        self.ctes.cte(CommonTableExpression::new().query(select).table_name(alias.clone()).to_owned());
        self.input = alias;
        self.input_name = name;
    }

    pub fn cte_once(&mut self, name: &str, build: impl FnOnce(&Alias) -> SelectStatement) -> Alias {
        if let Some(alias) = self.cte(name) {
            return alias;
        }
        let mut full = format!("x_{name}");
        let mut seen = 0;
        while self.side.iter().any(|(_, _, taken)| taken == &full) {
            seen += 1;
            full = format!("x_{name}_{seen}");
        }
        let alias = Alias::new(&full);
        let select = build(&self.input);
        self.ctes.cte(CommonTableExpression::new().query(select).table_name(alias.clone()).to_owned());
        self.side.push((name.to_string(), self.input_name.clone(), full));
        alias
    }

    pub fn cte(&self, name: &str) -> Option<Alias> {
        self.side
            .iter()
            .find(|(registered, over, _)| registered == name && over == &self.input_name)
            .map(|(_, _, full)| Alias::new(full))
    }

    pub fn geometry(&self) -> &str {
        &self.geometry
    }

    pub fn input(&self) -> Alias {
        self.input.clone()
    }

    pub fn finish(mut self, format: Format) -> String {
        let geometry = self.geometry.clone();
        let replaced = Query::select()
            .expr(Expr::cust_with_exprs(
                "* REPLACE ($1 AS $2)",
                [
                    Func::cust("ST_AsGeoJSON").arg(Expr::col(Alias::new(&geometry))).into(),
                    Expr::col(Alias::new(&geometry)),
                ],
            ))
            .from(self.input.clone())
            .take();
        self.step(replaced);
        match format {
            Format::GeoJson | Format::Json => Query::select()
                .expr(Func::cast_as(
                    Func::cust("json_object").args([
                        Expr::val("type"),
                        Expr::val("Feature"),
                        Expr::val("properties"),
                        Func::cust("json_merge_patch")
                            .arg(Func::cust("to_json").arg(Expr::col(self.input.clone())))
                            .arg(Func::cust("json_object").args([Expr::val(&geometry), Expr::cust("NULL")]))
                            .into(),
                        Expr::val("geometry"),
                        Func::cast_as(Expr::col(Alias::new(&geometry)), "JSON").into(),
                    ]),
                    "VARCHAR",
                ))
                .from(self.input)
                .to_owned()
                .with(self.ctes)
                .to_string(PostgresQueryBuilder),
        }
    }

    pub fn finish_raw(self) -> String {
        Query::select()
            .expr(Expr::cust("*"))
            .from(self.input)
            .to_owned()
            .with(self.ctes)
            .to_string(PostgresQueryBuilder)
    }
}

pub fn build_sql(
    source: &str,
    encoding: &str,
    filters: &[Segment],
    format: Format,
    columns: &[(String, String)],
    crs: Option<&str>,
) -> anyhow::Result<String> {
    let mut pipeline = Pipeline::source(source, encoding, columns)?;
    pipeline.filter(filters, columns)?;
    pipeline.reproject(crs, format);
    Ok(pipeline.finish(format))
}

pub fn plan(
    grammar: &Grammar,
    parsed: &ParsedUrl,
    source: &str,
    columns: &[(String, String)],
    crs: Option<&str>,
) -> anyhow::Result<String> {
    let mut pipeline = Pipeline::source(source, &parsed.encoding, columns)?;
    pipeline.filter(&parsed.filters, columns)?;
    pipeline.reproject(crs, parsed.format);

    if parsed.actions.is_empty() {
        return Ok(pipeline.finish(parsed.format));
    }

    for action in &parsed.actions {
        let Some(def) = grammar.action_for(&action.name) else {
            anyhow::bail!("action {:?} is not registered in this grammar", action.name);
        };
        let select = {
            let mut ctx = StageCtx::new(&mut pipeline);
            let mut parts: Vec<(&'static str, SimpleExpr)> = Vec::new();
            for segment in &action.segments {
                let Some(option) = def.option(&segment.name) else {
                    return Err(anyhow::anyhow!("option {:?} is not registered on {:?}", segment.name, action.name));
                };
                let Some(fragment) = option.shape_for(segment.params.len()).and_then(|s| s.fragment.as_ref()) else {
                    return Err(anyhow::anyhow!(
                        "option {:?} does not take {} parameters",
                        segment.name,
                        segment.params.len()
                    ));
                };
                let expr = fragment(&mut ctx, &segment.params).map_err(anyhow::Error::new)?;
                parts.push((option.canonical(), expr));
            }
            (def.assemble)(&mut ctx, parts)
        };
        pipeline.step(select);
    }
    Ok(pipeline.finish_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn geom_columns() -> Vec<(String, String)> {
        vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())]
    }

    fn id_filter() -> Vec<Segment> {
        vec![Segment { name: "id".to_string(), params: vec!["7".to_string()] }]
    }

    fn probe_grammar() -> Grammar {
        fn bounds(ctx: &mut StageCtx) -> SimpleExpr {
            let extent = ctx.cte_once("extent", |from| {
                Query::select().expr_as(Expr::cust("ST_Extent(geom)"), Alias::new("e")).from(from.clone()).take()
            });
            Expr::col((extent, Alias::new("e")))
        }
        fn count(_: &mut StageCtx) -> SimpleExpr {
            Expr::cust("COUNT(*)")
        }
        fn width(ctx: &mut StageCtx) -> SimpleExpr {
            let extent = ctx.cte_once("extent", |from| {
                Query::select().expr_as(Expr::cust("ST_Extent(geom)"), Alias::new("e")).from(from.clone()).take()
            });
            Expr::cust_with_exprs("ST_XMax($1)", [Expr::col((extent, Alias::new("e")))])
        }
        fn three(
            _: &mut StageCtx,
            c: crate::grammar::Column,
            o: crate::grammar::Operator,
            v: crate::grammar::Value,
        ) -> SimpleExpr {
            Expr::cust(format!("{}|{}|{}", c.0, o.0, v.0))
        }
        fn assemble(ctx: &mut StageCtx, parts: Vec<(&'static str, SimpleExpr)>) -> SelectStatement {
            let mut select = Query::select();
            for (name, expr) in parts {
                select.expr_as(expr, Alias::new(name));
            }
            select.from(ctx.data());
            if let Some(extent) = ctx.cte("extent") {
                select.from(extent);
            }
            select.take()
        }
        let mut g = Grammar::core();
        g.action(
            crate::grammar::action("probe")
                .option(&["bounds", "b"], bounds)
                .option(&["count", "n"], count)
                .option(&["width"], width)
                .option(&["three"], three)
                .terminal(assemble)
                .build(),
        );
        g
    }

    fn planned(url: &str) -> String {
        let grammar = probe_grammar();
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse(url, &grammar).unwrap();
        plan(&grammar, &parsed, "/x.geojson", &columns, None).unwrap()
    }

    #[test]
    fn a_request_with_no_action_is_the_feature_pipeline() {
        let out = planned("/@dataset:x.geojson");
        assert!(out.contains("ST_AsGeoJSON"), "{out}");
        assert!(!out.contains("COUNT(*)"), "{out}");
    }

    #[test]
    fn an_action_replaces_the_feature_select_with_its_own() {
        let out = planned("/@dataset:x/@probe/count.json");
        assert!(out.contains("COUNT(*) AS \"count\""), "{out}");
        assert!(!out.contains("ST_AsGeoJSON"), "an action must skip the geojson conversion: {out}");
    }

    #[test]
    fn option_expressions_arrive_in_url_order_under_canonical_names() {
        let out = planned("/@dataset:x/@probe/n,b.json");
        let count_at = out.find("AS \"count\"").expect("count");
        let bounds_at = out.find("AS \"bounds\"").expect("bounds");
        assert!(count_at < bounds_at, "aliases must not reorder the parts: {out}");
    }

    #[test]
    fn a_fragment_may_add_a_side_table_that_options_share() {
        let out = planned("/@dataset:x/@probe/b,width.json");
        assert_eq!(out.matches("\"x_extent\" AS").count(), 1, "{out}");
        assert!(out.contains("\"x_extent\""), "the side table must be joined, not dangling: {out}");
    }

    #[test]
    fn every_option_sees_the_same_relation() {
        let out = planned("/@dataset:x,id:7/@probe/count,bounds,width.json");
        assert_eq!(out.matches("FROM \"step_1\"").count(), 2, "options disagreed about the input: {out}");
        assert_eq!(out.matches("\"x_extent\" AS").count(), 1, "the shared side table was built twice: {out}");
    }

    #[test]
    fn a_fragment_receives_its_parameters_in_order() {
        let out = planned("/@dataset:x/@probe/three:name:eq:foo.json");
        assert!(out.contains("name|eq|foo AS \"three\""), "parameters arrived out of order: {out}");
    }

    #[test]
    fn a_bad_parameter_reports_its_own_position() {
        fn flagged(_: &mut StageCtx, _v: crate::grammar::Value, _f: crate::grammar::Boolean) -> SimpleExpr {
            Expr::cust("1")
        }
        fn assemble(_: &mut StageCtx, _parts: Vec<(&'static str, SimpleExpr)>) -> SelectStatement {
            Query::select().expr(Expr::cust("1")).take()
        }
        let mut grammar = Grammar::core();
        grammar.action(crate::grammar::action("probe").option(&["pair"], flagged).terminal(assemble).build());
        let columns = vec![("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse("/@dataset:x/@probe/pair:ok:banana.json", &grammar).unwrap();
        let err = plan(&grammar, &parsed, "/x.geojson", &columns, None).unwrap_err();
        let typed = err.downcast_ref::<crate::grammar::ParamError>().expect("the kind must survive");
        assert_eq!(typed.at, 1, "the wrong parameter position was reported");
        assert_eq!(typed.got, "banana");
    }

    #[test]
    fn an_action_still_reprojects_a_source_that_is_not_wgs84() {
        let grammar = probe_grammar();
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse("/@dataset:x/@probe/count.json", &grammar).unwrap();
        let out = plan(&grammar, &parsed, "/x.geojson", &columns, Some("EPSG:25832")).unwrap();
        assert!(out.contains("ST_Transform"), "an action skipped reprojection: {out}");
        assert!(out.find("ST_Transform") < out.find("COUNT(*)"), "reprojection must precede the action: {out}");
    }

    #[test]
    fn a_filter_applies_before_the_action_runs() {
        let out = planned("/@dataset:x,id:7/@probe/count.json");
        assert!(out.contains("\"step_1\" AS (SELECT * FROM \"source\" WHERE"), "{out}");
        assert!(out.contains("COUNT(*) AS \"count\" FROM \"step_1\""), "the action must read the filtered rows: {out}");
    }

    #[test]
    fn a_bad_action_parameter_surfaces_as_a_typed_error() {
        fn flagged(_: &mut StageCtx, _flag: crate::grammar::Boolean) -> SimpleExpr {
            Expr::cust("1")
        }
        fn assemble(_: &mut StageCtx, _parts: Vec<(&'static str, SimpleExpr)>) -> SelectStatement {
            Query::select().expr(Expr::cust("1")).take()
        }
        let mut grammar = Grammar::core();
        grammar.action(crate::grammar::action("probe").option(&["flag"], flagged).terminal(assemble).build());
        let columns = vec![("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse("/@dataset:x/@probe/flag:banana.json", &grammar).unwrap();
        let err = plan(&grammar, &parsed, "/x.geojson", &columns, None).unwrap_err();
        let typed = err.downcast_ref::<crate::grammar::ParamError>().expect("the kind must survive");
        assert_eq!(typed.expected, crate::grammar::Param::Boolean);
        assert_eq!(typed.got, "banana");
    }

    #[test]
    fn a_step_names_itself_and_becomes_the_input() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns).unwrap();
        p.step(Query::select().expr(Expr::cust("1")).from(p.input()).take());
        p.step(Query::select().expr(Expr::cust("2")).from(p.input()).take());
        let out = p.finish_raw();
        assert!(out.contains("\"step_1\" AS (SELECT 1 FROM \"source\")"), "{out}");
        assert!(out.contains("\"step_2\" AS (SELECT 2 FROM \"step_1\")"), "{out}");
        assert!(out.trim_end().ends_with("SELECT * FROM \"step_2\""), "{out}");
    }

    #[test]
    fn a_side_table_is_namespaced_shared_and_does_not_advance_the_input() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns).unwrap();
        assert!(p.cte("extent").is_none());
        p.cte_once("extent", |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
        p.cte_once("extent", |_| panic!("a repeated side table must not be rebuilt"));
        assert!(p.cte("extent").is_some());
        let out = p.finish_raw();
        assert_eq!(out.matches("\"x_extent\" AS").count(), 1, "the side table was emitted twice: {out}");
        assert!(out.trim_end().ends_with("SELECT * FROM \"source\""), "a side table advanced the input: {out}");
    }

    #[test]
    fn a_side_table_lookup_names_the_alias_built_over_the_current_input() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns).unwrap();
        p.cte_once("extent", |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
        p.filter(&id_filter(), &columns).unwrap();
        assert!(p.cte("extent").is_none(), "a side table from an earlier stage must not be reported as live");
        p.cte_once("extent", |from| Query::select().expr(Expr::cust("2")).from(from.clone()).take());

        let live = p.cte("extent").expect("a side table was built over the current input");
        let out = Query::select().expr(Expr::cust("*")).from(live).to_owned().to_string(PostgresQueryBuilder);
        assert!(out.ends_with("FROM \"x_extent_1\""), "the lookup named the stale alias: {out}");
    }

    #[rstest]
    #[case(&["a", "a", "a_1"])]
    #[case(&["a_1", "a", "a"])]
    fn a_side_table_never_reuses_an_alias_another_one_took(#[case] names: &[&str]) {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns).unwrap();
        for (at, name) in names.iter().enumerate() {
            if at > 0 {
                p.filter(&id_filter(), &columns).unwrap();
            }
            p.cte_once(name, |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
            assert!(p.cte(name).is_some(), "{name} was built but is not live");
        }
        let out = p.finish_raw();
        let mut declared: Vec<&str> = out
            .match_indices("\" AS (")
            .map(|(at, _)| {
                let open = out[..at].rfind('"').expect("a quoted cte name");
                &out[open + 1..at]
            })
            .collect();
        let total = declared.len();
        declared.sort_unstable();
        declared.dedup();
        assert_eq!(declared.len(), total, "a cte name was declared twice: {out}");
    }

    #[test]
    fn a_side_table_is_not_shared_across_stages() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns).unwrap();
        p.cte_once("extent", |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
        p.filter(&id_filter(), &columns).unwrap();
        p.cte_once("extent", |from| Query::select().expr(Expr::cust("2")).from(from.clone()).take());
        let out = p.finish_raw();
        assert!(out.contains("\"x_extent\" AS (SELECT 1 FROM \"source\")"), "{out}");
        assert!(out.contains("\"x_extent_1\" AS (SELECT 2 FROM \"step_1\")"), "a stale side table was reused: {out}");
    }

    #[test]
    fn a_side_table_reads_from_the_current_input() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns).unwrap();
        p.filter(&id_filter(), &columns).unwrap();
        p.cte_once("extent", |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
        let out = p.finish_raw();
        assert!(out.contains("\"x_extent\" AS (SELECT 1 FROM \"step_1\")"), "{out}");
    }

    #[test]
    fn finish_raw_skips_the_geojson_conversion() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns).unwrap();
        p.step(Query::select().expr(Expr::cust("1")).from(p.input()).take());
        assert_eq!(
            p.finish_raw(),
            "WITH \"source\" AS (SELECT * FROM ST_Read('/x.geojson', open_options=list_value('ENCODING=UTF-8')) AS \"src\") , \"step_1\" AS (SELECT 1 FROM \"source\") SELECT * FROM \"step_1\""
        );
    }

    #[test]
    fn reprojection_takes_the_step_after_the_filter() {
        let columns = geom_columns();
        let out =
            build_sql("/x.geojson", "UTF-8", &id_filter(), Format::GeoJson, &columns, Some("EPSG:25832")).unwrap();
        assert!(out.contains("\"step_2\" AS (SELECT * REPLACE (ST_Transform("), "{out}");
        assert!(out.contains("FROM \"step_1\""), "{out}");
        assert!(out.contains("\"step_3\" AS (SELECT * REPLACE (ST_AsGeoJSON("), "{out}");
        assert!(out.contains("to_json(\"step_3\")"), "the feature select read the wrong cte: {out}");
        assert!(out.trim_end().ends_with("FROM \"step_3\""), "the feature select read the wrong cte: {out}");
    }

    #[test]
    fn a_crs_already_in_wgs84_adds_no_reprojection_step() {
        let columns = geom_columns();
        let out = build_sql("/x.geojson", "UTF-8", &[], Format::GeoJson, &columns, Some("EPSG:4326")).unwrap();
        assert!(!out.contains("ST_Transform"), "{out}");
        assert!(out.contains("\"step_1\" AS (SELECT * REPLACE (ST_AsGeoJSON("), "a pass-through step was added: {out}");
    }

    #[test]
    fn build_sql_without_a_geometry_column_errors_rather_than_panics() {
        let columns = vec![("id".to_string(), "BIGINT".to_string())];
        let err = build_sql("/x.geojson", "UTF-8", &[], Format::GeoJson, &columns, None).unwrap_err();
        assert!(err.downcast_ref::<query::NoGeometry>().is_some(), "{err:#}");
    }

    #[test]
    fn an_unfiltered_request_adds_no_filter_step() {
        let columns = vec![("name".to_string(), "VARCHAR".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let out = build_sql("/x.geojson", "UTF-8", &[], Format::GeoJson, &columns, None).unwrap();
        assert!(!out.contains("WHERE"), "an empty filter list added a predicate: {out}");
    }

    #[test]
    fn a_filtered_request_is_the_first_numbered_step() {
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let filters = vec![Segment { name: "id".to_string(), params: vec!["7".to_string()] }];
        let out = build_sql("/x.geojson", "UTF-8", &filters, Format::GeoJson, &columns, None).unwrap();
        assert!(out.contains("\"step_1\" AS (SELECT * FROM \"source\" WHERE"), "{out}");
        assert!(out.contains("\"id\" = (7)"), "{out}");
    }
}
