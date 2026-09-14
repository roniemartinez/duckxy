use std::fmt;

use crate::grammar::{ActionDef, Grammar, SegmentDef};

pub const DEFAULT_ENCODING: &str = crate::encodings::UTF8;
pub const MAX_FILTERS: usize = 50;
pub const MAX_ACTIONS: usize = 10;
pub const MAX_OPTIONS: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub name: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub name: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedUrl {
    pub dataset: String,
    pub path: Option<String>,
    pub encoding: String,
    pub filters: Vec<Segment>,
    pub actions: Vec<Action>,
    pub format: crate::formats::Format,
    pub extension: &'static str,
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    Empty,
    EmptyOptionValue(String),
    EmptySourceValue,
    InvalidName(String),
    InvalidPath(String),
    MisplacedOption(String),
    MissingOutputSegment,
    MissingSourcePrefix,
    RepeatedOption(String),
    UnknownFormat(String),
    UnknownOption(String),
    UnknownSource(String),
    UnknownAction(String),
    ActionWithoutOptions(String),
    MisplacedAction(String),
    TooManyActions,
    TooManyOptions,
    MalformedEncoding(String),
    MisplacedSourceOption(String),
    OptionTakesOneValue(String),
    WrongParameterCount(String),
    TooManyFilters,
    UnbalancedValue(String),
    ValueTooDeep(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => write!(f, "empty path"),
            ParseError::EmptyOptionValue(s) => write!(f, "option has no value: {s}"),
            ParseError::EmptySourceValue => write!(f, "source value is empty"),
            ParseError::InvalidName(n) => write!(f, "invalid name: {n}"),
            ParseError::InvalidPath(p) => write!(f, "path has characters that are not allowed: {p}"),
            ParseError::MisplacedOption(s) => write!(f, "options must come before the output name: {s}"),
            ParseError::MissingOutputSegment => write!(f, "missing .<format> suffix"),
            ParseError::MissingSourcePrefix => write!(f, "path must start with @"),
            ParseError::RepeatedOption(s) => write!(f, "option given more than once: {s}"),
            ParseError::UnknownFormat(s) => write!(f, "unknown format: {s}"),
            ParseError::UnknownOption(s) => write!(f, "unknown option: {s}"),
            ParseError::UnknownSource(s) => write!(f, "unknown source type: {s}"),
            ParseError::MalformedEncoding(s) => write!(f, "encoding has characters that are not allowed: {s}"),
            ParseError::MisplacedSourceOption(s) => write!(f, "source options must come before any filter: {s}"),
            ParseError::OptionTakesOneValue(s) => write!(f, "option takes exactly one value: {s}"),
            ParseError::WrongParameterCount(s) => write!(f, "filter has the wrong number of parameters: {s}"),
            ParseError::UnknownAction(s) => write!(f, "unknown action: {s}"),
            ParseError::ActionWithoutOptions(s) => write!(f, "action has no options: {s}"),
            ParseError::MisplacedAction(s) => write!(f, "actions must come before the output name: {s}"),
            ParseError::TooManyFilters => write!(f, "at most {MAX_FILTERS} filters are allowed"),
            ParseError::TooManyActions => write!(f, "at most {MAX_ACTIONS} actions are allowed"),
            ParseError::TooManyOptions => write!(f, "at most {MAX_OPTIONS} options are allowed"),
            ParseError::UnbalancedValue(s) => write!(f, "value has unbalanced ~ or (): {s}"),
            ParseError::ValueTooDeep(s) => {
                write!(f, "value nests groups more than {} deep: {s}", crate::parexp::MAX_DEPTH)
            }
        }
    }
}

impl std::error::Error for ParseError {}

