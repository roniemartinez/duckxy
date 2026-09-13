use crate::parexp;
use crate::url::Segment;
use sea_query::{Alias, Expr, ExprTrait, SimpleExpr};
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
}

enum NumericKind {
    Integer,
    Fractional,
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterError::NoIdColumn => {
                write!(f, "source has no id column, expected one of: {}", ID_CANDIDATES.join(", "))
            }
            FilterError::UnknownFilter(name) => write!(f, "unknown filter: {name}"),
            FilterError::BadParams(name) => write!(f, "filter takes exactly one value: {name}"),
            FilterError::ExpansionTooLarge(pattern) => {
                write!(f, "pattern expands past the limit of {} values: {pattern}", parexp::MAX_EXPANSION)
            }
            FilterError::UnknownColumn(key) => write!(f, "source has no column: {key}"),
            FilterError::UnknownOperator(op) => write!(f, "unknown property operator: {op}"),
            FilterError::NotANumber(value) => write!(f, "a numeric column needs a numeric value: {value}"),
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
    match kind.split('(').next().unwrap_or(kind) {
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT" | "USMALLINT" | "UINTEGER"
        | "UBIGINT" | "UHUGEINT" => Some(NumericKind::Integer),
        "FLOAT" | "DOUBLE" | "REAL" | "DECIMAL" => Some(NumericKind::Fractional),
        _ => None,
    }
}

fn value_set(column: impl Fn() -> SimpleExpr, kind: &str, pattern: &str) -> Result<SimpleExpr, FilterError> {
    if matches!(numeric_kind(kind), Some(NumericKind::Integer))
        && let Some((lo, hi)) = parexp::as_integer_range(pattern)
    {
        let bounds = [column(), Expr::val(lo), Expr::val(hi)];
        return Ok(Expr::cust_with_exprs("TRY_CAST($1 AS BIGINT) BETWEEN $2 AND $3", bounds));
    }
    if parexp::count(pattern) > parexp::MAX_EXPANSION {
        return Err(FilterError::ExpansionTooLarge(pattern.to_string()));
    }
    if let [only] = parexp::expand(pattern).as_slice() {
        return Ok(Expr::cust_with_exprs("CAST($1 AS VARCHAR) = $2", [column(), Expr::val(only)]));
    }
    Ok(Expr::cust_with_exprs("CAST($1 AS VARCHAR) IN (SELECT v FROM parexp($2))", [column(), Expr::val(pattern)]))
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
    let literal = || parexp::expand(raw).pop().unwrap_or_default();

    Ok(match operator {
        "null" => Expr::cust_with_exprs("$1 IS NULL", [column()]),
        "notnull" => Expr::cust_with_exprs("$1 IS NOT NULL", [column()]),
        "eq" | "in" => value_set(column, kind, raw)?,
        "ne" | "nin" => Expr::cust_with_exprs("NOT ($1)", [value_set(column, kind, raw)?]),
        "ieq" => Expr::cust_with_exprs("lower(CAST($1 AS VARCHAR)) = lower($2)", [column(), Expr::val(literal())]),
        "gt" | "gte" | "lt" | "lte" => ordering(column, kind, operator, &literal())?,
        "sw" | "ew" | "ct" => Expr::cust_with_exprs(
            "CAST($1 AS VARCHAR) LIKE $2 ESCAPE '\\'",
            [column(), Expr::val(anchor(operator, &literal()))],
        ),
        "isw" | "iew" | "ict" => Expr::cust_with_exprs(
            "lower(CAST($1 AS VARCHAR)) LIKE lower($2) ESCAPE '\\'",
            [column(), Expr::val(anchor(&operator[1..], &literal()))],
        ),
        other => return Err(FilterError::UnknownOperator(other.to_string())),
    })
}

