use crate::backend::Backend;
use crate::formats::Output;
use crate::grammar::{Grammar, ParamError, StageCtx};
use crate::url::{ParsedUrl, Segment};
use crate::{filters, query};
use sea_query::{
    Alias, CommonTableExpression, Expr, PostgresQueryBuilder, Query, SelectStatement, SimpleExpr, WithClause,
};
use std::sync::Arc;

pub struct Pipeline {
    ctes: WithClause,
    input: Alias,
    input_name: String,
    steps: Vec<(String, usize)>,
    geometry: String,
    backend: Arc<Backend>,
    side: Vec<(String, String, String)>,
}

impl Pipeline {
    pub fn source(
        source: &str,
        encoding: &str,
        columns: &[(String, String)],
        backend: Arc<Backend>,
    ) -> anyhow::Result<Self> {
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
        Ok(Self {
            ctes,
            input,
            input_name: "source".to_string(),
            steps: Vec::new(),
            geometry,
            backend,
            side: Vec::new(),
        })
    }

    pub fn filter(&mut self, filters: &[Segment], columns: &[(String, String)]) -> anyhow::Result<()> {
        if let Some(predicate) = filters::condition(filters, columns, &self.geometry)? {
            let filtered = Query::select().expr(Expr::cust("*")).from(self.input.clone()).and_where(predicate).take();
            self.step("step", filtered);
        }
        Ok(())
    }

    pub fn reproject(&mut self, crs: Option<&str>, output: &dyn Output) {
        let Some(from) = crs.filter(|c| output.requires_wgs84() && !matches!(*c, "EPSG:4326" | "OGC:CRS84")) else {
            return;
        };
        let reprojected = Query::select()
            .expr(Expr::cust_with_exprs(
                "* REPLACE ($1 AS $2)",
                [
                    self.dialect().transform(Expr::col(Alias::new(&self.geometry)), from, "EPSG:4326"),
                    Expr::col(Alias::new(&self.geometry)),
                ],
            ))
            .from(self.input.clone())
            .take();
        self.step("step", reprojected);
    }

    pub fn step(&mut self, prefix: &str, select: SelectStatement) {
        let n = match self.steps.iter_mut().find(|(known, _)| known == prefix) {
            Some((_, seen)) => {
                *seen += 1;
                *seen
            }
            None => {
                self.steps.push((prefix.to_string(), 1));
                1
            }
        };
        let name = format!("{prefix}_{n}");
        let alias = Alias::new(&name);
        self.ctes.cte(CommonTableExpression::new().query(select).table_name(alias.clone()).to_owned());
        self.input = alias;
        self.input_name = name;
    }

    pub fn cte_once(&mut self, prefix: &str, name: &str, build: impl FnOnce(&Alias) -> SelectStatement) -> Alias {
        if let Some(alias) = self.cte(prefix, name) {
            return alias;
        }
        let key = format!("{prefix}_{name}");
        let mut full = key.clone();
        let mut seen = 0;
        while self.side.iter().any(|(_, _, taken)| taken == &full) {
            seen += 1;
            full = format!("{key}_{seen}");
        }
        let alias = Alias::new(&full);
        let select = build(&self.input);
        self.ctes.cte(CommonTableExpression::new().query(select).table_name(alias.clone()).to_owned());
        self.side.push((key, self.input_name.clone(), full));
        alias
    }

    pub fn cte(&self, prefix: &str, name: &str) -> Option<Alias> {
        let key = format!("{prefix}_{name}");
        self.side
            .iter()
            .find(|(registered, over, _)| registered == &key && over == &self.input_name)
            .map(|(_, _, full)| Alias::new(full))
    }

    pub fn geometry(&self) -> &str {
        &self.geometry
    }

    pub fn replace_geometry(&mut self, prefix: &str, geometry: SimpleExpr) {
        let select = Query::select()
            .expr(Expr::cust_with_exprs("* REPLACE ($1 AS $2)", [geometry, Expr::col(Alias::new(&self.geometry))]))
            .from(self.input.clone())
            .take();
        self.step(prefix, select);
    }

