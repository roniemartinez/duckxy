use crate::formats::Format;
use crate::url::Segment;
use crate::{filters, query};
use sea_query::{Alias, CommonTableExpression, Expr, Func, PostgresQueryBuilder, Query, SelectStatement, WithClause};

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
        if let Some(predicate) = filters::condition(filters, columns)? {
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
        if let Some((_, _, full)) = self.side.iter().find(|(n, over, _)| n == name && over == &self.input_name) {
            return Alias::new(full);
        }
        let seen = self.side.iter().filter(|(n, _, _)| n == name).count();
        let full = match seen {
            0 => format!("x_{name}"),
            n => format!("x_{name}_{n}"),
        };
        let alias = Alias::new(&full);
        let select = build(&self.input);
        self.ctes.cte(CommonTableExpression::new().query(select).table_name(alias.clone()).to_owned());
        self.side.push((name.to_string(), self.input_name.clone(), full));
        alias
    }

    pub fn has_cte(&self, name: &str) -> bool {
        self.side.iter().any(|(n, _, _)| n == name)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn geom_columns() -> Vec<(String, String)> {
        vec![("id".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())]
    }

    fn id_filter() -> Vec<Segment> {
        vec![Segment { name: "id".to_string(), params: vec!["7".to_string()] }]
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
        assert!(!p.has_cte("extent"));
        p.cte_once("extent", |from| Query::select().expr(Expr::cust("1")).from(from.clone()).take());
        p.cte_once("extent", |_| panic!("a repeated side table must not be rebuilt"));
        assert!(p.has_cte("extent"));
        let out = p.finish_raw();
        assert_eq!(out.matches("\"x_extent\" AS").count(), 1, "the side table was emitted twice: {out}");
        assert!(out.trim_end().ends_with("SELECT * FROM \"source\""), "a side table advanced the input: {out}");
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
