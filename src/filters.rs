use crate::grammar::{FromParam, Param};
use crate::parexp;
use crate::url::Segment;
use sea_query::{Alias, Expr, ExprTrait, Func, LikeExpr, Query, SelectStatement, SimpleExpr};
use std::fmt;

const ID_CANDIDATES: [&str; 4] = ["id", "fid", "gid", "objectid"];
pub const GEOMETRY_TYPES: [&str; 7] =
    ["POINT", "LINESTRING", "POLYGON", "MULTIPOINT", "MULTILINESTRING", "MULTIPOLYGON", "GEOMETRYCOLLECTION"];
const LINEAR_TYPES: [&str; 2] = ["LINESTRING", "MULTILINESTRING"];
const BOOLEAN: &[&[Param]] = &[&[Param::Boolean]];
const ID_SHAPE: &[&[Param]] = &[&[Param::Value], &[Param::Operator, Param::Value]];
const PROP_SHAPE: &[&[Param]] = &[&[Param::Column, Param::Value], &[Param::Column, Param::Operator, Param::Value]];
const TYPE_SHAPE: &[&[Param]] = &[&[Param::GeometryType]];

pub const CORE: &[FilterDef] = &[
    filter("id", "", ID_SHAPE, id),
    filter("prop", "", PROP_SHAPE, prop),
    filter("type", "", TYPE_SHAPE, geometry_type),
    filter("valid", "", BOOLEAN, valid),
    filter("empty", "", BOOLEAN, empty),
    filter("simple", "", BOOLEAN, simple),
    filter("closed", "", BOOLEAN, closed),
];

#[derive(Debug, PartialEq)]
pub enum FilterError {
    NoIdColumn,
    UnknownFilter(String),
    BadParams(String),
    ExpansionTooLarge(String),
    UnknownColumn(String),
    UnknownOperator(String),
    NotANumber(String),
    NotOneValue(String),
    NotABoolean(String),
    UnknownGeometryType(String),
}

enum NumericKind {
    Integer,
    Exact,
    Approximate,
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterError::NoIdColumn => {
                write!(f, "source has no id column, expected one of: {}", ID_CANDIDATES.join(", "))
            }
            FilterError::UnknownFilter(name) => write!(f, "unknown filter: {name}"),
            FilterError::BadParams(name) => write!(f, "filter has the wrong number of parameters: {name}"),
            FilterError::ExpansionTooLarge(pattern) => {
                write!(f, "pattern expands past the limit of {} values: {pattern}", parexp::MAX_EXPANSION)
            }
            FilterError::UnknownColumn(key) => write!(f, "source has no column: {key}"),
            FilterError::UnknownOperator(op) => write!(f, "unknown property operator: {op}"),
            FilterError::NotANumber(value) => write!(f, "a numeric column needs a numeric value: {value}"),
            FilterError::NotOneValue(pattern) => {
                write!(f, "this operator takes a single value, not a pattern: {pattern}")
            }
            FilterError::NotABoolean(value) => write!(f, "this filter takes true or false: {value}"),
            FilterError::UnknownGeometryType(value) => {
                write!(f, "unknown geometry type: {value}, expected one of: {}", GEOMETRY_TYPES.join(", "))
            }
        }
    }
}

impl std::error::Error for FilterError {}

pub struct FilterCtx<'a> {
    pipeline: &'a mut crate::sql::Pipeline,
    columns: &'a [(String, String)],
}

impl<'a> FilterCtx<'a> {
    pub fn new(pipeline: &'a mut crate::sql::Pipeline, columns: &'a [(String, String)]) -> Self {
        Self { pipeline, columns }
    }

    pub fn geometry(&self) -> &str {
        self.pipeline.geometry()
    }

    pub fn geom(&self) -> SimpleExpr {
        Expr::col(Alias::new(self.pipeline.geometry()))
    }

    pub fn columns(&self) -> &[(String, String)] {
        self.columns
    }

    pub fn column(&self, name: &str) -> Option<&(String, String)> {
        self.columns.iter().find(|(held, _)| held == name)
    }

    pub fn crs(&self) -> &str {
        self.pipeline.crs()
    }

    pub fn transform(&self, target: &str) -> SimpleExpr {
        self.pipeline.transform(target)
    }

    pub fn dialect(&self) -> &dyn crate::backend::Dialect {
        self.pipeline.dialect()
    }

    pub fn call(&self, name: &str, args: Vec<SimpleExpr>) -> Result<SimpleExpr, crate::backend::UnknownOp> {
        self.pipeline.backend().call(name, args)
    }

    pub fn has(&self, name: &str) -> bool {
        self.pipeline.backend().has(name)
    }

    pub fn cte_once(&mut self, name: &str, build: impl FnOnce(&Alias) -> SelectStatement) -> Alias {
        self.pipeline.cte_once("filter", name, build)
    }

    pub fn cte(&self, name: &str) -> Option<Alias> {
        self.pipeline.cte("filter", name)
    }
}

pub type FilterFn = fn(&mut FilterCtx, &[String]) -> anyhow::Result<SimpleExpr>;

#[derive(Clone, Copy)]
pub struct FilterDef {
    pub name: &'static str,
    pub short: &'static str,
    pub shapes: &'static [&'static [Param]],
    pub condition: FilterFn,
}

pub const fn filter(
    name: &'static str,
    short: &'static str,
    shapes: &'static [&'static [Param]],
    condition: FilterFn,
) -> FilterDef {
    FilterDef { name, short, shapes, condition }
}