    pub fn backend(&self) -> &Arc<Backend> {
        &self.backend
    }

    pub fn dialect(&self) -> &dyn crate::backend::Dialect {
        self.backend.dialect()
    }

    pub fn input(&self) -> Alias {
        self.input.clone()
    }

    pub fn finish(self, select: SelectStatement) -> String {
        select.with(self.ctes).to_string(PostgresQueryBuilder)
    }
}

pub fn plan(
    grammar: &Grammar,
    parsed: &ParsedUrl,
    source: &str,
    columns: &[(String, String)],
    crs: Option<&str>,
    backend: Arc<Backend>,
) -> anyhow::Result<String> {
    let mut pipeline = Pipeline::source(source, &parsed.encoding, columns, backend)?;
    pipeline.filter(&parsed.filters, columns)?;
    pipeline.reproject(crs, parsed.output.as_ref());

    for action in &parsed.actions {
        let Some(def) = grammar.action_for(&action.name) else {
            anyhow::bail!("action {:?} is not registered in this grammar", action.name);
        };
        for segment in &action.segments {
            let Some(option) = def.option(&segment.name) else {
                anyhow::bail!("option {:?} is not registered on {:?}", segment.name, action.name);
            };
            let Some(shape) = option.shape_for(segment.params.len()) else {
                anyhow::bail!("option {:?} does not take {} parameters", segment.name, segment.params.len());
            };
            for (at, (kind, raw)) in shape.iter().zip(&segment.params).enumerate() {
                if !kind.accepts(raw) {
                    return Err(anyhow::Error::new(ParamError { at, expected: *kind, got: raw.clone() }));
                }
            }
        }
        let mut ctx = StageCtx::new(&mut pipeline, def.canonical());
        def.run(&mut ctx, &action.segments)?;
    }

    let select = {
        let mut ctx = StageCtx::new(&mut pipeline, "out");
        parsed.output.clone().rows(&mut ctx, parsed)?
    };
    Ok(pipeline.finish(select))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(pipeline: Pipeline) -> String {
        let input = pipeline.input();
        pipeline.finish(Query::select().expr(Expr::cust("*")).from(input).take())
    }

    fn parsed_for(url: &str) -> crate::url::ParsedUrl {
        crate::url::parse(url, &Grammar::core()).unwrap()
    }

    fn backend() -> Arc<Backend> {
        Arc::new(Backend::default())
    }
    use rstest::rstest;

    fn geom_columns() -> Vec<(String, String)> {
        vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())]
    }

    fn id_filter() -> Vec<Segment> {
        vec![Segment { name: "id".to_string(), params: vec!["7".to_string()] }]
    }

    struct Probe;

    impl crate::grammar::Action for Probe {
        fn name(&self) -> &'static str {
            "test"
        }

        fn options(&self) -> &'static [crate::grammar::Opt] {
            use crate::grammar::{Param, opt};
            const OPTIONS: &[crate::grammar::Opt] = &[
                opt("bounds", "b", &[&[]]),
                opt("count", "n", &[&[]]),
                opt("width", "", &[&[]]),
                opt("three", "", &[&[Param::Column, Param::Operator, Param::Value]]),
            ];
            OPTIONS
        }

        fn run(&self, ctx: &mut StageCtx, segments: &[Segment]) -> anyhow::Result<()> {
            let extent = |ctx: &mut StageCtx| {
                ctx.cte_once("extent", |from| {
                    Query::select()
                        .expr_as(Expr::cust("ST_Extent_Agg(geom)"), Alias::new("e"))
                        .from(from.clone())
                        .take()
                })
            };
            let mut parts: Vec<(&'static str, SimpleExpr)> = Vec::new();
            for segment in segments {
                let part: (&'static str, SimpleExpr) = match segment.name.as_str() {
                    "bounds" => {
                        let e = extent(ctx);
                        ("bounds", Expr::col((e, Alias::new("e"))))
                    }
                    "count" => ("count", Expr::cust("COUNT(*)")),
                    "width" => {
                        let e = extent(ctx);
                        ("width", Expr::cust_with_exprs("ST_XMax($1)", [Expr::col((e, Alias::new("e")))]))
                    }
                    "three" => ("three", Expr::cust(segment.params.join("|"))),
                    other => anyhow::bail!("option {other:?} is not handled"),
                };
                parts.push(part);
            }
            let data = ctx.data();
            let held = ctx.cte("extent");
            let mut select = Query::select();
            select.expr(Expr::cust("*"));
            for (name, expr) in parts {
                select.expr_as(expr, Alias::new(name));
            }
            select.from(data);
            if let Some(extent) = held {
                select.from(extent);
            }
            ctx.step(select.take());
            Ok(())
        }
    }

    struct Second;

    impl crate::grammar::Action for Second {
        fn name(&self) -> &'static str {
            "second"
        }

        fn options(&self) -> &'static [crate::grammar::Opt] {
            const OPTIONS: &[crate::grammar::Opt] = &[crate::grammar::flag("tally", "")];
            OPTIONS
        }

        fn run(&self, ctx: &mut StageCtx, _: &[Segment]) -> anyhow::Result<()> {
            let extent = ctx.cte_once("extent", |from| {
                Query::select().expr_as(Expr::cust("ST_Extent_Agg(geom)"), Alias::new("e")).from(from.clone()).take()
            });
            let data = ctx.data();
            ctx.step(
                Query::select()
                    .expr(Expr::cust("*"))
                    .expr_as(Expr::col((extent, Alias::new("e"))), Alias::new("tally"))
                    .from(data)
                    .take(),
            );
            Ok(())
        }
    }

    struct Typed;

    impl crate::grammar::Action for Typed {
        fn name(&self) -> &'static str {
            "typed"
        }

        fn options(&self) -> &'static [crate::grammar::Opt] {
            use crate::grammar::{Param, opt};
            const OPTIONS: &[crate::grammar::Opt] =
                &[opt("pair", "", &[&[Param::Boolean, Param::Boolean]]), opt("flag", "", &[&[Param::Boolean]])];
            OPTIONS
        }

        fn run(&self, _: &mut StageCtx, _: &[Segment]) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn typed_grammar() -> Grammar {
        let mut g = Grammar::core();
        g.register_action(Typed);
        g
    }

    fn test_action_grammar() -> Grammar {
        let mut g = Grammar::core();
        g.register_action(Probe);
        g
    }

    fn planned(url: &str) -> String {
        planned_with(url, test_action_grammar())
    }

    fn planned_with(url: &str, grammar: Grammar) -> String {
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse(url, &grammar).unwrap();
        plan(&grammar, &parsed, "/x.geojson", &columns, None, backend()).unwrap()
    }

    #[test]
    fn a_step_counter_is_kept_per_prefix() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns, backend()).unwrap();
        p.step("step", Query::select().expr(Expr::cust("1")).from(p.input()).take());
        p.step("alpha", Query::select().expr(Expr::cust("2")).from(p.input()).take());
        p.step("step", Query::select().expr(Expr::cust("3")).from(p.input()).take());
        let out = rendered(p);
        assert!(out.contains("\"step_1\" AS (SELECT 1 FROM \"source\")"), "{out}");
        assert!(out.contains("\"alpha_1\" AS (SELECT 2 FROM \"step_1\")"), "{out}");
        assert!(out.contains("\"step_2\" AS (SELECT 3 FROM \"alpha_1\")"), "{out}");
    }

    #[test]
    fn an_action_names_its_step_and_its_side_table_after_itself() {
        let out = planned("/@dataset:x/@test/bounds.json");
        assert!(out.contains("\"test_extent\" AS"), "the side table is not namespaced: {out}");
        assert!(out.contains("\"test_1\" AS"), "the step is not namespaced: {out}");
    }

    #[test]
    fn two_actions_keep_their_own_namespaces() {
        let mut grammar = test_action_grammar();
        grammar.register_action(Second);
        let out = planned_with("/@dataset:x/@test/bounds/@second/tally.json", grammar);
        for name in ["test_extent", "test_1", "second_extent", "second_1"] {
            assert!(out.contains(&format!("\"{name}\" AS")), "{name} is missing: {out}");
        }
        assert_eq!(out.matches("ST_Extent_Agg").count(), 2, "the actions shared a side table: {out}");
    }

    #[test]
    fn a_request_with_no_action_is_the_feature_pipeline() {
        let out = planned("/@dataset:x.geojson");
        assert!(out.contains("ST_AsGeoJSON"), "{out}");
        assert!(!out.contains("COUNT(*)"), "{out}");
    }

    #[test]
    fn an_action_adds_to_the_relation_and_the_output_still_frames_it() {
        let out = planned("/@dataset:x/@test/count.json");
        assert!(out.contains("COUNT(*) AS \"count\""), "{out}");
        assert!(out.contains("ST_AsGeoJSON"), "the output still supplies the terminal query: {out}");
    }

    #[test]
    fn option_expressions_arrive_in_url_order_under_canonical_names() {
        let out = planned("/@dataset:x/@test/n,b.json");
        let count_at = out.find("AS \"count\"").expect("count");
        let bounds_at = out.find("AS \"bounds\"").expect("bounds");
        assert!(count_at < bounds_at, "aliases must not reorder the parts: {out}");
    }

    #[test]
    fn a_fragment_may_add_a_side_table_that_options_share() {
        let out = planned("/@dataset:x/@test/b,width.json");
        assert_eq!(out.matches("\"test_extent\" AS").count(), 1, "{out}");
        assert!(out.contains("\"test_extent\""), "the side table must be joined, not dangling: {out}");
    }

    #[test]
    fn every_option_sees_the_same_relation() {
        let out = planned("/@dataset:x,id:7/@test/count,bounds,width.json");
        assert_eq!(out.matches("FROM \"step_1\"").count(), 2, "options disagreed about the input: {out}");
        assert_eq!(out.matches("\"test_extent\" AS").count(), 1, "the shared side table was built twice: {out}");
    }

    #[test]
    fn a_fragment_receives_its_parameters_in_order() {
        let out = planned("/@dataset:x/@test/three:name:eq:foo.json");
        assert!(out.contains("name|eq|foo AS \"three\""), "parameters arrived out of order: {out}");
    }

    #[test]
    fn a_bad_parameter_reports_its_own_position() {
        let grammar = typed_grammar();
        let columns = vec![("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse("/@dataset:x/@typed/pair:true:banana.json", &grammar).unwrap();
        let err = plan(&grammar, &parsed, "/x.geojson", &columns, None, backend()).unwrap_err();
        let typed = err.downcast_ref::<crate::grammar::ParamError>().expect("the kind must survive");
        assert_eq!(typed.at, 1, "the wrong parameter position was reported");
        assert_eq!(typed.got, "banana");
    }

    #[test]
    fn an_action_still_reprojects_a_source_that_is_not_wgs84() {
        let grammar = test_action_grammar();
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse("/@dataset:x/@test/count.json", &grammar).unwrap();
        let out = plan(&grammar, &parsed, "/x.geojson", &columns, Some("EPSG:25832"), backend()).unwrap();
        assert!(out.contains("ST_Transform"), "an action skipped reprojection: {out}");
        assert!(out.find("ST_Transform") < out.find("COUNT(*)"), "reprojection must precede the action: {out}");
    }

    #[test]
    fn a_filter_applies_before_the_action_runs() {
        let out = planned("/@dataset:x,id:7/@test/count.json");
        assert!(out.contains("\"step_1\" AS (SELECT * FROM \"source\" WHERE"), "{out}");
        assert!(out.contains("COUNT(*) AS \"count\" FROM \"step_1\""), "the action must read the filtered rows: {out}");
    }

    #[test]
    fn a_bad_action_parameter_surfaces_as_a_typed_error() {
        let grammar = typed_grammar();
        let columns = vec![("geom".to_string(), "GEOMETRY".to_string())];
        let parsed = crate::url::parse("/@dataset:x/@typed/flag:banana.json", &grammar).unwrap();
        let err = plan(&grammar, &parsed, "/x.geojson", &columns, None, backend()).unwrap_err();
        let typed = err.downcast_ref::<crate::grammar::ParamError>().expect("the kind must survive");
        assert_eq!(typed.expected, crate::grammar::Param::Boolean);
        assert_eq!(typed.got, "banana");
    }

    #[test]
    fn a_step_names_itself_and_becomes_the_input() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns, backend()).unwrap();
        p.step("step", Query::select().expr(Expr::cust("1")).from(p.input()).take());
        p.step("step", Query::select().expr(Expr::cust("2")).from(p.input()).take());
        let out = rendered(p);
        assert!(out.contains("\"step_1\" AS (SELECT 1 FROM \"source\")"), "{out}");
        assert!(out.contains("\"step_2\" AS (SELECT 2 FROM \"step_1\")"), "{out}");
        assert!(out.trim_end().ends_with("SELECT * FROM \"step_2\""), "{out}");
    }

    #[test]
    fn a_side_table_is_namespaced_shared_and_does_not_advance_the_input() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns, backend()).unwrap();
        assert!(p.cte("test", "extent").is_none());
        p.cte_once("test", "extent", |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
        p.cte_once("test", "extent", |_| panic!("a repeated side table must not be rebuilt"));
        assert!(p.cte("test", "extent").is_some());
        let out = rendered(p);
        assert_eq!(out.matches("\"test_extent\" AS").count(), 1, "the side table was emitted twice: {out}");
        assert!(out.trim_end().ends_with("SELECT * FROM \"source\""), "a side table advanced the input: {out}");
    }

    #[test]
    fn a_side_table_lookup_names_the_alias_built_over_the_current_input() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns, backend()).unwrap();
        p.cte_once("test", "extent", |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
        p.filter(&id_filter(), &columns).unwrap();
        assert!(p.cte("test", "extent").is_none(), "a side table from an earlier stage must not be reported as live");
        p.cte_once("test", "extent", |from| Query::select().expr(Expr::cust("2")).from(from.clone()).take());

        let live = p.cte("test", "extent").expect("a side table was built over the current input");
        let out = Query::select().expr(Expr::cust("*")).from(live).to_owned().to_string(PostgresQueryBuilder);
        assert!(out.ends_with("FROM \"test_extent_1\""), "the lookup named the stale alias: {out}");
    }

    #[rstest]
    #[case(&["a", "a", "a_1"])]
    #[case(&["a_1", "a", "a"])]
    fn a_side_table_never_reuses_an_alias_another_one_took(#[case] names: &[&str]) {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns, backend()).unwrap();
        for (at, name) in names.iter().enumerate() {
            if at > 0 {
                p.filter(&id_filter(), &columns).unwrap();
            }
            p.cte_once("test", name, |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
            assert!(p.cte("test", name).is_some(), "{name} was built but is not live");
        }
        let out = rendered(p);
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
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns, backend()).unwrap();
        p.cte_once("test", "extent", |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
        p.filter(&id_filter(), &columns).unwrap();
        p.cte_once("test", "extent", |from| Query::select().expr(Expr::cust("2")).from(from.clone()).take());
        let out = rendered(p);
        assert!(out.contains("\"test_extent\" AS (SELECT 1 FROM \"source\")"), "{out}");
        assert!(
            out.contains("\"test_extent_1\" AS (SELECT 2 FROM \"step_1\")"),
            "a stale side table was reused: {out}"
        );
    }

    #[test]
    fn a_side_table_reads_from_the_current_input() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns, backend()).unwrap();
        p.filter(&id_filter(), &columns).unwrap();
        p.cte_once("test", "extent", |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
        let out = rendered(p);
        assert!(out.contains("\"test_extent\" AS (SELECT 1 FROM \"step_1\")"), "{out}");
    }

    #[test]
    fn the_source_relation_carries_the_reader_and_its_encoding() {
        let columns = geom_columns();
        let mut p = Pipeline::source("/x.geojson", "UTF-8", &columns, backend()).unwrap();
        p.step("step", Query::select().expr(Expr::cust("1")).from(p.input()).take());
        assert_eq!(
            rendered(p),
            "WITH \"source\" AS (SELECT * FROM ST_Read('/x.geojson', open_options=list_value('ENCODING=UTF-8')) AS \"src\") , \"step_1\" AS (SELECT 1 FROM \"source\") SELECT * FROM \"step_1\""
        );
    }

    #[test]
    fn reprojection_takes_the_step_after_the_filter() {
        let columns = geom_columns();
        let out = plan(
            &Grammar::core(),
            &parsed_for("/@dataset:x,id:7.geojson"),
            "/x.geojson",
            &columns,
            Some("EPSG:25832"),
            backend(),
        )
        .unwrap();
        assert!(out.contains("\"step_2\" AS (SELECT * REPLACE (ST_Transform("), "{out}");
        assert!(out.contains("FROM \"step_1\""), "{out}");
        assert!(
            out.contains("\"out_1\" AS (SELECT * REPLACE (ST_AsGeoJSON("),
            "the output did not namespace its step: {out}"
        );
        assert!(out.contains("to_json(\"out_1\")"), "the feature select read the wrong cte: {out}");
        assert!(out.trim_end().ends_with("FROM \"out_1\""), "the feature select read the wrong cte: {out}");
    }

    #[test]
    fn a_crs_already_in_wgs84_adds_no_reprojection_step() {
        let columns = geom_columns();
        let out = plan(
            &Grammar::core(),
            &parsed_for("/@dataset:x.geojson"),
            "/x.geojson",
            &columns,
            Some("EPSG:4326"),
            backend(),
        )
        .unwrap();
        assert!(!out.contains("ST_Transform"), "{out}");
        assert!(out.contains("\"out_1\" AS (SELECT * REPLACE (ST_AsGeoJSON("), "a pass-through step was added: {out}");
    }

    #[test]
    fn a_source_without_a_geometry_column_errors_rather_than_panics() {
        let columns = vec![("id".to_string(), "BIGINT".to_string())];
        let err = plan(&Grammar::core(), &parsed_for("/@dataset:x.geojson"), "/x.geojson", &columns, None, backend())
            .unwrap_err();
        assert!(err.downcast_ref::<query::NoGeometry>().is_some(), "{err:#}");
    }

    #[test]
    fn an_unfiltered_request_adds_no_filter_step() {
        let columns = vec![("name".to_string(), "VARCHAR".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let out = plan(&Grammar::core(), &parsed_for("/@dataset:x.geojson"), "/x.geojson", &columns, None, backend())
            .unwrap();
        assert!(!out.contains("WHERE"), "an empty filter list added a predicate: {out}");
    }

    #[test]
    fn a_filtered_request_is_the_first_numbered_step() {
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let out =
            plan(&Grammar::core(), &parsed_for("/@dataset:x,id:7.geojson"), "/x.geojson", &columns, None, backend())
                .unwrap();
        assert!(out.contains("\"step_1\" AS (SELECT * FROM \"source\" WHERE"), "{out}");
        assert!(out.contains("\"id\" = (7)"), "{out}");
    }
}
