use std::fmt;

use crate::grammar::Grammar;

pub const DEFAULT_ENCODING: &str = crate::encodings::UTF8;
pub const MAX_FILTERS: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub name: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedUrl {
    pub dataset: String,
    pub path: Option<String>,
    pub encoding: String,
    pub filters: Vec<Segment>,
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
            ParseError::TooManyFilters => write!(f, "at most {MAX_FILTERS} filters are allowed"),
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
        if !is_valid_path(value) {
            return Err(ParseError::InvalidPath(value.to_string()));
        }
        path = Some(trim_filename(value).to_string());
    }

    let (encoding, filters) = read_options(&mut scan, grammar)?;

    if scan.eat(b'/') {
        let name = scan.take_until(b"");
        if name.contains(',') || name.contains(':') {
            return Err(ParseError::MisplacedOption(name.to_string()));
        }
    }

    let encoding = match encoding {
        Some(label) => resolve_encoding(&label).ok_or(ParseError::MalformedEncoding(label))?,
        None => DEFAULT_ENCODING.to_string(),
    };

    Ok(ParsedUrl { dataset: name.to_string(), path, encoding, filters, format, extension })
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
        let mut params: Vec<String> = Vec::new();
        while scan.eat(b':') {
            params.push(scan.take_value(b":,/").to_string());
        }
        if params.is_empty() || params.iter().any(String::is_empty) {
            return Err(ParseError::EmptyOptionValue(key.to_string()));
        }
        for value in &params {
            validate_value(value)?;
        }
        match key {
            _ if Grammar::RESERVED.contains(&key) => {
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
                let spec = spec.expect("checked above");
                if !spec.accepts(params.len()) {
                    return Err(ParseError::WrongParameterCount(key.to_string()));
                }
                if filters.len() == MAX_FILTERS {
                    return Err(ParseError::TooManyFilters);
                }
                filters.push(Segment { name: spec.canonical().to_string(), params });
            }
        }
    }
    Ok((encoding, filters))
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
    use rstest::rstest;

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