impl FilterDef {
    pub fn matches(&self, name: &str) -> bool {
        self.name == name || (!self.short.is_empty() && self.short == name)
    }

    pub fn canonical(&self) -> &'static str {
        self.name
    }

    pub fn accepts(&self, params: usize) -> bool {
        self.shapes.iter().any(|shape| shape.len() == params)
    }
}

pub fn condition(
    grammar: &crate::grammar::Grammar,
    ctx: &mut FilterCtx,
    filters: &[Segment],
) -> anyhow::Result<Option<SimpleExpr>> {
    let mut combined: Option<SimpleExpr> = None;
    for held in filters {
        let Some(declared) = grammar.filter_for(&held.name) else {
            return Err(anyhow::Error::new(FilterError::UnknownFilter(held.name.clone())));
        };
        let next = (declared.condition)(ctx, &held.params)?;
        combined = Some(match combined {
            Some(existing) => existing.and(next),
            None => next,
        });
    }
    Ok(combined)
}

fn id(ctx: &mut FilterCtx, params: &[String]) -> anyhow::Result<SimpleExpr> {
    let column = ID_CANDIDATES
        .iter()
        .find_map(|want| ctx.columns().iter().find(|(name, _)| name.eq_ignore_ascii_case(want)))
        .ok_or(FilterError::NoIdColumn)?;
    Ok(compare(column, params, "id")?)
}

fn prop(ctx: &mut FilterCtx, params: &[String]) -> anyhow::Result<SimpleExpr> {
    let [key, rest @ ..] = params else { return Err(FilterError::BadParams("prop".to_string()).into()) };
    let column = ctx.column(key).ok_or_else(|| FilterError::UnknownColumn(key.clone()))?;
    Ok(compare(column, rest, "prop")?)
}

fn geometry_type(ctx: &mut FilterCtx, params: &[String]) -> anyhow::Result<SimpleExpr> {
    Ok(shape_of(ctx.geometry()).eq(named_type(only("type", params)?)?))
}

fn valid(ctx: &mut FilterCtx, params: &[String]) -> anyhow::Result<SimpleExpr> {
    Ok(predicate("ST_IsValid", ctx).eq(boolean(only("valid", params)?)?))
}

fn empty(ctx: &mut FilterCtx, params: &[String]) -> anyhow::Result<SimpleExpr> {
    Ok(predicate("ST_IsEmpty", ctx).eq(boolean(only("empty", params)?)?))
}

fn simple(ctx: &mut FilterCtx, params: &[String]) -> anyhow::Result<SimpleExpr> {
    Ok(predicate("ST_IsSimple", ctx).eq(boolean(only("simple", params)?)?))
}

fn closed(ctx: &mut FilterCtx, params: &[String]) -> anyhow::Result<SimpleExpr> {
    let linear: SimpleExpr =
        Expr::case(shape_of(ctx.geometry()).is_in(LINEAR_TYPES), predicate("ST_IsClosed", ctx)).into();
    Ok(linear.eq(boolean(only("closed", params)?)?))
}

fn predicate(function: &'static str, ctx: &FilterCtx) -> SimpleExpr {
    Func::cust(function).arg(ctx.geom()).into()
}

fn only<'a>(name: &str, params: &'a [String]) -> Result<&'a str, FilterError> {
    match params {
        [single] => Ok(single.as_str()),
        _ => Err(FilterError::BadParams(name.to_string())),
    }
}

fn numeric_kind(kind: &str) -> Option<NumericKind> {
    let (name, width) = kind.split_once('(').unwrap_or((kind, ""));
    if !width.is_empty() && !width.ends_with(')') {
        return None;
    }
    match name {
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT" | "USMALLINT" | "UINTEGER"
        | "UBIGINT" | "UHUGEINT" => Some(NumericKind::Integer),
        "DECIMAL" | "NUMERIC" => Some(NumericKind::Exact),
        "FLOAT" | "DOUBLE" | "REAL" => Some(NumericKind::Approximate),
        _ => None,
    }
}

fn text(value: SimpleExpr) -> SimpleExpr {
    Func::cast_as(value, Alias::new("VARCHAR")).into()
}

fn boolean(value: &str) -> Result<bool, FilterError> {
    crate::grammar::Boolean::from_param(value)
        .map(|parsed| parsed.0)
        .ok_or_else(|| FilterError::NotABoolean(value.to_string()))
}

fn named_type(value: &str) -> Result<&'static str, FilterError> {
    let wanted = value.to_ascii_uppercase();
    GEOMETRY_TYPES
        .iter()
        .find(|known| **known == wanted)
        .copied()
        .ok_or_else(|| FilterError::UnknownGeometryType(value.to_string()))
}

fn shape_of(geometry: &str) -> SimpleExpr {
    text(Func::cust("ST_GeometryType").arg(Expr::col(Alias::new(geometry))).into())
}

fn numeral(value: &str) -> Option<&str> {
    let body = value.strip_prefix('-').unwrap_or(value);
    let digits = body.chars().filter(|c| c.is_ascii_digit()).count();
    let shaped = body.chars().all(|c| c.is_ascii_digit() || c == '.') && body.matches('.').count() <= 1;
    (digits > 0 && shaped).then_some(value)
}

