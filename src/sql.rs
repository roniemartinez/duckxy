use crate::formats::Format;
use crate::url::Segment;
use crate::{filters, query};
use sea_query::{Alias, CommonTableExpression, Expr, Func, PostgresQueryBuilder, Query, SelectStatement, WithClause};

fn add_step(ctes: &mut WithClause, steps: &mut usize, query: SelectStatement) -> Alias {
    *steps += 1;
    let name = Alias::new(format!("step_{steps}"));
    ctes.cte(CommonTableExpression::new().query(query).table_name(name.clone()).to_owned());
    name
}

pub fn build_sql(
    source: &str,
    encoding: &str,
    filters: &[Segment],
    format: Format,
    columns: &[(String, String)],
    crs: Option<&str>,
) -> anyhow::Result<String> {
    let geometry = query::geometry_of(columns).ok_or(query::NoGeometry)?;
    let mut ctes = WithClause::new();
    let mut input = Alias::new("source");
    let mut steps = 0;
    ctes.cte(
        CommonTableExpression::new()
            .query(
                Query::select().expr(Expr::cust("*")).from_function(query::read_source(source, encoding), "src").take(),
            )
            .table_name(input.clone())
            .to_owned(),
    );

    if let Some(predicate) = filters::condition(filters, columns)? {
        let filtered = Query::select().expr(Expr::cust("*")).from(input.clone()).and_where(predicate).take();
        input = add_step(&mut ctes, &mut steps, filtered);
    }

    if let Some(from) = crs.filter(|c| format.requires_wgs84() && !matches!(*c, "EPSG:4326" | "OGC:CRS84")) {
        let reprojected = Query::select()
            .expr(Expr::cust_with_exprs(
                "* REPLACE ($1 AS $2)",
                [
                    Func::cust("ST_Transform")
                        .arg(Expr::col(Alias::new(geometry)))
                        .arg(from)
                        .arg("EPSG:4326")
                        .arg(Expr::cust("always_xy := true"))
                        .into(),
                    Expr::col(Alias::new(geometry)),
                ],
            ))
            .from(input.clone())
            .take();
        input = add_step(&mut ctes, &mut steps, reprojected);
    }

    let replaced = Query::select()
        .expr(Expr::cust_with_exprs(
            "* REPLACE ($1 AS $2)",
            [Func::cust("ST_AsGeoJSON").arg(Expr::col(Alias::new(geometry))).into(), Expr::col(Alias::new(geometry))],
        ))
        .from(input.clone())
        .take();
    input = add_step(&mut ctes, &mut steps, replaced);

    Ok(match format {
        Format::GeoJson | Format::Json => Query::select()
            .expr(Func::cast_as(
                Func::cust("json_object").args([
                    Expr::val("type"),
                    Expr::val("Feature"),
                    Expr::val("properties"),
                    Func::cust("json_merge_patch")
                        .arg(Func::cust("to_json").arg(Expr::col(input.clone())))
                        .arg(Func::cust("json_object").args([Expr::val(geometry), Expr::cust("NULL")]))
                        .into(),
                    Expr::val("geometry"),
                    Func::cast_as(Expr::col(Alias::new(geometry)), "JSON").into(),
                ]),
                "VARCHAR",
            ))
            .from(input)
            .to_owned()
            .with(ctes)
            .to_string(PostgresQueryBuilder),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
