use duckdb::core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId};
use duckdb::ffi::duckdb_vector_size;
use duckdb::vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const MAX_EXPANSION: usize = 10_000;
pub const MAX_DEPTH: usize = 32;

enum Range<'a> {
    Numeric { lo: i64, hi: i64, step: i64, lo_raw: &'a str, hi_raw: &'a str },
    Alpha { from: u8, to: u8 },
}

pub fn expand(pattern: &str) -> Vec<String> {
    let mut out = expand_inner(pattern, true, 0);
    out.truncate(MAX_EXPANSION);
    out
}

pub fn count(pattern: &str) -> usize {
    count_inner(pattern, true, 0)
}

pub fn as_integer_range(pattern: &str) -> Option<(i64, i64)> {
    let inner = pattern.strip_prefix('(')?.strip_suffix(')')?;
    if inner.contains(',') || inner.contains('~') || inner.contains('(') {
        return None;
    }
    match parse_range(inner)? {
        Range::Numeric { lo, hi, step, lo_raw, hi_raw } if step == 1 && !padded(lo_raw) && !padded(hi_raw) => {
            Some((lo.min(hi), lo.max(hi)))
        }
        _ => None,
    }
}

fn expand_leaf(s: &str) -> Vec<String> {
    vec![s.replace('~', "")]
}

fn padded(raw: &str) -> bool {
    let digits = raw.replace('_', "");
    digits.len() > 1 && digits.starts_with('0')
}

fn parse_range(s: &str) -> Option<Range<'_>> {
    let parts: Vec<&str> = s.split("..").collect();
    let alpha = |t: &str| t.len() == 1 && t.as_bytes()[0].is_ascii_alphabetic();
    if parts.len() == 2 && alpha(parts[0]) && alpha(parts[1]) {
        return Some(Range::Alpha { from: parts[0].as_bytes()[0], to: parts[1].as_bytes()[0] });
    }
    if parts.len() != 2 && parts.len() != 3 {
        return None;
    }
    if !parts.iter().all(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '_')) {
        return None;
    }
    let num = |t: &str| t.replace('_', "").parse::<i64>().ok();
    let step = match parts.len() {
        3 => num(parts[2])?,
        _ => 1,
    };
    match step > 0 {
        true => {
            Some(Range::Numeric { lo: num(parts[0])?, hi: num(parts[1])?, step, lo_raw: parts[0], hi_raw: parts[1] })
        }
        false => None,
    }
}