fn ordering(
    column: impl Fn() -> SimpleExpr,
    kind: &str,
    operator: &str,
    value: &str,
) -> Result<SimpleExpr, FilterError> {
    if numeric_kind(kind).is_none() {
        let template = match operator {
            "gt" => "CAST($1 AS VARCHAR) > $2",
            "gte" => "CAST($1 AS VARCHAR) >= $2",
            "lt" => "CAST($1 AS VARCHAR) < $2",
            _ => "CAST($1 AS VARCHAR) <= $2",
        };
        return Ok(Expr::cust_with_exprs(template, [column(), Expr::val(value)]));
    }
    let number = value.parse::<f64>().map_err(|_| FilterError::NotANumber(value.to_string()))?;
    let template = match operator {
        "gt" => "TRY_CAST($1 AS DOUBLE) > $2",
        "gte" => "TRY_CAST($1 AS DOUBLE) >= $2",
        "lt" => "TRY_CAST($1 AS DOUBLE) < $2",
        _ => "TRY_CAST($1 AS DOUBLE) <= $2",
    };
    Ok(Expr::cust_with_exprs(template, [column(), Expr::val(number)]))
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
        let out = sql(&[seg(&[value])]);
        assert!(out.contains(&format!("CAST(\"id\" AS VARCHAR) = {literal}")), "{out}");
        assert!(!out.contains("TRY_CAST"), "a value took a numeric path: {out}");
        assert!(!out.contains(" OR "), "an id lookup must match one spelling only: {out}");
    }

    #[rstest]
    #[case("~Armagh City, Banbridge~", "'Armagh City, Banbridge'")]
    #[case("~a:b~", "'a:b'")]
    #[case("plain", "'plain'")]
    fn tilde_quoting_is_stripped(#[case] value: &str, #[case] needle: &str) {
        assert!(sql(&[seg(&[value])]).contains(needle));
    }

    #[rstest]
    #[case(vec![("fid".to_string(), "BIGINT".to_string())], true)]
    #[case(vec![("GID".to_string(), "BIGINT".to_string())], true)]
    #[case(vec![("ObjectID".to_string(), "BIGINT".to_string())], true)]
    #[case(vec![("name".to_string(), "VARCHAR".to_string())], false)]
    fn id_finds_its_column_case_insensitively(#[case] cols: Vec<(String, String)>, #[case] found: bool) {
        let outcome = condition(&[seg(&["A"])], &cols);
        assert_eq!(outcome.is_ok(), found, "{outcome:?}");
        if !found {
            assert_eq!(outcome.unwrap_err(), FilterError::NoIdColumn);
        }
    }

    #[test]
    fn id_prefers_id_over_the_other_candidates() {
        let cols = vec![("fid".to_string(), "BIGINT".to_string()), ("id".to_string(), "BIGINT".to_string())];
        let out = render(&[seg(&["A"])], &cols);
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
    #[case(&["gte", "500"], "TRY_CAST(\"id\" AS DOUBLE) >= 500")]
    #[case(&["lt", "1000"], "TRY_CAST(\"id\" AS DOUBLE) < 1000")]
    #[case(&["in", "(500..1000)"], "TRY_CAST(\"id\" AS BIGINT) BETWEEN 500 AND 1000")]
    #[case(&["(50..100)"], "TRY_CAST(\"id\" AS BIGINT) BETWEEN 50 AND 100")]
    #[case(&["ct", "ab"], "LIKE")]
    fn id_accepts_the_same_operators_as_prop(#[case] params: &[&str], #[case] needle: &str) {
        let out = sql(&[seg(params)]);
        assert!(out.contains(needle), "{out}");
    }

    fn text_columns() -> Vec<(String, String)> {
        vec![("id".to_string(), "VARCHAR".to_string()), ("geom".to_string(), "GEOMETRY".to_string())]
    }

    #[rstest]
    #[case("(1,2)")]
    #[case("(a,b)")]
    #[case("file-(1..3).txt")]
    #[case("prefix-(a,b)")]
    #[case("(a..d)")]
    #[case("(1..10..2)")]
    fn a_group_expands_through_the_table_function(#[case] value: &str) {
        let out = sql(&[seg(&[value])]);
        assert!(out.contains("parexp("), "{out}");
        assert!(!out.contains("BETWEEN"), "{out}");
        assert!(out.len() < 400, "the expansion leaked into sql text, {} bytes", out.len());
    }

    #[test]
    fn a_pure_range_on_a_numeric_column_becomes_between() {
        let out = sql(&[seg(&["(1..100)"])]);
        assert!(out.contains("TRY_CAST"), "{out}");
        assert!(out.contains("BETWEEN"), "{out}");
        assert!(!out.contains("parexp"), "a pure range on a numeric column must not expand: {out}");
    }

    #[test]
    fn a_range_past_the_f64_mantissa_keeps_every_digit() {
        let out = sql(&[seg(&["(9007199254740993..9007199254740993)"])]);
        assert!(out.contains("9007199254740993"), "the bound was rounded: {out}");
        assert!(out.contains("BIGINT"), "{out}");
    }

    #[rstest]
    #[case("(01..01)")]
    #[case("(0.5..1.5)")]
    #[case("(-5..10)")]
    #[case("(+5..10)")]
    fn a_padded_signed_or_fractional_range_does_not_take_the_numeric_path(#[case] value: &str) {
        let out = sql(&[seg(&[value])]);
        assert!(!out.contains("BETWEEN"), "{out}");
    }

    #[rstest]
    #[case("DOUBLE")]
    #[case("FLOAT")]
    #[case("REAL")]
    #[case("DECIMAL(18,3)")]
    fn a_range_against_a_fractional_column_expands_rather_than_rounding(#[case] kind: &str) {
        let cols = vec![("id".to_string(), kind.to_string()), ("geom".to_string(), "GEOMETRY".to_string())];
        let out = render(&[seg(&["(3..4)"])], &cols);
        assert!(!out.contains("BETWEEN"), "casting to BIGINT would round 2.6 into this range: {out}");
        assert!(out.contains("parexp("), "{out}");
    }

    #[test]
    fn an_expansion_past_the_cap_is_refused_rather_than_truncated() {
        let err = condition(&[seg(&["item-(1..1000000)"])], &columns()).unwrap_err();
        assert!(matches!(err, FilterError::ExpansionTooLarge(..)), "{err:?}");
        assert!(err.to_string().contains("past the limit of 10000"), "{err}");
    }

    #[test]
    fn a_huge_range_on_a_text_column_is_refused_rather_than_truncated() {
        let err = condition(&[seg(&["(1..1000000)"])], &text_columns()).unwrap_err();
        assert!(matches!(err, FilterError::ExpansionTooLarge(..)), "{err:?}");
    }

    #[test]
    fn a_pure_range_on_a_text_column_expands_instead() {
        let out = render(&[seg(&["(1..100)"])], &text_columns());
        assert!(out.contains("parexp("), "{out}");
        assert!(!out.contains("BETWEEN"), "a text column must not be matched numerically: {out}");
        assert!(!out.contains("TRY_CAST"), "{out}");
    }

    #[rstest]
    #[case("(1..1e400)", "'1..1e400'")]
    #[case("(inf..100)", "'inf..100'")]
    #[case("(1..nan)", "'1..nan'")]
    fn a_non_finite_range_is_compared_as_a_literal(#[case] value: &str, #[case] literal: &str) {
        let out = sql(&[seg(&[value])]);
        assert!(!out.contains("BETWEEN"), "a non-finite bound reached the numeric path: {out}");
        assert!(out.contains(literal), "{out}");
    }

    #[rstest]
    #[case("~District (North)~", "'District (North)'")]
    #[case("~a(b~", "'a(b'")]
    fn a_tilde_quoted_paren_is_a_literal_not_a_group(#[case] value: &str, #[case] literal: &str) {
        let out = sql(&[seg(&[value])]);
        assert!(out.contains(literal), "{out}");
    }

    fn cols(kind: &str) -> Vec<(String, String)> {
        vec![
            ("id".to_string(), "BIGINT".to_string()),
            ("name".to_string(), kind.to_string()),
            ("geom".to_string(), "GEOMETRY".to_string()),
        ]
    }

    fn prop(params: &[&str]) -> Segment {
        Segment { name: "prop".to_string(), params: params.iter().map(|p| p.to_string()).collect() }
    }

    #[rstest]
    #[case(&["name", "CA"], "CAST(\"name\" AS VARCHAR) = 'CA'")]
    #[case(&["name", "eq", "CA"], "CAST(\"name\" AS VARCHAR) = 'CA'")]
    #[case(&["name", "ne", "CA"], "NOT (CAST(\"name\" AS VARCHAR) = 'CA')")]
    #[case(&["name", "ieq", "ca"], "lower(CAST(\"name\" AS VARCHAR)) = lower('ca')")]
    #[case(&["name", "null"], "\"name\" IS NULL")]
    #[case(&["name", "notnull"], "\"name\" IS NOT NULL")]
    #[case(&["name", "sw", "New"], "'New%'")]
    #[case(&["name", "ew", "City"], "'%City'")]
    #[case(&["name", "ct", "ew"], "'%ew%'")]
    fn prop_operators_compile(#[case] params: &[&str], #[case] needle: &str) {
        let out = render(&[prop(params)], &cols("VARCHAR"));
        assert!(out.contains(needle), "{out}");
    }

    #[rstest]
    #[case("BIGINT", "TRY_CAST")]
    #[case("INTEGER", "TRY_CAST")]
    #[case("DOUBLE", "TRY_CAST")]
    #[case("DECIMAL(18,3)", "TRY_CAST")]
    fn ordering_on_a_numeric_column_is_numeric(#[case] kind: &str, #[case] needle: &str) {
        let out = render(&[prop(&["name", "gt", "100"])], &cols(kind));
        assert!(out.contains(needle), "{out}");
        assert!(out.contains("> 100"), "{out}");
    }

    #[rstest]
    #[case("gt", ">")]
    #[case("gte", ">=")]
    #[case("lt", "<")]
    #[case("lte", "<=")]
    fn ordering_on_a_text_column_is_lexical(#[case] operator: &str, #[case] symbol: &str) {
        let out = render(&[prop(&["name", operator, "M"])], &cols("VARCHAR"));
        assert!(out.contains(&format!("CAST(\"name\" AS VARCHAR) {symbol} 'M'")), "{out}");
        assert!(!out.contains("TRY_CAST"), "a text column ordered numerically: {out}");
    }

    #[test]
    fn a_non_numeric_value_against_a_numeric_column_is_refused() {
        let err = condition(&[prop(&["name", "gt", "abc"])], &cols("BIGINT")).unwrap_err();
        assert_eq!(err, FilterError::NotANumber("abc".to_string()));
    }

    #[rstest]
    #[case("50_000", r"50\\_000")]
    #[case("100%", r"100\\%")]
    #[case(r"back\slash", r"back\\\\slash")]
    fn like_metacharacters_are_escaped(#[case] value: &str, #[case] escaped: &str) {
        let out = render(&[prop(&["name", "ct", value])], &cols("VARCHAR"));
        assert!(out.contains(escaped), "unescaped wildcard reached sql: {out}");
        assert!(out.contains("ESCAPE"), "{out}");
    }

    #[rstest]
    #[case("ieq")]
    #[case("isw")]
    #[case("iew")]
    #[case("ict")]
    fn case_insensitive_operators_lower_both_sides(#[case] operator: &str) {
        let out = render(&[prop(&["name", operator, "ca"])], &cols("VARCHAR"));
        assert_eq!(out.matches("lower(").count(), 2, "{out}");
    }

    #[rstest]
    #[case(&["name", "in", "(CA,NY)"])]
    #[case(&["name", "(CA,NY)"])]
    fn in_shares_the_expansion_path(#[case] params: &[&str]) {
        let out = render(&[prop(params)], &cols("VARCHAR"));
        assert!(out.contains("parexp("), "{out}");
        assert!(!out.contains("'CA'"), "the expansion leaked into sql text: {out}");
    }

    #[test]
    fn nin_negates_in() {
        let out = render(&[prop(&["name", "nin", "(CA,NY)"])], &cols("VARCHAR"));
        assert!(out.contains("NOT"), "{out}");
        assert!(out.contains("parexp("), "{out}");
    }

    #[test]
    fn a_prop_expansion_past_the_cap_is_refused() {
        let err = condition(&[prop(&["name", "in", "item-(1..1000000)"])], &cols("VARCHAR")).unwrap_err();
        assert!(matches!(err, FilterError::ExpansionTooLarge(..)), "{err:?}");
    }

    #[test]
    fn a_tilde_quoted_prop_value_is_stripped() {
        let out = render(&[prop(&["name", "~Armagh City, Banbridge~"])], &cols("VARCHAR"));
        assert!(out.contains("'Armagh City, Banbridge'"), "{out}");
    }

    #[test]
    fn two_prop_filters_are_and_chained() {
        let out = render(&[prop(&["name", "a"]), prop(&["name", "b"])], &cols("VARCHAR"));
        assert!(out.contains(" AND "), "{out}");
    }

    #[rstest]
    #[case(&["nosuch", "eq", "x"], FilterError::UnknownColumn("nosuch".to_string()))]
    #[case(&["name", "zzz", "x"], FilterError::UnknownOperator("zzz".to_string()))]
    #[case(&["name"], FilterError::BadParams("prop".to_string()))]
    fn prop_rejects(#[case] params: &[&str], #[case] expected: FilterError) {
        assert_eq!(condition(&[prop(params)], &cols("VARCHAR")).unwrap_err(), expected);
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
        let expr = condition(&[seg(&[value])], &columns()).unwrap().unwrap();
        let out = Query::select()
            .expr(Expr::cust("1"))
            .from_subquery(Query::select().expr(Expr::cust("42 AS id")).take(), Alias::new("t"))
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