fn compare((name, kind): &(String, String), params: &[String], filter: &str) -> Result<SimpleExpr, FilterError> {
    let column = || Expr::col(Alias::new(name.as_str()));
    let (operator, raw) = match params {
        [only] if only == "null" || only == "notnull" => (only.as_str(), ""),
        [only] => ("eq", only.as_str()),
        [named, supplied] => (named.as_str(), supplied.as_str()),
        _ => return Err(FilterError::BadParams(filter.to_string())),
    };
    let literal = || match parexp::count(raw) {
        1 => Ok(parexp::expand(raw).pop().unwrap_or_default()),
        _ => Err(FilterError::NotOneValue(raw.to_string())),
    };

    Ok(match operator {
        "null" => column().is_null(),
        "notnull" => column().is_not_null(),
        "eq" | "in" => value_set(column, kind, raw)?,
        "ne" | "nin" => value_set(column, kind, raw)?.not(),
        "ieq" => Func::lower(text(column())).eq(Func::lower(Expr::val(literal()?))),
        "gt" | "gte" | "lt" | "lte" => ordering(column, kind, operator, &literal()?)?,
        "sw" | "ew" | "ct" => text(column()).like(LikeExpr::new(anchor(operator, &literal()?)).escape('\\')),
        "isw" | "iew" | "ict" => Expr::cust_with_exprs(
            "LOWER($1) LIKE LOWER($2) ESCAPE '\\'",
            [text(column()), Expr::val(anchor(&operator[1..], &literal()?))],
        ),
        other => return Err(FilterError::UnknownOperator(other.to_string())),
    })
}

fn value_set(column: impl Fn() -> SimpleExpr, kind: &str, pattern: &str) -> Result<SimpleExpr, FilterError> {
    let numeric = numeric_kind(kind).is_some();
    if numeric && let Some((lo, hi)) = parexp::as_integer_range(pattern) {
        return Ok(column().between(lo, hi));
    }
    if parexp::count(pattern) > parexp::MAX_EXPANSION {
        return Err(FilterError::ExpansionTooLarge(pattern.to_string()));
    }
    let values = parexp::expand(pattern);
    if numeric && let Some(bad) = values.iter().find(|value| numeral(value).is_none()) {
        return Err(FilterError::NotANumber(bad.clone()));
    }
    let compared = || match numeric {
        true => column(),
        false => text(column()),
    };
    if let [only] = values.as_slice() {
        return Ok(match numeric {
            true => compared().eq(Expr::cust(only.to_string())),
            false => compared().eq(only.as_str()),
        });
    }
    let member = || match numeric {
        true => Expr::cust_with_exprs(format!("TRY_CAST($1 AS {kind})"), [Expr::col(Alias::new("v"))]),
        false => Expr::col(Alias::new("v")),
    };
    let expansion = Query::select()
        .expr(member())
        .from_function(Func::cust("parexp").arg(pattern), Alias::new("expanded"))
        .and_where_option(numeric.then(|| member().is_not_null()))
        .take();
    Ok(compared().in_subquery(expansion))
}

fn ordering(
    column: impl Fn() -> SimpleExpr,
    kind: &str,
    operator: &str,
    value: &str,
) -> Result<SimpleExpr, FilterError> {
    let (left, right) = match numeric_kind(kind) {
        None => (text(column()), Expr::val(value)),
        Some(_) => {
            let exact = numeral(value).ok_or_else(|| FilterError::NotANumber(value.to_string()))?;
            (column(), Expr::cust(exact.to_string()))
        }
    };
    Ok(match operator {
        "gt" => left.gt(right),
        "gte" => left.gte(right),
        "lt" => left.lt(right),
        _ => left.lte(right),
    })
}