pub fn parse(url: &str, grammar: &Grammar) -> Result<ParsedUrl, ParseError> {
    let url = url.trim_start_matches('/');
    if url.is_empty() {
        return Err(ParseError::Empty);
    }
    let Some((source, extension, format)) = crate::formats::Format::split(url) else {
        return Err(match url.rsplit_once('.') {
            Some((_, ext)) => ParseError::UnknownFormat(ext.to_ascii_lowercase()),
            None => ParseError::MissingOutputSegment,
        });
    };

    let mut scan = Scan { text: source, at: 0 };
    if !scan.eat(b'@') {
        return Err(ParseError::MissingSourcePrefix);
    }
    let kind = scan.take_until(b":");
    if !matches!(kind, "dataset" | "ds") {
        return Err(ParseError::UnknownSource(kind.to_string()));
    }
    if !scan.eat(b':') {
        return Err(ParseError::EmptySourceValue);
    }

    let name = scan.take_until(b":,/");
    if name.is_empty() {
        return Err(ParseError::EmptySourceValue);
    }
    if !is_valid_name(name) {
        return Err(ParseError::InvalidName(name.to_string()));
    }

    let mut path = None;
    if scan.eat(b':') {
        let value = scan.take_until(b",");
        if value.is_empty() {
            return Err(ParseError::EmptySourceValue);
        }
        let bounded = match action_at(value, grammar) {
            Some(at) => &value[..at],
            None => value,
        };
        if bounded.is_empty() {
            return Err(ParseError::EmptySourceValue);
        }
        let trimmed = trim_filename(bounded);
        scan.rewind(value.len() - trimmed.len());
        if !is_valid_path(trimmed) {
            return Err(ParseError::InvalidPath(trimmed.to_string()));
        }
        path = Some(trimmed.to_string());
    }

    let (encoding, filters) = read_options(&mut scan, grammar)?;

    let mut actions: Vec<Action> = Vec::new();
    while let Some(name) = scan.action_name() {
        let Some(def) = grammar.action_for(name) else {
            return Err(ParseError::UnknownAction(name.to_string()));
        };
        scan.advance(2 + name.len() + 1);
        let segments = read_action_segments(&mut scan, def)?;
        if actions.len() == MAX_ACTIONS {
            return Err(ParseError::TooManyActions);
        }
        actions.push(Action { name: def.canonical().to_string(), segments });
    }

    if scan.eat(b'/') {
        let name = scan.take_until(b"");
        if let Some(bare) = name.strip_prefix('@')
            && let Some(def) = grammar.action_for(bare)
        {
            return Err(ParseError::ActionWithoutOptions(def.canonical().to_string()));
        }
        if let Some(def) = buried_action(name, grammar) {
            return Err(ParseError::MisplacedAction(def.to_string()));
        }
        if name.contains(',') || name.contains(':') {
            return Err(ParseError::MisplacedOption(name.to_string()));
        }
    }

    let encoding = match encoding {
        Some(label) => resolve_encoding(&label).ok_or(ParseError::MalformedEncoding(label))?,
        None => DEFAULT_ENCODING.to_string(),
    };

    Ok(ParsedUrl { dataset: name.to_string(), path, encoding, filters, actions, format, extension })
}

fn read_options<'a>(scan: &mut Scan<'a>, grammar: &Grammar) -> Result<(Option<String>, Vec<Segment>), ParseError> {
    let mut encoding = None;
    let mut filters: Vec<Segment> = Vec::new();
    while scan.eat(b',') {
        let key = scan.take_until(b":,/");
        let spec = grammar.filter_for(key);
        if spec.is_none() && !Grammar::RESERVED.contains(&key) {
            return Err(ParseError::UnknownOption(key.to_string()));
        }
        let params = read_params(scan);
        match key {
            _ if Grammar::RESERVED.contains(&key) => {
                if params.is_empty() || params.iter().any(String::is_empty) {
                    return Err(ParseError::EmptyOptionValue(key.to_string()));
                }
                for value in &params {
                    validate_value(value)?;
                }
                let [value] = params.as_slice() else { return Err(ParseError::OptionTakesOneValue(key.to_string())) };
                if !filters.is_empty() {
                    return Err(ParseError::MisplacedSourceOption(key.to_string()));
                }
                if encoding.is_some() {
                    return Err(ParseError::RepeatedOption(key.to_string()));
                }
                encoding = Some(value.clone());
            }
            _ => {
                let segment = finish_segment(key, params, spec.expect("checked above"))?;
                if filters.len() == MAX_FILTERS {
                    return Err(ParseError::TooManyFilters);
                }
                filters.push(segment);
            }
        }
    }
    Ok((encoding, filters))
}

fn action_at(name: &str, grammar: &Grammar) -> Option<usize> {
    name.match_indices("/@")
        .find(|(at, _)| {
            let rest = &name[at + 2..];
            let end = rest.find('/').unwrap_or(rest.len());
            grammar.action_for(&rest[..end]).is_some()
        })
        .map(|(at, _)| at)
}

fn buried_action<'g>(name: &str, grammar: &'g Grammar) -> Option<&'g str> {
    let at = action_at(name, grammar)?;
    let rest = &name[at + 2..];
    let end = rest.find('/').unwrap_or(rest.len());
    grammar.action_for(&rest[..end]).map(|def| def.canonical())
}

fn read_params(scan: &mut Scan<'_>) -> Vec<String> {
    let mut params: Vec<String> = Vec::new();
    while scan.eat(b':') {
        params.push(scan.take_value(b":,/").to_string());
    }
    params
}

fn finish_segment(key: &str, params: Vec<String>, def: &SegmentDef) -> Result<Segment, ParseError> {
    if params.iter().any(String::is_empty) || (params.is_empty() && !def.accepts(0)) {
        return Err(ParseError::EmptyOptionValue(key.to_string()));
    }
    for value in &params {
        validate_value(value)?;
    }
    if !def.accepts(params.len()) {
        return Err(ParseError::WrongParameterCount(key.to_string()));
    }
    Ok(Segment { name: def.canonical().to_string(), params })
}

