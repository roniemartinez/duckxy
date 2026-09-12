use crate::url::Segment;
use sea_query::{Alias, Expr, ExprTrait, SimpleExpr};
use std::fmt;

const ID_CANDIDATES: [&str; 4] = ["id", "fid", "gid", "objectid"];

#[derive(Debug, PartialEq)]
pub enum FilterError {
    NoIdColumn,
    UnknownFilter(String),
    BadParams(String),
    ExpansionUnsupported(String),
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterError::NoIdColumn => {
                write!(f, "source has no id column, expected one of: {}", ID_CANDIDATES.join(", "))
            }
            FilterError::UnknownFilter(name) => write!(f, "unknown filter: {name}"),
            FilterError::BadParams(name) => write!(f, "filter takes exactly one value: {name}"),
            FilterError::ExpansionUnsupported(value) => {
                write!(f, "pattern expansion is not supported yet, use a single value: {value}")
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

fn id_condition(params: &[String], columns: &[(String, String)]) -> Result<SimpleExpr, FilterError> {
    let column = ID_CANDIDATES
        .iter()
        .find_map(|want| columns.iter().find(|(name, _)| name.eq_ignore_ascii_case(want)))
        .map(|(name, _)| name.as_str())
        .ok_or(FilterError::NoIdColumn)?;
    let [raw] = params else { return Err(FilterError::BadParams("id".to_string())) };
    let value = unquote(raw);
    if value.len() == raw.len() && value.contains('(') {
        return Err(FilterError::ExpansionUnsupported(raw.clone()));
    }
    Ok(Expr::cust_with_exprs("CAST($1 AS VARCHAR) = $2", [Expr::col(Alias::new(column)), Expr::val(value)]))
}

fn unquote(value: &str) -> &str {
    match value.len() >= 2 && value.starts_with('~') && value.ends_with('~') {
        true => &value[1..value.len() - 1],
        false => value,
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
    #[case(&["a", "b"])]
    fn id_takes_exactly_one_value(#[case] params: &[&str]) {
        assert_eq!(condition(&[seg(params)], &columns()).unwrap_err(), FilterError::BadParams("id".to_string()));
    }

    #[rstest]
    #[case("(1,2)")]
    #[case("(1..100)")]
    #[case("(a,b)")]
    #[case("file-(1..3).txt")]
    #[case("prefix-(a,b)")]
    fn a_value_containing_a_group_is_refused_with_a_clear_message(#[case] value: &str) {
        let err = condition(&[seg(&[value])], &columns()).unwrap_err();
        assert_eq!(err, FilterError::ExpansionUnsupported(value.to_string()));
        assert!(err.to_string().contains("not supported yet"), "{err}");
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