fn anchor(operator: &str, value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    match operator {
        "sw" => format!("{escaped}%"),
        "ew" => format!("%{escaped}"),
        _ => format!("%{escaped}%"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_filter_the_core_grammar_registers_is_executable() {
        let columns = columns();
        for def in &crate::grammar::Grammar::core().filters {
            for shape in def.shapes {
                let segment = Segment { name: def.name.to_string(), params: vec!["name".to_string(); shape.len()] };
                let outcome = held(&[segment], &columns);
                assert!(
                    !matches!(outcome, Err(FilterError::UnknownFilter(_))),
                    "{:?} is registered but condition cannot execute it",
                    def.name
                );
            }
        }
    }
    use rstest::rstest;
    use sea_query::{PostgresQueryBuilder, Query};

    fn columns() -> Vec<(String, String)> {
        vec![
            ("id".to_string(), "BIGINT".to_string()),
            ("name".to_string(), "VARCHAR".to_string()),
            ("geom".to_string(), "GEOMETRY".to_string()),
        ]
    }

    fn pipeline_for(cols: &[(String, String)]) -> crate::sql::Pipeline {
        let backend = std::sync::Arc::new(crate::backend::Backend::default());
        crate::sql::Pipeline::source("/x.geojson", "UTF-8", cols, None, backend).unwrap()
    }

    fn held(filters: &[Segment], cols: &[(String, String)]) -> Result<Option<SimpleExpr>, FilterError> {
        let mut pipeline = pipeline_for(cols);
        let mut ctx = FilterCtx::new(&mut pipeline, cols);
        crate::filters::condition(&crate::grammar::Grammar::core(), &mut ctx, filters)
            .map_err(|e| e.downcast::<FilterError>().expect("a core filter reports a FilterError"))
    }

    fn seg(params: &[&str]) -> Segment {
        Segment { name: "id".to_string(), params: params.iter().map(|p| p.to_string()).collect() }
    }

    fn render(filters: &[Segment], cols: &[(String, String)]) -> String {
        let expr = held(filters, cols).unwrap().unwrap();
        Query::select().expr(Expr::cust("1")).and_where(expr).to_string(PostgresQueryBuilder)
    }

    fn sql(filters: &[Segment]) -> String {
        render(filters, &columns())
    }

    fn text_sql(filters: &[Segment]) -> String {
        let cols = vec![("id".to_string(), "VARCHAR".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        render(filters, &cols)
    }

    fn executes(params: &[&str], kind: &str, row: &str) -> Result<bool, String> {
        let cols = vec![("v".to_string(), kind.to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let seg = Segment { name: "prop".to_string(), params: params.iter().map(|p| p.to_string()).collect() };
        let expr = held(&[seg], &cols).map_err(|e| e.to_string())?.unwrap();
        let sql = Query::select()
            .expr(Expr::cust("1"))
            .from_subquery(Query::select().expr(Expr::cust(row.to_string())).take(), Alias::new("t"))
            .and_where(expr)
            .to_string(PostgresQueryBuilder);
        crate::ensure_spatial();
        let conn = duckdb::Connection::open_in_memory().map_err(|e| e.to_string())?;
        conn.register_table_function::<crate::parexp::Parexp>("parexp").map_err(|e| e.to_string())?;
        conn.prepare(&sql)
            .and_then(|mut st| st.query([]).and_then(|mut r| r.next().map(|found| found.is_some())))
            .map_err(|e| format!("{}  ||  {sql}", e.to_string().lines().next().unwrap_or("")))
    }

    fn shaped(name: &str, value: &str, wkt: &str) -> Result<bool, String> {
        let cols = vec![("geom".to_string(), "GEOMETRY".to_string())];
        let seg = Segment { name: name.to_string(), params: vec![value.to_string()] };
        let expr = held(&[seg], &cols).map_err(|e| e.to_string())?.unwrap();
        let geom = match wkt {
            "NULL" => "NULL::GEOMETRY AS geom".to_string(),
            _ => format!("ST_GeomFromText('{wkt}') AS geom"),
        };
        let row = Query::select().expr(Expr::cust(geom)).take();
        let sql = Query::select()
            .expr(Expr::cust("1"))
            .from_subquery(row, Alias::new("t"))
            .and_where(expr)
            .to_string(PostgresQueryBuilder);
        crate::ensure_spatial();
        let conn = duckdb::Connection::open_in_memory().map_err(|e| e.to_string())?;
        conn.execute_batch("LOAD spatial;").map_err(|e| e.to_string())?;
        conn.prepare(&sql)
            .and_then(|mut st| st.query([]).and_then(|mut r| r.next().map(|found| found.is_some())))
            .map_err(|e| format!("{}  ||  {sql}", e.to_string().lines().next().unwrap_or("")))
    }

    const POINT: &str = "POINT(1 2)";
    const OPEN_LINE: &str = "LINESTRING(0 0,1 1)";
    const SHUT_LINE: &str = "LINESTRING(0 0,1 0,1 1,0 0)";
    const BOWTIE: &str = "POLYGON((0 0,1 1,1 0,0 1,0 0))";

    #[rstest]
    #[case("type", "Point", POINT, true)]
    #[case("type", "point", POINT, true)]
    #[case("type", "POINT", POINT, true)]
    #[case("type", "LineString", POINT, false)]
    #[case("type", "LineString", OPEN_LINE, true)]
    #[case("type", "Polygon", BOWTIE, true)]
    #[case("valid", "true", POINT, true)]
    #[case("valid", "true", BOWTIE, false)]
    #[case("valid", "false", BOWTIE, true)]
    #[case("valid", "1", POINT, true)]
    #[case("valid", "0", POINT, false)]
    #[case("empty", "true", "POINT EMPTY", true)]
    #[case("empty", "false", POINT, true)]
    #[case("empty", "true", POINT, false)]
    #[case("simple", "true", OPEN_LINE, true)]
    #[case("simple", "false", "LINESTRING(0 0,1 1,1 0,0 1)", true)]
    #[case("closed", "true", SHUT_LINE, true)]
    #[case("closed", "false", SHUT_LINE, false)]
    #[case("closed", "false", OPEN_LINE, true)]
    #[case("closed", "true", OPEN_LINE, false)]
    fn every_geometry_predicate_executes(
        #[case] name: &str,
        #[case] value: &str,
        #[case] wkt: &str,
        #[case] expected: bool,
    ) {
        match shaped(name, value, wkt) {
            Ok(matched) => assert_eq!(matched, expected, "{name}:{value} against {wkt}"),
            Err(e) => panic!("{name}:{value} against {wkt} produced invalid sql: {e}"),
        }
    }

    #[rstest]
    #[case("type", "Point", "POINT Z (1 2 3)", true)]
    #[case("type", "LineString", "LINESTRING Z (0 0 0,1 1 1)", true)]
    #[case("valid", "true", "POINT Z (1 2 3)", true)]
    #[case("closed", "false", "LINESTRING Z (0 0 0,1 1 1)", true)]
    fn a_third_dimension_does_not_change_the_type_name(
        #[case] name: &str,
        #[case] value: &str,
        #[case] wkt: &str,
        #[case] expected: bool,
    ) {
        match shaped(name, value, wkt) {
            Ok(matched) => assert_eq!(matched, expected, "{name}:{value} against {wkt}"),
            Err(e) => panic!("{name}:{value} against {wkt} produced invalid sql: {e}"),
        }
    }

    #[rstest]
    #[case("true", POINT)]
    #[case("false", POINT)]
    #[case("true", BOWTIE)]
    #[case("false", BOWTIE)]
    #[case("true", "GEOMETRYCOLLECTION(POINT(1 2))")]
    fn closed_excludes_a_geometry_that_cannot_be_closed(#[case] value: &str, #[case] wkt: &str) {
        match shaped("closed", value, wkt) {
            Ok(matched) => assert!(!matched, "closed:{value} matched {wkt}"),
            Err(e) => panic!("closed:{value} against {wkt} produced invalid sql: {e}"),
        }
    }

    fn counted(name: &str, value: &str) -> Result<i64, String> {
        let cols = vec![("geom".to_string(), "GEOMETRY".to_string())];
        let seg = Segment { name: name.to_string(), params: vec![value.to_string()] };
        let expr = held(&[seg], &cols).map_err(|e| e.to_string())?.unwrap();
        let sql = Query::select()
            .expr(Expr::cust("count(*)"))
            .from(Alias::new("t"))
            .and_where(expr)
            .to_string(PostgresQueryBuilder);
        crate::ensure_spatial();
        let conn = duckdb::Connection::open_in_memory().map_err(|e| e.to_string())?;
        let rows = [POINT, BOWTIE, SHUT_LINE, OPEN_LINE, "GEOMETRYCOLLECTION(POINT(1 2))"]
            .map(|wkt| format!("(ST_GeomFromText('{wkt}'))"))
            .join(",");
        conn.execute_batch(&format!("LOAD spatial; CREATE TABLE t AS SELECT * FROM (VALUES {rows}) AS v(geom);"))
            .map_err(|e| e.to_string())?;
        conn.query_row(&sql, [], |r| r.get(0))
            .map_err(|e| format!("{}  ||  {sql}", e.to_string().lines().next().unwrap_or("")))
    }

    #[rstest]
    #[case("closed", "true", 1)]
    #[case("closed", "false", 1)]
    #[case("valid", "true", 4)]
    #[case("valid", "false", 1)]
    #[case("simple", "true", 4)]
    #[case("simple", "false", 1)]
    #[case("type", "Point", 1)]
    #[case("type", "LineString", 2)]
    fn a_predicate_holds_over_a_table_of_mixed_geometries(
        #[case] name: &str,
        #[case] value: &str,
        #[case] expected: i64,
    ) {
        match counted(name, value) {
            Ok(found) => assert_eq!(found, expected, "{name}:{value}"),
            Err(e) => panic!("{name}:{value} produced invalid sql: {e}"),
        }
    }

    #[rstest]
    #[case("valid", "true")]
    #[case("valid", "false")]
    #[case("empty", "true")]
    #[case("empty", "false")]
    #[case("simple", "false")]
    #[case("closed", "false")]
    #[case("type", "Point")]
    fn a_null_geometry_never_matches_a_predicate(#[case] name: &str, #[case] value: &str) {
        match shaped(name, value, "NULL") {
            Ok(matched) => assert!(!matched, "{name}:{value} matched a null geometry"),
            Err(e) => panic!("{name}:{value} produced invalid sql: {e}"),
        }
    }

    #[rstest]
    #[case("valid", "yes", FilterError::NotABoolean("yes".to_string()))]
    #[case("empty", "", FilterError::NotABoolean(String::new()))]
    #[case("simple", "2", FilterError::NotABoolean("2".to_string()))]
    #[case("closed", "maybe", FilterError::NotABoolean("maybe".to_string()))]
    #[case("type", "Banana", FilterError::UnknownGeometryType("Banana".to_string()))]
    #[case("type", "ST_Point", FilterError::UnknownGeometryType("ST_Point".to_string()))]
    fn a_geometry_predicate_refuses_a_value_it_cannot_read(
        #[case] name: &str,
        #[case] value: &str,
        #[case] expected: FilterError,
    ) {
        let seg = Segment { name: name.to_string(), params: vec![value.to_string()] };
        assert_eq!(held(&[seg], &columns()).unwrap_err(), expected);
    }

    #[rstest]
    #[case(&["v", "eq", "Alpha"], true)]
    #[case(&["v", "ne", "Alpha"], false)]
    #[case(&["v", "ieq", "alpha"], true)]
    #[case(&["v", "sw", "Al"], true)]
    #[case(&["v", "ew", "ha"], true)]
    #[case(&["v", "ct", "lph"], true)]
    #[case(&["v", "isw", "al"], true)]
    #[case(&["v", "iew", "HA"], true)]
    #[case(&["v", "ict", "LPH"], true)]
    #[case(&["v", "ict", "zzz"], false)]
    #[case(&["v", "gt", "A"], true)]
    #[case(&["v", "lt", "Z"], true)]
    #[case(&["v", "in", "(Alpha,Beta)"], true)]
    #[case(&["v", "nin", "(Beta)"], true)]
    #[case(&["v", "notnull"], true)]
    fn every_text_operator_executes(#[case] params: &[&str], #[case] expected: bool) {
        match executes(params, "VARCHAR", "'Alpha' AS v") {
            Ok(matched) => assert_eq!(matched, expected, "{params:?}"),
            Err(e) => panic!("{params:?} produced invalid sql: {e}"),
        }
    }

    #[rstest]
    #[case("BIGINT", "42::BIGINT AS v", &["v", "gt", "1"], true)]
    #[case("BIGINT", "42::BIGINT AS v", &["v", "lt", "1"], false)]
    #[case("BIGINT", "9007199254740993::BIGINT AS v", &["v", "gt", "9007199254740992"], true)]
    #[case("HUGEINT", "42::HUGEINT AS v", &["v", "gt", "1"], true)]
    #[case("UBIGINT", "18446744073709551615::UBIGINT AS v", &["v", "gt", "0"], true)]
    #[case("DOUBLE", "1.5::DOUBLE AS v", &["v", "gt", "1"], true)]
    #[case("DECIMAL(38,0)", "99999999999999999999999999999999999999::DECIMAL(38,0) AS v", &["v", "gt", "0"], true)]
    #[case("DECIMAL(18,3)", "1.5::DECIMAL(18,3) AS v", &["v", "lt", "2"], true)]
    fn every_numeric_column_executes(
        #[case] kind: &str,
        #[case] row: &str,
        #[case] params: &[&str],
        #[case] expected: bool,
    ) {
        match executes(params, kind, row) {
            Ok(matched) => assert_eq!(matched, expected, "{kind} {params:?}"),
            Err(e) => panic!("{kind} {params:?} produced invalid sql: {e}"),
        }
    }

    fn nearby(ctx: &mut FilterCtx, params: &[String]) -> anyhow::Result<SimpleExpr> {
        ctx.cte_once("reach", |from| Query::select().expr(Expr::cust("ST_Union_Agg(geom)")).from(from.clone()).take());
        let reach = Query::select().expr(Expr::cust("*")).from(Alias::new("filter_reach")).take();
        let measured = ctx.call("as_text", vec![ctx.geom()])?;
        Ok(Expr::cust_with_exprs(
            format!("ST_Whatever($1, $2, '{}', {})", ctx.crs(), params[0]),
            [measured, SimpleExpr::SubQuery(None, Box::new(sea_query::SubQueryStatement::SelectStatement(reach)))],
        ))
    }

    #[test]
    fn a_filter_core_never_named_can_be_registered() {
        let mut g = crate::grammar::Grammar::default();
        g.register_filter(filter("nearby", "nb", &[&[Param::Value]], nearby));
        g.register_output(crate::formats::GeoJson);
        let cols = columns();
        let backend = crate::backend::Backend::default().extend(|v| {
            v.register("as_text", |mut args: Vec<SimpleExpr>| Func::cast_as(args.remove(0), "VARCHAR").into());
        });
        let mut pipeline =
            crate::sql::Pipeline::source("/x.geojson", "UTF-8", &cols, None, std::sync::Arc::new(backend)).unwrap();
        let segment = Segment { name: "nearby".to_string(), params: vec!["500".to_string()] };
        let expr = {
            let mut ctx = FilterCtx::new(&mut pipeline, &cols);
            crate::filters::condition(&g, &mut ctx, &[segment]).unwrap().unwrap()
        };
        assert!(pipeline.cte("filter", "reach").is_some(), "the side table was not kept on the pipeline");
        let input = pipeline.input();
        let rendered = pipeline.finish(Query::select().expr(Expr::cust("*")).from(input).and_where(expr).take());
        assert!(rendered.contains("ST_Whatever"), "the registered filter did not reach the sql: {rendered}");
        assert!(rendered.contains("EPSG:4326"), "the filter could not read the pipeline crs: {rendered}");
        assert!(
            rendered.contains("CAST(\"geom\" AS VARCHAR)"),
            "the filter could not reach the vocabulary: {rendered}"
        );
        assert!(
            rendered.contains("\"filter_reach\" AS (SELECT ST_Union_Agg(geom)"),
            "the side table was not built from the filter's own query: {rendered}"
        );
    }

    #[test]
    fn no_filters_is_no_condition() {
        assert!(held(&[], &columns()).unwrap().is_none());
    }

    #[rstest]
    #[case("7", "'7'")]
    #[case("CA", "'CA'")]
    #[case("02257", "'02257'")]
    #[case("inf", "'inf'")]
    #[case("NaN", "'NaN'")]
    #[case("1e400", "'1e400'")]
    #[case("9007199254740993", "'9007199254740993'")]
    fn every_value_is_compared_as_exact_text(#[case] value: &str, #[case] literal: &str) {
        let out = text_sql(&[seg(&[value])]);
        assert!(out.contains(&format!("CAST(\"id\" AS VARCHAR) = {literal}")), "{out}");
        assert!(!out.contains("TRY_CAST"), "a value took a numeric path: {out}");
        assert!(!out.contains(" OR "), "an id lookup must match one spelling only: {out}");
    }

    #[rstest]
    #[case("BIGINT", "1::BIGINT AS v", &["v", "eq", "01"], true)]
    #[case("BIGINT", "1::BIGINT AS v", &["v", "ne", "01"], false)]
    #[case("BIGINT", "1::BIGINT AS v", &["v", "in", "(01,99)"], true)]
    #[case("BIGINT", "1::BIGINT AS v", &["v", "nin", "(01,99)"], false)]
    #[case("BIGINT", "1::BIGINT AS v", &["v", "nin", "(2,99)"], true)]
    #[case("UBIGINT", "18446744073709551615::UBIGINT AS v", &["v", "eq", "18446744073709551615"], true)]
    #[case("DECIMAL(18,3)", "1.5::DECIMAL(18,3) AS v", &["v", "eq", "1.5"], true)]
    #[case("DECIMAL(18,3)", "1.5::DECIMAL(18,3) AS v", &["v", "eq", "1.500"], true)]
    #[case("DECIMAL(18,3)", "1.5::DECIMAL(18,3) AS v", &["v", "in", "(1.5,9)"], true)]
    #[case("DOUBLE", "1.5::DOUBLE AS v", &["v", "eq", "1.50"], true)]
    #[case("BIGINT", "1::BIGINT AS v", &["v", "eq", "2"], false)]
    #[case("BIGINT", "1::BIGINT AS v", &["v", "in", "(2,99)"], false)]
    fn a_numeric_column_compares_by_value_not_by_spelling(
        #[case] kind: &str,
        #[case] row: &str,
        #[case] params: &[&str],
        #[case] expected: bool,
    ) {
        match executes(params, kind, row) {
            Ok(matched) => assert_eq!(matched, expected, "{kind} {params:?}"),
            Err(e) => panic!("{kind} {params:?} produced invalid sql: {e}"),
        }
    }

    #[rstest]
    #[case("DECIMAL(18,3)[]", "[1.5]::DECIMAL(18,3)[] AS v", &["v", "eq", "1.5"])]
    #[case("DECIMAL(18,3)[]", "[1.5]::DECIMAL(18,3)[] AS v", &["v", "gt", "1"])]
    #[case("DECIMAL(18,3)[]", "[1.5]::DECIMAL(18,3)[] AS v", &["v", "in", "(1..3)"])]
    #[case("BIGINT[]", "[1]::BIGINT[] AS v", &["v", "eq", "1"])]
    #[case("STRUCT(a DECIMAL(18,3))", "{'a': 1.5}::STRUCT(a DECIMAL(18,3)) AS v", &["v", "eq", "1.5"])]
    fn a_column_that_merely_contains_a_number_is_not_numeric(
        #[case] kind: &str,
        #[case] row: &str,
        #[case] p: &[&str],
    ) {
        match executes(p, kind, row) {
            Ok(_) => {}
            Err(e) => panic!("{kind} {p:?} produced invalid sql: {e}"),
        }
    }

    #[rstest]
    #[case(&["v", "null"], true)]
    #[case(&["v", "notnull"], false)]
    #[case(&["v", "eq", "alpha"], false)]
    #[case(&["v", "ne", "alpha"], false)]
    #[case(&["v", "in", "(alpha,beta)"], false)]
    #[case(&["v", "nin", "(alpha,beta)"], false)]
    fn a_null_never_matches_a_comparison(#[case] params: &[&str], #[case] expected: bool) {
        match executes(params, "VARCHAR", "NULL::VARCHAR AS v") {
            Ok(matched) => assert_eq!(matched, expected, "{params:?}"),
            Err(e) => panic!("{params:?} produced invalid sql: {e}"),
        }
    }

    #[test]
    fn a_value_past_the_column_range_is_no_match_not_an_error() {
        let too_big = &["v", "nin", "(999999999999999999999999,2)"];
        assert_eq!(executes(too_big, "BIGINT", "1::BIGINT AS v"), Ok(true));
    }

    #[rstest]
    #[case("CA")]
    #[case("' OR 1=1 --")]
    #[case("1e400")]
    #[case("(a,b)")]
    fn a_numeric_column_refuses_a_value_that_is_not_a_number(#[case] value: &str) {
        let outcome = held(&[seg(&[value])], &columns());
        assert!(matches!(outcome, Err(FilterError::NotANumber(_))), "{outcome:?}");
    }

    #[rstest]
    #[case("~Armagh City, Banbridge~", "'Armagh City, Banbridge'")]
    #[case("~a:b~", "'a:b'")]
    #[case("plain", "'plain'")]
    fn tilde_quoting_is_stripped(#[case] value: &str, #[case] needle: &str) {
        assert!(text_sql(&[seg(&[value])]).contains(needle));
    }

    #[rstest]
    #[case(vec![("fid".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())], true)]
    #[case(vec![("GID".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())], true)]
    #[case(vec![("ObjectID".to_string(), "BIGINT".to_string()), ("geom".to_string(), "GEOMETRY".to_string())], true)]
    #[case(vec![("name".to_string(), "VARCHAR".to_string()), ("geom".to_string(), "GEOMETRY".to_string())], false)]
    fn id_finds_its_column_case_insensitively(#[case] cols: Vec<(String, String)>, #[case] found: bool) {
        let outcome = held(&[seg(&["1"])], &cols);
        assert_eq!(outcome.is_ok(), found, "{outcome:?}");
        if !found {
            assert_eq!(outcome.unwrap_err(), FilterError::NoIdColumn);
        }
    }

    #[test]
    fn id_prefers_id_over_the_other_candidates() {
        let cols = vec![
            ("fid".to_string(), "BIGINT".to_string()),
            ("id".to_string(), "BIGINT".to_string()),
            ("geom".to_string(), "GEOMETRY".to_string()),
        ];
        let out = render(&[seg(&["1"])], &cols);
        assert!(out.contains("\"id\""), "{out}");
        assert!(!out.contains("\"fid\""), "{out}");
    }

    #[test]
    fn two_filters_are_and_chained() {
        let out = sql(&[seg(&["1"]), seg(&["2"])]);
        assert!(out.contains(" AND "), "{out}");
    }

    #[rstest]
    #[case(&[])]
    #[case(&["a", "b", "c"])]
    fn id_rejects_a_bad_parameter_count(#[case] params: &[&str]) {
        assert_eq!(held(&[seg(params)], &columns()).unwrap_err(), FilterError::BadParams("id".to_string()));
    }

    #[rstest]
    #[case(&["gte", "500"], "\"id\" >= (500)")]
    #[case(&["lt", "1000"], "\"id\" < (1000)")]
    #[case(&["in", "(500..1000)"], "BETWEEN 500 AND 1000")]
    #[case(&["(50..100)"], "BETWEEN 50 AND 100")]
    #[case(&["ct", "ab"], "LIKE")]
    fn id_accepts_the_same_operators_as_prop(#[case] params: &[&str], #[case] needle: &str) {
        let out = sql(&[seg(params)]);
        assert!(out.contains(needle), "{out}");
    }

    #[rstest]
    #[case("(1,2)")]
    #[case("(a,b)")]
    #[case("file-(1..3).txt")]
    #[case("prefix-(a,b)")]
    #[case("(a..d)")]
    #[case("(1..10..2)")]
    fn a_group_expands_through_the_table_function(#[case] value: &str) {
        let out = text_sql(&[seg(&[value])]);
        assert!(out.contains("parexp("), "{out}");
        assert!(!out.contains("BETWEEN"), "{out}");
        assert!(out.len() < 400, "the expansion leaked into sql text, {} bytes", out.len());
    }

    #[test]
    fn a_pure_range_on_a_numeric_column_becomes_between() {
        let out = sql(&[seg(&["(1..100)"])]);
        assert!(out.contains("BETWEEN 1 AND 100"), "{out}");
        assert!(!out.contains("parexp"), "a pure range on a numeric column must not expand: {out}");
        assert!(!out.contains("CAST"), "the column should not be cast: {out}");
    }

    #[test]
    fn a_range_past_the_f64_mantissa_keeps_every_digit() {
        let out = sql(&[seg(&["(9007199254740993..9007199254740993)"])]);
        assert!(out.contains("9007199254740993"), "the bound was rounded: {out}");
    }

    #[rstest]
    #[case("(01..01)")]
    #[case("(0.5..1.5)")]
    #[case("(-5..10)")]
    #[case("(+5..10)")]
    fn a_padded_signed_or_fractional_range_does_not_take_the_numeric_path(#[case] value: &str) {
        match held(&[seg(&[value])], &columns()) {
            Ok(expr) => {
                let out =
                    Query::select().expr(Expr::cust("1")).and_where(expr.unwrap()).to_string(PostgresQueryBuilder);
                assert!(!out.contains("BETWEEN"), "{out}");
            }
            Err(e) => assert!(matches!(e, FilterError::NotANumber(_)), "{e:?}"),
        }
    }

    #[rstest]
    #[case("DOUBLE", "2.6::DOUBLE AS v", &["v", "in", "(3..4)"], false)]
    #[case("DOUBLE", "3.0::DOUBLE AS v", &["v", "in", "(3..4)"], true)]
    #[case("DECIMAL(18,3)", "2.6::DECIMAL(18,3) AS v", &["v", "in", "(3..4)"], false)]
    fn a_range_on_a_fractional_column_does_not_round(
        #[case] kind: &str,
        #[case] row: &str,
        #[case] params: &[&str],
        #[case] expected: bool,
    ) {
        match executes(params, kind, row) {
            Ok(matched) => assert_eq!(matched, expected, "{kind} {params:?}"),
            Err(e) => panic!("{kind} {params:?} produced invalid sql: {e}"),
        }
    }

    #[rstest]
    #[case(vec!["a"])]
    #[case(vec!["a", "b"])]
    #[case(vec![])]
    fn an_unknown_name_is_unknown_whatever_it_carries(#[case] params: Vec<&str>) {
        let seg = Segment { name: "zzz".to_string(), params: params.iter().map(|p| p.to_string()).collect() };
        let outcome = held(&[seg], &columns());
        assert_eq!(outcome.unwrap_err(), FilterError::UnknownFilter("zzz".to_string()));
    }

    #[test]
    fn an_unknown_filter_name_is_reported() {
        let unknown = Segment { name: "zzz".to_string(), params: vec!["1".to_string()] };
        assert_eq!(held(&[unknown], &columns()).unwrap_err(), FilterError::UnknownFilter("zzz".to_string()));
    }

    #[rstest]
    #[case("' OR 1=1 --")]
    #[case("x'); DROP TABLE t; --")]
    #[case("1' UNION SELECT 'x")]
    fn duckdb_executes_a_hostile_value_as_data(#[case] value: &str) {
        let cols = vec![("id".to_string(), "VARCHAR".to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let expr = held(&[seg(&[value])], &cols).unwrap().unwrap();
        let out = Query::select()
            .expr(Expr::cust("1"))
            .from_subquery(Query::select().expr(Expr::cust("'42' AS id")).take(), Alias::new("t"))
            .and_where(expr)
            .to_string(PostgresQueryBuilder);
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let matched = conn
            .prepare(&out)
            .and_then(|mut s| s.query([]).and_then(|mut r| r.next().map(|row| row.is_some())))
            .unwrap_or_else(|e| panic!("duckdb rejected the escaped literal: {e}\n{out}"));
        assert!(!matched, "a hostile value matched: {out}");
    }
}