fn read_action_segments(scan: &mut Scan<'_>, def: &ActionDef) -> Result<Vec<Segment>, ParseError> {
    let mut out: Vec<Segment> = Vec::new();
    loop {
        let key = scan.take_until(b":,/");
        if key.is_empty() {
            return Err(ParseError::ActionWithoutOptions(def.canonical().to_string()));
        }
        let Some(option) = def.option(key) else {
            return Err(ParseError::UnknownOption(key.to_string()));
        };
        let segment = finish_segment(key, read_params(scan), option)?;
        if out.iter().any(|seen| seen.name == segment.name) {
            return Err(ParseError::RepeatedOption(segment.name.clone()));
        }
        if out.len() == MAX_OPTIONS {
            return Err(ParseError::TooManyOptions);
        }
        out.push(segment);
        if !scan.eat(b',') {
            return Ok(out);
        }
    }
}

struct Scan<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Scan<'a> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            return true;
        }
        false
    }

    fn advance(&mut self, by: usize) {
        self.at += by;
    }

    fn rewind(&mut self, by: usize) {
        self.at -= by;
    }

    fn action_name(&self) -> Option<&'a str> {
        let bytes = self.text.as_bytes();
        if bytes.get(self.at) != Some(&b'/') || bytes.get(self.at + 1) != Some(&b'@') {
            return None;
        }
        let rest = &self.text[self.at + 2..];
        let end = rest.find('/')?;
        (end > 0).then_some(&rest[..end])
    }

    fn take_until(&mut self, stops: &[u8]) -> &'a str {
        let from = self.at;
        while let Some(byte) = self.peek() {
            if stops.contains(&byte) {
                break;
            }
            self.at += 1;
        }
        &self.text[from..self.at]
    }

    fn take_value(&mut self, stops: &[u8]) -> &'a str {
        let from = self.at;
        let mut depth = 0usize;
        let mut in_tilde = false;
        while let Some(byte) = self.peek() {
            match byte {
                b'~' => in_tilde = !in_tilde,
                b'(' if !in_tilde => depth += 1,
                b')' if !in_tilde => depth = depth.saturating_sub(1),
                _ if depth == 0 && !in_tilde && stops.contains(&byte) => break,
                _ => {}
            }
            self.at += 1;
        }
        &self.text[from..self.at]
    }
}

fn validate_value(value: &str) -> Result<(), ParseError> {
    let mut depth = 0i32;
    let mut in_tilde = false;
    for byte in value.bytes() {
        match byte {
            b'~' => in_tilde = !in_tilde,
            b'(' if !in_tilde => depth += 1,
            b')' if !in_tilde => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return Err(ParseError::UnbalancedValue(value.to_string()));
        }
        if depth as usize > crate::parexp::MAX_DEPTH {
            return Err(ParseError::ValueTooDeep(value.to_string()));
        }
    }
    match depth == 0 && !in_tilde {
        true => Ok(()),
        false => Err(ParseError::UnbalancedValue(value.to_string())),
    }
}

fn trim_filename(path: &str) -> &str {
    let mut at = 0;
    let mut readable = None;
    for part in path.split('/') {
        let end = at + part.len();
        if crate::dataset::looks_like_source(&path[at..end]) {
            readable = Some(end);
        }
        at = end + 1;
    }
    readable.map_or(path, |end| &path[..end])
}

fn resolve_encoding(label: &str) -> Option<String> {
    if let Some((_, name)) = crate::encodings::ALIASES.iter().find(|(alias, _)| alias.eq_ignore_ascii_case(label)) {
        return Some(name.to_string());
    }
    let safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':');
    (!label.is_empty() && label.chars().all(safe)).then(|| label.to_string())
}

pub fn is_valid_path(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~' | '(' | ')'))
}

