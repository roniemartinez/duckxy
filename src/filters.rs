use crate::parexp;
use crate::url::Segment;
use sea_query::{Alias, Expr, ExprTrait, Func, LikeExpr, Query, SimpleExpr};
use std::fmt;

const ID_CANDIDATES: [&str; 4] = ["id", "fid", "gid", "objectid"];

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
        }
    }
}

impl std::error::Error for FilterError {}

pub fn condition(filters: &[Segment], columns: &[(String, String)]) -> Result<Option<SimpleExpr>, FilterError> {
    let mut combined: Option<SimpleExpr> = None;
    for filter in filters {
        let next = match filter.name.as_str() {
            "id" => id_condition(&filter.params, columns)?,
            "prop" => prop_condition(&filter.params, columns)?,
            other => return Err(FilterError::UnknownFilter(other.to_string())),
        };
        combined = Some(match combined {
            Some(existing) => existing.and(next),
            None => next,
        });
    }
    Ok(combined)
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

fn numeral(value: &str) -> Option<&str> {
    let body = value.strip_prefix('-').unwrap_or(value);
    let digits = body.chars().filter(|c| c.is_ascii_digit()).count();
    let shaped = body.chars().all(|c| c.is_ascii_digit() || c == '.') && body.matches('.').count() <= 1;
    (digits > 0 && shaped).then_some(value)
}

fn id_condition(params: &[String], columns: &[(String, String)]) -> Result<SimpleExpr, FilterError> {
    let column = ID_CANDIDATES
        .iter()
        .find_map(|want| columns.iter().find(|(name, _)| name.eq_ignore_ascii_case(want)))
        .ok_or(FilterError::NoIdColumn)?;
    compare(column, params, "id")
}

fn prop_condition(params: &[String], columns: &[(String, String)]) -> Result<SimpleExpr, FilterError> {
    let [key, rest @ ..] = params else { return Err(FilterError::BadParams("prop".to_string())) };
    let column = columns.iter().find(|(name, _)| name == key).ok_or_else(|| FilterError::UnknownColumn(key.clone()))?;
    compare(column, rest, "prop")
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
        let columns = vec![("id".to_string(), "BIGINT".to_string()), ("name".to_string(), "VARCHAR".to_string())];
        for def in &crate::grammar::Grammar::core().filters {
            for shape in &def.shapes {
                let segment =
                    Segment { name: def.canonical().to_string(), params: vec!["name".to_string(); shape.len()] };
                let outcome = condition(&[segment], &columns);
                assert!(
                    !matches!(outcome, Err(FilterError::UnknownFilter(_))),
                    "{:?} is registered but condition cannot execute it",
                    def.canonical()
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

    fn seg(params: &[&str]) -> Segment {
        Segment { name: "id".to_string(), params: params.iter().map(|p| p.to_string()).collect() }
    }

    fn render(filters: &[Segment], cols: &[(String, String)]) -> String {
        let expr = condition(filters, cols).unwrap().unwrap();
        Query::select().expr(Expr::cust("1")).and_where(expr).to_string(PostgresQueryBuilder)
    }

    fn sql(filters: &[Segment]) -> String {
        render(filters, &columns())
    }

    fn text_sql(filters: &[Segment]) -> String {
        let cols = vec![("id".to_string(), "VARCHAR".to_string())];
        render(filters, &cols)
    }

    fn executes(params: &[&str], kind: &str, row: &str) -> Result<bool, String> {
        let cols = vec![("v".to_string(), kind.to_string())];
        let seg = Segment { name: "prop".to_string(), params: params.iter().map(|p| p.to_string()).collect() };
        let expr = condition(&[seg], &cols).map_err(|e| e.to_string())?.unwrap();
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

    #[test]
    fn no_filters_is_no_condition() {
        assert!(condition(&[], &columns()).unwrap().is_none());
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
        let outcome = condition(&[seg(&[value])], &columns());
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
    #[case(vec![("fid".to_string(), "BIGINT".to_string())], true)]
    #[case(vec![("GID".to_string(), "BIGINT".to_string())], true)]
    #[case(vec![("ObjectID".to_string(), "BIGINT".to_string())], true)]
    #[case(vec![("name".to_string(), "VARCHAR".to_string())], false)]
    fn id_finds_its_column_case_insensitively(#[case] cols: Vec<(String, String)>, #[case] found: bool) {
        let outcome = condition(&[seg(&["1"])], &cols);
        assert_eq!(outcome.is_ok(), found, "{outcome:?}");
        if !found {
            assert_eq!(outcome.unwrap_err(), FilterError::NoIdColumn);
        }
    }

    #[test]
    fn id_prefers_id_over_the_other_candidates() {
        let cols = vec![("fid".to_string(), "BIGINT".to_string()), ("id".to_string(), "BIGINT".to_string())];
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
        assert_eq!(condition(&[seg(params)], &columns()).unwrap_err(), FilterError::BadParams("id".to_string()));
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
        match condition(&[seg(&[value])], &columns()) {
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

    #[test]
    fn an_unknown_filter_name_is_reported() {
        let unknown = Segment { name: "zzz".to_string(), params: vec!["1".to_string()] };
        assert_eq!(condition(&[unknown], &columns()).unwrap_err(), FilterError::UnknownFilter("zzz".to_string()));
    }

    #[rstest]
    #[case("' OR 1=1 --")]
    #[case("x'); DROP TABLE t; --")]
    #[case("1' UNION SELECT 'x")]
    fn duckdb_executes_a_hostile_value_as_data(#[case] value: &str) {
        let cols = vec![("id".to_string(), "VARCHAR".to_string())];
        let expr = condition(&[seg(&[value])], &cols).unwrap().unwrap();
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