fn find_group(s: &str) -> Option<(usize, usize)> {
    let (mut depth, mut in_tilde, mut open) = (0usize, false, 0usize);
    for (at, byte) in s.as_bytes().iter().enumerate() {
        match byte {
            b'~' => in_tilde = !in_tilde,
            b'(' if !in_tilde => {
                if depth == 0 {
                    open = at;
                }
                depth += 1;
            }
            b')' if !in_tilde && depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, at));
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut in_tilde = false;
    let mut from = 0usize;
    for (at, byte) in s.as_bytes().iter().enumerate() {
        match byte {
            b'~' => in_tilde = !in_tilde,
            b'(' if !in_tilde => depth += 1,
            b')' if !in_tilde => depth = depth.saturating_sub(1),
            b',' if depth == 0 && !in_tilde => {
                parts.push(&s[from..at]);
                from = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[from..]);
    parts
}

fn split_group(s: &str) -> Option<(&str, Vec<&str>, &str)> {
    let (open, close) = find_group(s)?;
    Some((&s[..open], split_top_level(&s[open + 1..close]), &s[close + 1..]))
}

fn expand_inner(s: &str, allow_range: bool, depth: usize) -> Vec<String> {
    if depth >= MAX_DEPTH {
        return expand_leaf(s);
    }
    let mut done: Vec<String> = vec![String::new()];
    let mut rest = s;
    let mut ranged = allow_range;
    loop {
        let Some((head, parts, tail)) = split_group(rest) else {
            let leaf = if ranged { expand_literal(rest) } else { expand_leaf(rest) };
            return cross(done, leaf);
        };
        let allow_child = parts.len() == 1;
        let mut group: Vec<String> = Vec::new();
        for part in parts {
            if group.len() >= MAX_EXPANSION {
                break;
            }
            group.extend(expand_inner(part, allow_child, depth + 1));
        }
        done = cross(cross(done, expand_literal(head)), group);
        rest = tail;
        ranged = true;
    }
}

fn cross(left: Vec<String>, right: Vec<String>) -> Vec<String> {
    if left.len() == 1 && left[0].is_empty() {
        return right;
    }
    if right.len() == 1 && right[0].is_empty() {
        return left;
    }
    let mut out = Vec::new();
    for l in &left {
        for r in &right {
            if out.len() >= MAX_EXPANSION {
                return out;
            }
            out.push(format!("{l}{r}"));
        }
    }
    out
}

fn expand_literal(s: &str) -> Vec<String> {
    match parse_range(s) {
        Some(Range::Numeric { lo, hi, step, lo_raw, hi_raw }) => {
            let width = match padded(lo_raw) {
                true => lo_raw.replace('_', "").len().max(hi_raw.replace('_', "").len()),
                false => 0,
            };
            let mut out = Vec::new();
            let mut n = lo;
            while (lo <= hi && n <= hi) || (lo > hi && n >= hi) {
                if out.len() >= MAX_EXPANSION {
                    break;
                }
                out.push(format!("{n:0width$}"));
                let Some(next) = (if lo <= hi { n.checked_add(step) } else { n.checked_sub(step) }) else { break };
                n = next;
            }
            out
        }
        Some(Range::Alpha { from, to }) => {
            let chars: Vec<u8> = if from <= to { (from..=to).collect() } else { (to..=from).rev().collect() };
            chars.into_iter().take(MAX_EXPANSION).map(|c| (c as char).to_string()).collect()
        }
        None => expand_leaf(s),
    }
}

fn count_inner(s: &str, allow_range: bool, depth: usize) -> usize {
    if depth >= MAX_DEPTH {
        return 1;
    }
    let mut total = 1usize;
    let mut rest = s;
    let mut ranged = allow_range;
    loop {
        let Some((head, parts, tail)) = split_group(rest) else {
            let leaf = if ranged { count_literal(rest) } else { 1 };
            return total.saturating_mul(leaf);
        };
        let allow_child = parts.len() == 1;
        let group =
            parts.iter().map(|part| count_inner(part, allow_child, depth + 1)).fold(0usize, usize::saturating_add);
        total = total.saturating_mul(count_literal(head)).saturating_mul(group);
        rest = tail;
        ranged = true;
    }
}

fn count_literal(s: &str) -> usize {
    match parse_range(s) {
        Some(Range::Numeric { lo, hi, step, .. }) => (lo.abs_diff(hi) / step.unsigned_abs()) as usize + 1,
        Some(Range::Alpha { from, to }) => from.abs_diff(to) as usize + 1,
        None => 1,
    }
}

pub struct Parexp;

pub struct ParexpBind {
    values: Vec<String>,
}

pub struct ParexpInit {
    cursor: AtomicUsize,
}

impl VTab for Parexp {
    type BindData = ParexpBind;
    type InitData = ParexpInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn std::error::Error>> {
        bind.add_result_column("v", LogicalTypeId::Varchar.into());
        Ok(ParexpBind { values: expand(&bind.get_parameter(0).to_string()) })
    }

    fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn std::error::Error>> {
        Ok(ParexpInit { cursor: AtomicUsize::new(0) })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), Box<dyn std::error::Error>> {
        let values = &func.get_bind_data().values;
        let projected = output.num_columns() > 0;
        let capacity =
            if projected { output.flat_vector(0).capacity() } else { unsafe { duckdb_vector_size() as usize } };
        let start = func.get_init_data().cursor.fetch_add(capacity, Ordering::Relaxed);
        if start >= values.len() {
            output.set_len(0);
            return Ok(());
        }
        let end = (start + capacity).min(values.len());
        if projected {
            let vector = output.flat_vector(0);
            for (at, value) in values[start..end].iter().enumerate() {
                vector.insert(at, value.as_str());
            }
        }
        output.set_len(end - start);
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![LogicalTypeId::Varchar.into()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("plain", vec!["plain"])]
    #[case("(a,b,c)", vec!["a", "b", "c"])]
    #[case("(1,2,5,7)", vec!["1", "2", "5", "7"])]
    #[case("(1,2,5,7,11,13)", vec!["1", "2", "5", "7", "11", "13"])]
    #[case("(CA,NY,TX,WA)", vec!["CA", "NY", "TX", "WA"])]
    #[case("(1..5)", vec!["1", "2", "3", "4", "5"])]
    #[case("(01..03)", vec!["01", "02", "03"])]
    #[case("(1..10..2)", vec!["1", "3", "5", "7", "9"])]
    #[case("(5..1)", vec!["5", "4", "3", "2", "1"])]
    #[case("(a..d)", vec!["a", "b", "c", "d"])]
    #[case("(1_000..1_003)", vec!["1000", "1001", "1002", "1003"])]
    #[case("file-(1..3).txt", vec!["file-1.txt", "file-2.txt", "file-3.txt"])]
    #[case("(x,y)-(1..2)", vec!["x-1", "x-2", "y-1", "y-2"])]
    #[case("(~A, B~,C)", vec!["A, B", "C"])]
    #[case("~Armagh City, Banbridge~", vec!["Armagh City, Banbridge"])]
    #[case("(1..3,7)", vec!["1..3", "7"])]
    #[case("(unmatched", vec!["(unmatched"])]
    #[case("(1..5..0)", vec!["1..5..0"])]
    #[case("a(b,c)d", vec!["abd", "acd"])]
    #[case("a(b(c,d),e)f", vec!["abcf", "abdf", "aef"])]
    #[case("", vec![""])]
    #[case("a(hello,)b", vec!["ahellob", "ab"])]
    #[case("a(,hello)b", vec!["ab", "ahellob"])]
    #[case("a(1..3)b", vec!["a1b", "a2b", "a3b"])]
    #[case("file(01..03).txt", vec!["file01.txt", "file02.txt", "file03.txt"])]
    #[case("(d..a)", vec!["d", "c", "b", "a"])]
    #[case("a(b..d)e", vec!["abe", "ace", "ade"])]
    #[case("prefix-(1..3)", vec!["prefix-1", "prefix-2", "prefix-3"])]
    #[case("(a,b)-suffix", vec!["a-suffix", "b-suffix"])]
    #[case("pre-(a,b)-suf", vec!["pre-a-suf", "pre-b-suf"])]
    #[case("file-(01..03).txt", vec!["file-01.txt", "file-02.txt", "file-03.txt"])]
    #[case("x(y,z)w(a,b)", vec!["xywa", "xywb", "xzwa", "xzwb"])]
    #[case("v(1..2).(0..1)", vec!["v1.0", "v1.1", "v2.0", "v2.1"])]
    #[case("((1..3))", vec!["1", "2", "3"])]
    #[case("((a,b,c))", vec!["a", "b", "c"])]
    #[case("(((1..2)))", vec!["1", "2"])]
    #[case("(1,5..7,10)", vec!["1", "5..7", "10"])]
    #[case("(1,(5..7),10)", vec!["1", "5", "6", "7", "10"])]
    #[case("file-(~a,b~).txt", vec!["file-a,b.txt"])]
    #[case("prefix-~(a,b)~", vec!["prefix-(a,b)"])]
    #[case("a~)~b", vec!["a)b"])]
    #[case("~~x~~", vec!["x"])]
    #[case("(~a~,~b~)", vec!["a", "b"])]
    #[case("~a~-(1..2)", vec!["a-1", "a-2"])]
    fn expands(#[case] pattern: &str, #[case] expected: Vec<&str>) {
        assert_eq!(expand(pattern), expected);
    }

    #[test]
    fn a_wide_numeric_range_is_not_zero_padded() {
        assert_eq!(expand("(1..100)"), (1..=100).map(|n| n.to_string()).collect::<Vec<_>>());
    }

    #[rstest]
    #[case("(1..10000)", "1", "10000")]
    #[case("(1..100000)", "1", "10000")]
    #[case("(1..1000000)", "1", "10000")]
    fn expansion_truncates_at_the_cap(#[case] pattern: &str, #[case] first: &str, #[case] last: &str) {
        let out = expand(pattern);
        assert_eq!(out.len(), MAX_EXPANSION, "{pattern}");
        assert_eq!(out.first().map(String::as_str), Some(first), "{pattern}");
        assert_eq!(out.last().map(String::as_str), Some(last), "{pattern}");
    }

    #[rstest]
    #[case("(1..10..2)", vec!["1", "3", "5", "7", "9"])]
    #[case("(1..100)", vec!["1", "2"])]
    fn a_wider_end_operand_does_not_zero_pad(#[case] pattern: &str, #[case] head: Vec<&str>) {
        let out = expand(pattern);
        assert_eq!(out[..head.len()], head[..], "the parent pads here, duckxy deliberately does not");
    }

    #[rstest]
    #[case("(9223372036854775807..9223372036854775807)", 1)]
    #[case("(9223372036854775806..9223372036854775807)", 2)]
    fn an_i64_boundary_range_does_not_overflow(#[case] pattern: &str, #[case] count: usize) {
        assert_eq!(expand(pattern).len(), count, "{pattern}");
    }

    #[rstest]
    #[case("(1..100)", Some((1, 100)))]
    #[case("(1_000..10_000)", Some((1000, 10000)))]
    #[case("(100..1)", Some((1, 100)))]
    #[case("(9007199254740993..9007199254740993)", Some((9007199254740993, 9007199254740993)))]
    #[case("(a..z)", None)]
    #[case("(1..10..2)", None)]
    #[case("(1,2,3)", None)]
    #[case("prefix(1..5)", None)]
    #[case("(~1..5~)", None)]
    #[case("plain", None)]
    #[case("(1..1e400)", None)]
    #[case("(inf..100)", None)]
    #[case("(1..nan)", None)]
    #[case("(-5.5..10.5)", None)]
    #[case("(0.5..1.5)", None)]
    #[case("(01..01)", None)]
    #[case("(01..03)", None)]
    #[case("(-5..10)", None)]
    #[case("(+5..10)", None)]
    #[case("(5..-10)", None)]
    fn recognises_an_exact_integer_range(#[case] pattern: &str, #[case] expected: Option<(i64, i64)>) {
        assert_eq!(as_integer_range(pattern), expected);
    }

    #[rstest]
    #[case("plain", 1)]
    #[case("(a,b,c)", 3)]
    #[case("(1..5)", 5)]
    #[case("(1..10..2)", 5)]
    #[case("(a..d)", 4)]
    #[case("(x,y)-(1..2)", 4)]
    #[case("(1..1000000)", 1000000)]
    #[case("a(b(c,d),e)f", 3)]
    #[case("(-5..10)", 1)]
    fn counts_without_expanding(#[case] pattern: &str, #[case] expected: usize) {
        assert_eq!(count(pattern), expected, "{pattern}");
        if expected <= MAX_EXPANSION {
            assert_eq!(expand(pattern).len(), expected, "count disagrees with expand for {pattern}");
        }
    }

    #[test]
    fn a_group_count_that_overflows_saturates_rather_than_wrapping() {
        let huge = "(0..9223372036854775807)";
        let pattern = format!("({huge},{huge})");
        assert_eq!(count(&pattern), usize::MAX, "the sum wrapped and bypassed the cap");
    }

    #[rstest]
    #[case(4)]
    #[case(31)]
    #[case(40)]
    #[case(200)]
    fn sequential_groups_do_not_exhaust_the_nesting_budget(#[case] leading: usize) {
        let pattern = format!("{}(b,c)", "(a)".repeat(leading));
        let expected = vec![format!("{}b", "a".repeat(leading)), format!("{}c", "a".repeat(leading))];
        assert_eq!(count(&pattern), 2, "count wrong after {leading} sequential groups");
        assert_eq!(expand(&pattern), expected, "expand wrong after {leading} sequential groups");
    }

    #[test]
    fn sequential_groups_cost_no_stack() {
        let pattern = format!("{}(b,c)", "(a)".repeat(20_000));
        assert_eq!(count(&pattern), 2);
        assert_eq!(expand(&pattern).len(), 2);
    }

    #[test]
    fn nesting_past_the_limit_degrades_to_a_literal_rather_than_overflowing() {
        let deep = format!("{}x{}", "(".repeat(5_000), ")".repeat(5_000));
        assert_eq!(count(&deep), 1);
        assert_eq!(expand(&deep).len(), 1);
    }

    #[test]
    fn counting_a_pathological_pattern_stays_cheap() {
        let pattern = "(a,b)".repeat(200);
        let started = std::time::Instant::now();
        assert!(count(&pattern) > MAX_EXPANSION);
        assert!(started.elapsed().as_millis() < 100, "counting took {:?}", started.elapsed());
    }

    #[rstest]
    #[case("(1..5)", "1,2,3,4,5")]
    #[case("(01..03)", "01,02,03")]
    #[case("(CA,NY)", "CA,NY")]
    #[case("(~A, B~,C)", "A, B,C")]
    fn the_table_function_returns_the_expansion(#[case] pattern: &str, #[case] expected: &str) {
        crate::ensure_spatial();
        let joined = crate::query::with_connection(|conn| {
            let mut stmt = conn.prepare("SELECT string_agg(v, ',') FROM parexp(?)")?;
            let mut rows = stmt.query([pattern])?;
            let row = rows.next()?.expect("one row");
            Ok(row.get::<_, Option<String>>(0)?.unwrap_or_default())
        })
        .unwrap();
        assert_eq!(joined, expected);
    }

    #[test]
    fn a_count_projects_no_columns_and_must_not_read_one() {
        crate::ensure_spatial();
        let n = crate::query::with_connection(|conn| {
            let mut stmt = conn.prepare("SELECT count(*)::VARCHAR FROM parexp('(1..5)')")?;
            let mut rows = stmt.query([])?;
            Ok(rows.next()?.expect("one row").get::<_, String>(0)?)
        })
        .unwrap();
        assert_eq!(n, "5");
    }

    #[test]
    fn the_table_function_handles_a_large_expansion() {
        crate::ensure_spatial();
        let n = crate::query::with_connection(|conn| {
            let mut stmt = conn.prepare("SELECT count(*)::VARCHAR FROM parexp('(1..10000)')")?;
            let mut rows = stmt.query([])?;
            Ok(rows.next()?.expect("one row").get::<_, String>(0)?)
        })
        .unwrap();
        assert_eq!(n, "10000");
    }
}