pub fn is_valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::{Param, action};
    use rstest::rstest;

    fn test_grammar() -> Grammar {
        let mut g = Grammar::core();
        g.action(
            action("probe")
                .alias("p")
                .option(&["aa", "a"], &[&[]])
                .option(&["bb"], &[&[]])
                .option(&["val"], &[&[Param::Value]])
                .build(),
        );
        g
    }

    #[test]
    fn core_rejects_an_action_it_does_not_know() {
        let err = parse("/@dataset:pts/@probe/aa.json", &Grammar::core()).unwrap_err();
        assert_eq!(err, ParseError::UnknownAction("probe".to_string()));
    }

    #[test]
    fn a_registered_action_parses_with_its_options() {
        let out = parse("/@dataset:pts/@probe/aa,bb.json", &test_grammar()).unwrap();
        assert_eq!(out.actions.len(), 1);
        assert_eq!(out.actions[0].name, "probe");
        let names: Vec<&str> = out.actions[0].segments.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["aa", "bb"]);
        assert!(out.actions[0].segments[0].params.is_empty(), "an option with no shape takes no parameters");
    }

    #[rstest]
    #[case("/@dataset:pts/@p/aa.json")]
    #[case("/@dataset:pts/@probe/a.json")]
    fn an_action_and_its_options_answer_to_their_aliases(#[case] url: &str) {
        let out = parse(url, &test_grammar()).unwrap();
        assert_eq!(out.actions[0].name, "probe", "the canonical action name is stored");
        assert_eq!(out.actions[0].segments[0].name, "aa", "the canonical option name is stored");
    }

    #[rstest]
    #[case("/@dataset:pts/@probe/zz.json", ParseError::UnknownOption("zz".to_string()))]
    #[case("/@dataset:pts/@probe/.json", ParseError::ActionWithoutOptions("probe".to_string()))]
    #[case("/@dataset:pts/@probe/val.json", ParseError::EmptyOptionValue("val".to_string()))]
    #[case("/@dataset:pts/@probe/val:.json", ParseError::EmptyOptionValue("val".to_string()))]
    #[case("/@dataset:pts/@probe/aa:1.json", ParseError::WrongParameterCount("aa".to_string()))]
    #[case("/@dataset:pts/@probe/aa,aa.json", ParseError::RepeatedOption("aa".to_string()))]
    #[case("/@dataset:pts/@probe/aa,a.json", ParseError::RepeatedOption("aa".to_string()))]
    fn an_action_option_is_held_to_its_registration(#[case] url: &str, #[case] expected: ParseError) {
        assert_eq!(parse(url, &test_grammar()), Err(expected));
    }

    #[test]
    fn an_action_survives_an_archive_sub_path() {
        let out = parse("/@dataset:uk:gb.shp/@probe/aa.json", &test_grammar()).unwrap();
        assert_eq!(out.path.as_deref(), Some("gb.shp"));
        assert_eq!(out.actions.len(), 1, "the sub-path swallowed the action");
    }

    #[rstest]
    #[case("/@dataset:uk:gb.shp/extra/@probe/aa.json")]
    #[case("/@dataset:uk:gb.shp/a/b/@p/aa.json")]
    fn an_action_buried_behind_a_download_name_is_rejected_not_dropped(#[case] url: &str) {
        assert_eq!(parse(url, &test_grammar()), Err(ParseError::MisplacedAction("probe".to_string())));
    }

    #[test]
    fn a_download_name_that_buries_no_registered_action_is_still_accepted() {
        let out = parse("/@dataset:uk:gb.shp/extra/@nope/aa.json", &test_grammar()).unwrap();
        assert_eq!(out.path.as_deref(), Some("gb.shp"));
        assert!(out.actions.is_empty());
    }

    #[test]
    fn an_at_prefixed_name_with_a_slash_is_read_as_an_action_not_a_filename() {
        assert_eq!(
            parse("/@dataset:pts/@a/b.json", &Grammar::core()),
            Err(ParseError::UnknownAction("a".to_string())),
            "an unknown action must not fall through to the filename slot"
        );
    }

    #[test]
    fn exactly_the_cap_is_accepted() {
        let actions: String = (0..MAX_ACTIONS).map(|_| "/@probe/aa".to_string()).collect();
        let out = parse(&format!("/@dataset:x{actions}.json"), &test_grammar()).unwrap();
        assert_eq!(out.actions.len(), MAX_ACTIONS);

        let mut g = Grammar::core();
        let mut builder = action("probe");
        for name in OPTION_NAMES {
            builder = builder.option(&[name], &[&[]]);
        }
        g.action(builder.build());
        let options: Vec<&str> = OPTION_NAMES.iter().take(MAX_OPTIONS).copied().collect();
        let out = parse(&format!("/@dataset:x/@probe/{}.json", options.join(",")), &g).unwrap();
        assert_eq!(out.actions[0].segments.len(), MAX_OPTIONS);
    }

    #[rstest]
    #[case("/@dataset:uk:gb.shp/@probe/aa/report.shp.json", "gb.shp")]
    #[case("/@dataset:uk:gb.shp/@probe/aa/report.json", "gb.shp")]
    #[case("/@dataset:uk:gb.shp/@probe/aa.json", "gb.shp")]
    #[case("/@dataset:uk:a/b.geojson/c.shp/@probe/aa/out.shp.json", "a/b.geojson/c.shp")]
    #[case("/@dataset:uk:member/@probe/aa.json", "member")]
    fn a_download_name_that_looks_like_a_source_does_not_swallow_the_action(#[case] url: &str, #[case] path: &str) {
        let out = parse(url, &test_grammar()).unwrap();
        assert_eq!(out.path.as_deref(), Some(path));
        assert_eq!(out.actions.len(), 1, "the action was lost: {out:?}");
        assert_eq!(out.actions[0].segments[0].name, "aa");
    }

    #[test]
    fn the_sub_path_stops_at_the_first_action_not_the_last() {
        let out = parse("/@dataset:uk:gb.shp/@probe/val:x.shp/@probe/bb/out.json", &test_grammar()).unwrap();
        assert_eq!(out.path.as_deref(), Some("gb.shp"));
        assert_eq!(out.actions.len(), 2, "an option value ending in .shp moved the boundary: {out:?}");
    }

    #[test]
    fn a_sub_path_that_is_only_an_action_marker_is_an_empty_source_value() {
        assert_eq!(parse("/@dataset:uk:/@probe/aa.json", &test_grammar()), Err(ParseError::EmptySourceValue));
    }

    #[test]
    fn a_registered_action_without_options_is_not_a_filename() {
        let err = parse("/@dataset:pts/@probe.json", &test_grammar()).unwrap_err();
        assert_eq!(err, ParseError::ActionWithoutOptions("probe".to_string()));
    }

    #[test]
    fn a_download_filename_may_still_start_with_an_at_sign() {
        let out = parse("/@dataset:uk/@2024-export.geojson", &test_grammar()).unwrap();
        assert_eq!(out.dataset, "uk");
        assert!(out.actions.is_empty(), "a filename is not an action");
    }

    #[test]
    fn an_action_may_be_followed_by_a_download_filename() {
        let out = parse("/@dataset:pts/@probe/aa/meta.json", &test_grammar()).unwrap();
        assert_eq!(out.actions.len(), 1);
        assert_eq!(out.dataset, "pts");
    }

    #[rstest]
    #[case("/@dataset:pts,type:Point/@probe/aa.json", "type")]
    #[case("/@dataset:pts,valid:true/@probe/aa.json", "valid")]
    fn a_geometry_predicate_applies_alongside_an_action(#[case] url: &str, #[case] filter: &str) {
        let out = parse(url, &test_grammar()).unwrap();
        assert_eq!(out.filters.len(), 1);
        assert_eq!(out.filters[0].name, filter);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn filters_still_apply_alongside_an_action() {
        let out = parse("/@dataset:pts,id:1/@probe/aa.json", &test_grammar()).unwrap();
        assert_eq!(out.filters.len(), 1);
        assert_eq!(out.actions.len(), 1);
    }

    #[test]
    fn a_tilde_quoted_value_containing_an_action_marker_is_not_an_action() {
        let out = parse("/@dataset:pts,prop:name:~a/@probe~/@probe/aa.json", &test_grammar()).unwrap();
        assert_eq!(out.filters[0].params[1], "~a/@probe~");
        assert_eq!(out.actions.len(), 1, "only the real action counts");
    }

    #[test]
    fn more_than_ten_actions_is_rejected() {
        let many: String = (0..MAX_ACTIONS + 1).map(|_| "/@probe/aa".to_string()).collect();
        assert_eq!(parse(&format!("/@dataset:x{many}.json"), &test_grammar()), Err(ParseError::TooManyActions));
    }

    #[test]
    fn more_than_twenty_options_is_rejected() {
        let mut g = Grammar::core();
        let mut builder = action("probe");
        for name in OPTION_NAMES {
            builder = builder.option(&[name], &[&[]]);
        }
        g.action(builder.build());
        let many: Vec<&str> = OPTION_NAMES.iter().take(MAX_OPTIONS + 1).copied().collect();
        let url = format!("/@dataset:x/@probe/{}.json", many.join(","));
        assert_eq!(parse(&url, &g), Err(ParseError::TooManyOptions));
    }

    const OPTION_NAMES: [&str; 21] = [
        "o0", "o1", "o2", "o3", "o4", "o5", "o6", "o7", "o8", "o9", "o10", "o11", "o12", "o13", "o14", "o15", "o16",
        "o17", "o18", "o19", "o20",
    ];

    #[rstest]
    #[case("/@dataset:plz-5stellig.json", "plz-5stellig", "json")]
    #[case("/@dataset:cities.GeoJSON", "cities", "geojson")]
    #[case("/@dataset:my.data.geojson", "my.data", "geojson")]
    fn parses_dataset_and_format(#[case] url: &str, #[case] dataset: &str, #[case] format: &str) {
        let p = parse(url, &Grammar::core()).unwrap();
        assert_eq!(p.dataset, dataset);
        assert_eq!(p.extension, format);
    }

    #[rstest]
    #[case("/@dataset:x.geojson", "UTF-8", None)]
    #[case("/@dataset:x,enc:csISOLatin1.geojson", "ISO-8859-1", None)]
    #[case("/@dataset:x,enc:KOI8-R.geojson", "KOI8-R", None)]
    #[case("/@dataset:x:a/b/c.shp,encoding:utf-8.geojson", "UTF-8", Some("a/b/c.shp"))]
    #[case("/@dataset:uk:gb.geojson.geojson", "UTF-8", Some("gb.geojson"))]
    fn parses_path_and_options(#[case] url: &str, #[case] encoding: &str, #[case] path: Option<&str>) {
        let p = parse(url, &Grammar::core()).unwrap();
        assert_eq!(p.encoding, encoding);
        assert_eq!(p.path.as_deref(), path);
    }

    #[rstest]
    #[case("/@dataset:plz-5stellig/output.geojson", "plz-5stellig", None, "UTF-8")]
    #[case("/@dataset:plz-5stellig/map.geojson", "plz-5stellig", None, "UTF-8")]
    #[case("/@dataset:plz-5stellig.geojson", "plz-5stellig", None, "UTF-8")]
    #[case("/@dataset:a/b/c.json", "a", None, "UTF-8")]
    #[case("/@dataset:a/.json", "a", None, "UTF-8")]
    #[case("/@dataset:a/b.c.d.json", "a", None, "UTF-8")]
    #[case("/@dataset:x,enc:iso-8859-1/output.geojson", "x", None, "ISO-8859-1")]
    #[case("/@dataset:x:a/b/c.shp,enc:iso-8859-1/output.geojson", "x", Some("a/b/c.shp"), "ISO-8859-1")]
    #[case("/@dataset:uk:gb.geojson/download.geojson", "uk", Some("gb.geojson"), "UTF-8")]
    #[case("/@dataset:uk:gb.geojson,enc:utf-8/download.geojson", "uk", Some("gb.geojson"), "UTF-8")]
    #[case("/@dataset:uk:ds/ds.gdb/download.geojson", "uk", Some("ds/ds.gdb"), "UTF-8")]
    #[case("/@dataset:uk:a/b.geojson/c.shp/name.geojson", "uk", Some("a/b.geojson/c.shp"), "UTF-8")]
    #[case("/@dataset:uk:layer.shp/report.geojson", "uk", Some("layer.shp"), "UTF-8")]
    #[case("/@dataset:uk:layer.shp/report.json.geojson", "uk", Some("layer.shp/report.json"), "UTF-8")]
    #[case("/@dataset:uk:nothing/readable.geojson", "uk", Some("nothing/readable"), "UTF-8")]
    fn the_filename_segment_is_discarded(
        #[case] url: &str,
        #[case] dataset: &str,
        #[case] path: Option<&str>,
        #[case] encoding: &str,
    ) {
        let p = parse(url, &Grammar::core()).unwrap();
        assert_eq!(p.dataset, dataset);
        assert_eq!(p.path.as_deref(), path);
        assert_eq!(p.encoding, encoding);
    }

    #[rstest]
    #[case("/@ds:cities.geojson", "/@dataset:cities.geojson")]
    #[case("/@ds:plz-5stellig/output.json", "/@dataset:plz-5stellig/output.json")]
    #[case("/@ds:x,enc:iso-8859-1/out.geojson", "/@dataset:x,enc:iso-8859-1/out.geojson")]
    #[case("/@ds:uk:gb.geojson/download.geojson", "/@dataset:uk:gb.geojson/download.geojson")]
    fn the_short_source_prefix_parses_the_same(#[case] short: &str, #[case] long: &str) {
        assert_eq!(parse(short, &Grammar::core()).unwrap(), parse(long, &Grammar::core()).unwrap());
    }

    #[rstest]
    #[case("/", ParseError::Empty)]
    #[case("/@dataset:cities", ParseError::MissingOutputSegment)]
    #[case("/@dataset:cities.xml", ParseError::UnknownFormat("xml".to_string()))]
    #[case("/@elsewhere:thing.json", ParseError::UnknownSource("elsewhere".to_string()))]
    #[case("/@d:thing.json", ParseError::UnknownSource("d".to_string()))]
    #[case("/@datasets:thing.json", ParseError::UnknownSource("datasets".to_string()))]
    #[case("/dataset:cities.json", ParseError::MissingSourcePrefix)]
    #[case("/@dataset:.json", ParseError::EmptySourceValue)]
    #[case("/@dataset.geojson", ParseError::EmptySourceValue)]
    #[case("/@dataset:x,enc:KOI8@8.geojson", ParseError::MalformedEncoding("KOI8@8".to_string()))]
    #[case("/@dataset:pts/export,enc:iso-8859-1.geojson", ParseError::MisplacedOption("export,enc:iso-8859-1".to_string()))]
    #[case("/@dataset:x,enc:.geojson", ParseError::EmptyOptionValue("enc".to_string()))]
    #[case("/@dataset:x,enc.geojson", ParseError::EmptyOptionValue("enc".to_string()))]
    #[case("/@dataset:x,encoding/out.geojson", ParseError::EmptyOptionValue("encoding".to_string()))]
    #[case("/@dataset:x,enc:utf-8:extra.geojson", ParseError::OptionTakesOneValue("enc".to_string()))]
    #[case("/@dataset:x,id:.geojson", ParseError::EmptyOptionValue("id".to_string()))]
    #[case("/@ds:pts,id:~2/export.geojson", ParseError::UnbalancedValue("~2/export".to_string()))]
    #[case("/@dataset:pts,id:~1,enc:latin1.geojson", ParseError::UnbalancedValue("~1,enc:latin1".to_string()))]
    #[case("/@dataset:pts,id:(a~b).geojson", ParseError::UnbalancedValue("(a~b)".to_string()))]
    #[case("/@dataset:pts,id:(1,2.geojson", ParseError::UnbalancedValue("(1,2".to_string()))]
    #[case("/@dataset:pts,id:a)b.geojson", ParseError::UnbalancedValue("a)b".to_string()))]
    #[case(
        "/@dataset:pts,id:((((((((((((((((((((((((((((((((((x)))))))))))))))))))))))))))))))))).geojson",
        ParseError::ValueTooDeep("((((((((((((((((((((((((((((((((((x))))))))))))))))))))))))))))))))))".to_string())
    )]
    #[case("/@dataset:x,id.geojson", ParseError::EmptyOptionValue("id".to_string()))]
    #[case("/@dataset:x,zzz:1.geojson", ParseError::UnknownOption("zzz".to_string()))]
    #[case("/@dataset:gbr:a;b.shp.geojson", ParseError::InvalidPath("a;b.shp".to_string()))]
    #[case("/@dataset:gbr:a b.shp.geojson", ParseError::InvalidPath("a b.shp".to_string()))]
    fn rejects(#[case] url: &str, #[case] expected: ParseError) {
        assert_eq!(parse(url, &Grammar::core()), Err(expected));
    }

    #[rstest]
    #[case("/@dataset:x,id:(~A, B~,C).geojson", "id", vec!["(~A, B~,C)"])]
    #[case("/@dataset:x,id:~Armagh City, Banbridge~.geojson", "id", vec!["~Armagh City, Banbridge~"])]
    #[case("/@dataset:x,id:(1..100).geojson", "id", vec!["(1..100)"])]
    #[case("/@dataset:x,id:~key:value~.geojson", "id", vec!["~key:value~"])]
    #[case("/@dataset:x,id:((1,2),(3,4)).geojson", "id", vec!["((1,2),(3,4))"])]
    #[case("/@dataset:x,id:file-(1..3).txt.geojson", "id", vec!["file-(1..3).txt"])]
    fn a_filter_is_one_segment(#[case] url: &str, #[case] name: &str, #[case] params: Vec<&str>) {
        let p = parse(url, &Grammar::core()).unwrap();
        assert_eq!(p.filters.len(), 1, "{:?}", p.filters);
        assert_eq!(p.filters[0].name, name);
        assert_eq!(p.filters[0].params, params);
    }

    #[rstest]
    #[case("/@dataset:x,prop:state:CA.geojson", vec!["state", "CA"])]
    #[case("/@dataset:x,prop:pop:gt:1000000.geojson", vec!["pop", "gt", "1000000"])]
    #[case("/@dataset:x,prop:deleted:null.geojson", vec!["deleted", "null"])]
    #[case("/@dataset:x,prop:name:in:(~A, B~,C).geojson", vec!["name", "in", "(~A, B~,C)"])]
    fn a_prop_filter_keeps_its_parameters(#[case] url: &str, #[case] params: Vec<&str>) {
        let p = parse(url, &Grammar::core()).unwrap();
        assert_eq!(p.filters.len(), 1, "{:?}", p.filters);
        assert_eq!(p.filters[0].name, "prop");
        assert_eq!(p.filters[0].params, params);
    }

    #[rstest]
    #[case("/@dataset:x,type:Point.geojson", "type", "Point")]
    #[case("/@dataset:x,valid:true.geojson", "valid", "true")]
    #[case("/@dataset:x,empty:false.geojson", "empty", "false")]
    #[case("/@dataset:x,simple:1.geojson", "simple", "1")]
    #[case("/@dataset:x,closed:0.geojson", "closed", "0")]
    fn a_geometry_predicate_keeps_its_one_parameter(#[case] url: &str, #[case] name: &str, #[case] value: &str) {
        let p = parse(url, &Grammar::core()).unwrap();
        assert_eq!(p.filters.len(), 1, "{:?}", p.filters);
        assert_eq!(p.filters[0].name, name);
        assert_eq!(p.filters[0].params, vec![value]);
    }

    #[rstest]
    #[case("/@dataset:x,prop:a:1,prop:b:2.geojson")]
    #[case("/@dataset:x,id:gte:5,id:lte:10.geojson")]
    #[case("/@dataset:x,id:1,prop:a:2.geojson")]
    fn filters_may_repeat_and_mix(#[case] url: &str) {
        assert_eq!(parse(url, &Grammar::core()).unwrap().filters.len(), 2, "{url}");
    }

    #[rstest]
    #[case("/@dataset:x,id:gte:500.geojson", vec!["gte", "500"])]
    #[case("/@dataset:x,id:lt:1000.geojson", vec!["lt", "1000"])]
    #[case("/@dataset:x,id:in:(500..1000).geojson", vec!["in", "(500..1000)"])]
    #[case("/@dataset:x,id:(50..100).geojson", vec!["(50..100)"])]
    #[case("/@dataset:x,id:50.geojson", vec!["50"])]
    fn an_id_filter_may_carry_an_operator(#[case] url: &str, #[case] params: Vec<&str>) {
        let p = parse(url, &Grammar::core()).unwrap();
        assert_eq!(p.filters[0].name, "id");
        assert_eq!(p.filters[0].params, params);
    }

    #[rstest]
    #[case("/@dataset:x,prop:only.geojson", ParseError::WrongParameterCount("prop".to_string()))]
    #[case("/@dataset:x,prop:a:b:c:d.geojson", ParseError::WrongParameterCount("prop".to_string()))]
    #[case("/@dataset:x,valid:true:false.geojson", ParseError::WrongParameterCount("valid".to_string()))]
    #[case("/@dataset:x,type:Point:Line.geojson", ParseError::WrongParameterCount("type".to_string()))]
    #[case("/@dataset:x,closed.geojson", ParseError::EmptyOptionValue("closed".to_string()))]
    #[case("/@dataset:x,id:a:b:c.geojson", ParseError::WrongParameterCount("id".to_string()))]
    #[case("/@dataset:x,prop.geojson", ParseError::EmptyOptionValue("prop".to_string()))]
    fn a_filter_takes_the_parameter_count_its_spec_allows(#[case] url: &str, #[case] expected: ParseError) {
        assert_eq!(parse(url, &Grammar::core()), Err(expected));
    }

    #[test]
    fn more_than_fifty_filters_is_rejected() {
        let many: String = (0..51).map(|i| format!(",prop:k{i}:v")).collect();
        assert_eq!(parse(&format!("/@dataset:x{many}.geojson"), &Grammar::core()), Err(ParseError::TooManyFilters));
        let fifty: String = (0..50).map(|i| format!(",prop:k{i}:v")).collect();
        assert_eq!(parse(&format!("/@dataset:x{fifty}.geojson"), &Grammar::core()).unwrap().filters.len(), 50);
    }

    #[rstest]
    #[case("/@dataset:plz:report~final,enc:latin1.geojson")]
    #[case("/@dataset:plz:file(1,enc:latin1.geojson")]
    fn a_sub_path_does_not_swallow_the_options_after_it(#[case] url: &str) {
        let p = parse(url, &Grammar::core()).unwrap();
        assert!(!p.path.as_deref().unwrap_or_default().contains("enc:"), "the encoding was swallowed: {p:?}");
        assert_eq!(p.encoding, "ISO-8859-1", "{p:?}");
    }

    #[test]
    fn a_deeply_nested_value_is_refused_before_anything_recurses() {
        let deep = format!("/@dataset:x,id:{}x{}.geojson", "(".repeat(5000), ")".repeat(5000));
        assert!(
            matches!(parse(&deep, &Grammar::core()), Err(ParseError::ValueTooDeep(_))),
            "{:?}",
            parse(&deep, &Grammar::core())
        );
    }

    #[test]
    fn nesting_up_to_the_limit_is_accepted() {
        let ok = format!("/@dataset:x,id:{}a,b{}.geojson", "(".repeat(32), ")".repeat(32));
        assert!(parse(&ok, &Grammar::core()).is_ok(), "{:?}", parse(&ok, &Grammar::core()));
    }

    #[test]
    fn an_encoding_comes_before_a_filter_and_not_after() {
        let p = parse("/@dataset:plz,enc:utf-8,id:2257.geojson", &Grammar::core()).unwrap();
        assert_eq!(p.encoding, "UTF-8");
        assert_eq!(p.filters.len(), 1, "encoding was collected as a filter: {:?}", p.filters);
        assert_eq!(p.filters[0].name, "id");
        assert_eq!(p.filters[0].params, ["2257"]);

        assert_eq!(
            parse("/@dataset:plz,id:2257,enc:utf-8.geojson", &Grammar::core()),
            Err(ParseError::MisplacedSourceOption("enc".to_string()))
        );
    }

    #[rstest]
    #[case("/@dataset:x,enc:utf-8,enc:latin1.geojson", "enc")]
    #[case("/@dataset:x,enc:utf-8,encoding:latin1.geojson", "encoding")]
    fn an_option_or_filter_may_appear_only_once(#[case] url: &str, #[case] name: &str) {
        assert_eq!(parse(url, &Grammar::core()), Err(ParseError::RepeatedOption(name.to_string())));
    }
}
