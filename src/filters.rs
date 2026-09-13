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
        }
    }
}

impl std::error::Error for FilterError {}

pub fn condition(filters: &[Segment], columns: &[(String, String)]) -> Result<Option<SimpleExpr>, FilterError> {
    let mut combined: Option<SimpleExpr> = None;
    for filter in filters {
        let next = match filter.name.as_str() {
            "id" => id_condition(&filter.params, columns)?,
            other => return Err(FilterError::UnknownFilter(other.to_string())),
        };
        combined = Some(match combined {
            Some(existing) => existing.and(next),
            None => next,
        });
    }
    Ok(combined)
}

fn is_integer(kind: &str) -> bool {
    matches!(
        kind,
        "TINYINT"
            | "SMALLINT"
            | "INTEGER"
            | "BIGINT"
            | "HUGEINT"
            | "UTINYINT"
            | "USMALLINT"
            | "UINTEGER"
            | "UBIGINT"
            | "UHUGEINT"
    )
}

fn id_condition(params: &[String], columns: &[(String, String)]) -> Result<SimpleExpr, FilterError> {
    let (name, kind) = ID_CANDIDATES
        .iter()
        .find_map(|want| columns.iter().find(|(name, _)| name.eq_ignore_ascii_case(want)))
        .ok_or(FilterError::NoIdColumn)?;
    let [raw] = params else { return Err(FilterError::BadParams("id".to_string())) };
    let column = || Expr::col(Alias::new(name.as_str()));

    if is_integer(kind)
        && let Some((lo, hi)) = parexp::as_integer_range(raw)
    {
        let bounds = [column(), Expr::val(lo), Expr::val(hi)];
        return Ok(Expr::cust_with_exprs("TRY_CAST($1 AS BIGINT) BETWEEN $2 AND $3", bounds));
    }
    if parexp::count(raw) > parexp::MAX_EXPANSION {
        return Err(FilterError::ExpansionTooLarge(raw.clone()));
    }
    if let [only] = parexp::expand(raw).as_slice() {
        return Ok(Expr::cust_with_exprs("CAST($1 AS VARCHAR) = $2", [column(), Expr::val(only)]));
    }
    Ok(Expr::cust_with_exprs("CAST($1 AS VARCHAR) IN (SELECT v FROM parexp($2))", [column(), Expr::val(raw)]))
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
    #[case(&["a", "b"])]
    fn id_takes_exactly_one_value(#[case] params: &[&str]) {
        assert_eq!(condition(&[seg(params)], &columns()).unwrap_err(), FilterError::BadParams("id".to_string()));
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
        assert!(matches!(err, FilterError::ExpansionTooLarge { .. }), "{err:?}");
        assert!(err.to_string().contains("past the limit of 10000"), "{err}");
    }

    #[test]
    fn a_huge_range_on_a_text_column_is_refused_rather_than_truncated() {
        let err = condition(&[seg(&["(1..1000000)"])], &text_columns()).unwrap_err();
        assert!(matches!(err, FilterError::ExpansionTooLarge { .. }), "{err:?}");
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
