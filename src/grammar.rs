use sea_query::{Alias, SelectStatement, SimpleExpr};

use crate::sql::Pipeline;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    Column,
    Operator,
    Value,
    Token,
    Boolean,
    GeometryType,
}

impl Param {
    pub fn label(self) -> &'static str {
        match self {
            Param::Column => "a column name",
            Param::Operator => "an operator",
            Param::Value => "a value",
            Param::Token => "letters, digits, dot, dash or underscore",
            Param::Boolean => "true or false",
            Param::GeometryType => "a geometry type",
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ParamError {
    pub at: usize,
    pub expected: Param,
    pub got: String,
}

impl std::fmt::Display for ParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parameter {} expected {}, got {:?}", self.at + 1, self.expected.label(), self.got)
    }
}

impl std::error::Error for ParamError {}

pub trait FromParam: Sized {
    const KIND: Param;
    fn from_param(raw: &str) -> Option<Self>;
}

pub struct Column(pub String);
pub struct Value(pub String);
pub struct Token(pub String);
pub struct Operator(pub String);
pub struct Boolean(pub bool);
pub struct GeometryType(pub &'static str);

impl FromParam for Column {
    const KIND: Param = Param::Column;
    fn from_param(raw: &str) -> Option<Self> {
        (!raw.is_empty()).then(|| Column(raw.to_string()))
    }
}

impl FromParam for Value {
    const KIND: Param = Param::Value;
    fn from_param(raw: &str) -> Option<Self> {
        (!raw.is_empty()).then(|| Value(raw.to_string()))
    }
}

impl FromParam for Token {
    const KIND: Param = Param::Token;
    fn from_param(raw: &str) -> Option<Self> {
        let shaped =
            !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
        shaped.then(|| Token(raw.to_string()))
    }
}

impl FromParam for Operator {
    const KIND: Param = Param::Operator;
    fn from_param(raw: &str) -> Option<Self> {
        (!raw.is_empty()).then(|| Operator(raw.to_string()))
    }
}

impl FromParam for Boolean {
    const KIND: Param = Param::Boolean;
    fn from_param(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "true" | "1" => Some(Boolean(true)),
            "false" | "0" => Some(Boolean(false)),
            _ => None,
        }
    }
}

impl FromParam for GeometryType {
    const KIND: Param = Param::GeometryType;
    fn from_param(raw: &str) -> Option<Self> {
        let wanted = raw.to_ascii_uppercase();
        crate::filters::GEOMETRY_TYPES.iter().find(|known| **known == wanted).map(|known| GeometryType(known))
    }
}

pub struct StageCtx<'a> {
    pipeline: &'a mut Pipeline,
    action: &'static str,
}

impl<'a> StageCtx<'a> {
    pub fn new(pipeline: &'a mut Pipeline, action: &'static str) -> Self {
        Self { pipeline, action }
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

    pub fn geometry(&self) -> &str {
        self.pipeline.geometry()
    }

    pub fn step(&mut self, select: SelectStatement) {
        self.pipeline.step(self.action, select);
    }

    pub fn replace_geometry(&mut self, geometry: SimpleExpr) {
        self.pipeline.replace_geometry(self.action, geometry);
    }

    pub fn geom(&self) -> SimpleExpr {
        sea_query::Expr::col(Alias::new(self.pipeline.geometry()))
    }

    pub fn data(&self) -> Alias {
        self.pipeline.input()
    }

    pub fn cte_once(&mut self, name: &str, build: impl FnOnce(&Alias) -> SelectStatement) -> Alias {
        self.pipeline.cte_once(self.action, name, build)
    }

    pub fn cte(&self, name: &str) -> Option<Alias> {
        self.pipeline.cte(self.action, name)
    }
}

impl Param {
    pub fn accepts(self, raw: &str) -> bool {
        match self {
            Param::Column => Column::from_param(raw).is_some(),
            Param::Operator => Operator::from_param(raw).is_some(),
            Param::Value => Value::from_param(raw).is_some(),
            Param::Token => Token::from_param(raw).is_some(),
            Param::Boolean => Boolean::from_param(raw).is_some(),
            Param::GeometryType => GeometryType::from_param(raw).is_some(),
        }
    }
}

pub struct Opt {
    pub name: &'static str,
    pub short: &'static str,
    pub shapes: &'static [&'static [Param]],
}

pub const fn opt(name: &'static str, short: &'static str, shapes: &'static [&'static [Param]]) -> Opt {
    Opt { name, short, shapes }
}

pub const fn flag(name: &'static str, short: &'static str) -> Opt {
    Opt { name, short, shapes: &[&[]] }
}

pub(crate) trait Declared {
    fn canonical(&self) -> &'static str;
    fn accepts(&self, params: usize) -> bool;
}

impl Opt {
    pub fn matches(&self, name: &str) -> bool {
        self.name == name || (!self.short.is_empty() && self.short == name)
    }

    pub fn shape_for(&self, params: usize) -> Option<&'static [Param]> {
        self.shapes.iter().copied().find(|shape| shape.len() == params)
    }
}

impl Declared for Opt {
    fn canonical(&self) -> &'static str {
        self.name
    }

    fn accepts(&self, params: usize) -> bool {
        self.shapes.iter().any(|shape| shape.len() == params)
    }
}

impl Declared for SegmentDef {
    fn canonical(&self) -> &'static str {
        self.name
    }

    fn accepts(&self, params: usize) -> bool {
        self.shapes.iter().any(|shape| shape.len() == params)
    }
}

pub trait Action: Send + Sync {
    fn name(&self) -> &'static str;
    fn options(&self) -> &'static [Opt];
    fn run(&self, ctx: &mut StageCtx, segments: &[crate::url::Segment]) -> anyhow::Result<()>;

    fn short(&self) -> &'static str {
        ""
    }

    fn matches(&self, name: &str) -> bool {
        self.name() == name || (!self.short().is_empty() && self.short() == name)
    }

    fn canonical(&self) -> &'static str {
        self.name()
    }

    fn option(&self, name: &str) -> Option<&'static Opt> {
        self.options().iter().find(|option| option.matches(name))
    }
}

pub struct SegmentDef {
    pub name: &'static str,
    pub short: &'static str,
    pub shapes: Vec<Vec<Param>>,
}

impl SegmentDef {
    pub fn matches(&self, name: &str) -> bool {
        self.name == name || (!self.short.is_empty() && self.short == name)
    }
}

fn assert_declaration(action: &dyn Action) {
    assert_pair(action.name(), action.short());
    assert!(!action.options().is_empty(), "action {:?} has no options", action.name());
    for (at, declared) in action.options().iter().enumerate() {
        assert_pair(declared.name, declared.short);
        assert!(!declared.shapes.is_empty(), "option {:?} needs at least one shape", declared.name);
        for earlier in &action.options()[..at] {
            if earlier.matches(declared.name) || (!declared.short.is_empty() && earlier.matches(declared.short)) {
                panic!("option {:?} is already registered on this action as {:?}", declared.name, earlier.name);
            }
        }
        let mut arities: Vec<usize> = declared.shapes.iter().map(|shape| shape.len()).collect();
        let total = arities.len();
        arities.sort_unstable();
        arities.dedup();
        assert_eq!(arities.len(), total, "option {:?} registered two shapes of the same arity", declared.name);
    }
}

fn assert_pair(name: &str, short: &str) {
    assert_scannable(name);
    if !short.is_empty() {
        assert_scannable(short);
        assert!(short != name, "short name {short:?} repeats the canonical name");
    }
}

fn assert_scannable(name: &str) {
    if name.is_empty() || name.contains([':', ',', '/']) {
        panic!("name {name:?} can never be parsed from a url");
    }
}

#[derive(Default)]
pub struct Grammar {
    pub filters: Vec<SegmentDef>,
    pub actions: Vec<Box<dyn Action>>,
    pub outputs: Vec<std::sync::Arc<dyn crate::formats::Output>>,
}

impl Grammar {
    pub const RESERVED: [&'static str; 2] = ["enc", "encoding"];

    pub fn core() -> Grammar {
        let mut g = Grammar::default();
        g.register_output(crate::formats::GeoJson);
        g.filter("id", "", &[&[Param::Value], &[Param::Operator, Param::Value]]);
        g.filter("prop", "", &[&[Param::Column, Param::Value], &[Param::Column, Param::Operator, Param::Value]]);
        g.filter("type", "", &[&[Param::GeometryType]]);
        g.filter("valid", "", &[&[Param::Boolean]]);
        g.filter("empty", "", &[&[Param::Boolean]]);
        g.filter("simple", "", &[&[Param::Boolean]]);
        g.filter("closed", "", &[&[Param::Boolean]]);
        g
    }

    pub fn filter(&mut self, name: &'static str, short: &'static str, shapes: &[&[Param]]) -> &mut Self {
        assert!(!shapes.is_empty(), "a filter needs at least one shape");
        assert!(!shapes.iter().any(|shape| shape.is_empty()), "a filter shape needs at least one parameter");
        assert_pair(name, short);
        for candidate in [name, short] {
            if candidate.is_empty() {
                continue;
            }
            if Grammar::RESERVED.contains(&candidate) {
                panic!("filter name {candidate:?} is reserved for the source encoding");
            }
            if self.filter_for(candidate).is_some() {
                panic!("filter {candidate:?} is already registered");
            }
        }
        self.filters.push(SegmentDef { name, short, shapes: shapes.iter().map(|s| s.to_vec()).collect() });
        self
    }

    pub fn register_action(&mut self, action: impl Action + 'static) -> &mut Self {
        assert_declaration(&action);
        for candidate in [action.name(), action.short()] {
            if !candidate.is_empty() && self.action_for(candidate).is_some() {
                panic!("action {candidate:?} is already registered");
            }
        }
        self.actions.push(Box::new(action));
        self
    }

    pub fn register_output(&mut self, output: impl crate::formats::Output + 'static) -> &mut Self {
        assert!(!output.extensions().is_empty(), "an output needs at least one extension");
        for (at, ext) in output.extensions().iter().enumerate() {
            assert_scannable(ext);
            assert!(!ext.starts_with('.') && !ext.ends_with('.'), "extension {ext:?} can never be parsed from a url");
            assert!(
                ext.chars().all(|c| !c.is_ascii_uppercase()),
                "extension {ext:?} must be lowercase; lookup lowercases the url, so an uppercase \
                 registration can never match"
            );
            assert!(!output.extensions()[..at].contains(ext), "extension {ext:?} is repeated in its own registration");
            if let Some(existing) = self.outputs.iter().find(|held| held.extensions().contains(ext))
                && !output.overrides()
            {
                panic!(
                    "extension {ext:?} is already claimed by an output for {:?}; an output sharing an \
                     extension must declare overrides() and gate itself with applies()",
                    existing.extensions()
                );
            }
        }
        self.outputs.push(std::sync::Arc::new(output));
        self
    }

    pub fn filter_for(&self, name: &str) -> Option<&SegmentDef> {
        self.filters.iter().find(|filter| filter.matches(name))
    }

    pub fn action_for(&self, name: &str) -> Option<&dyn Action> {
        self.actions.iter().find(|action| action.matches(name)).map(|held| held.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> std::sync::Arc<crate::backend::Backend> {
        std::sync::Arc::new(crate::backend::Backend::default())
    }
    use rstest::rstest;

    struct Noop(&'static str, &'static str, &'static [Opt]);

    impl Action for Noop {
        fn name(&self) -> &'static str {
            self.0
        }
        fn short(&self) -> &'static str {
            self.1
        }
        fn options(&self) -> &'static [Opt] {
            self.2
        }
        fn run(&self, _: &mut StageCtx, _: &[crate::url::Segment]) -> anyhow::Result<()> {
            Ok(())
        }
    }

    const ONE_AA: &[Opt] = &[opt("aa", "", &[&[Param::Value]])];
    const ONE_BB: &[Opt] = &[opt("bb", "", &[&[Param::Value]])];
    const AA_SHORT: &[Opt] = &[opt("aa", "a", &[&[Param::Value]])];
    const AA_SELF: &[Opt] = &[opt("aa", "aa", &[&[Param::Value]])];
    const AA_FLAG: &[Opt] = &[flag("aa", "")];
    const BAD_OPT: &[Opt] = &[opt("a/b", "", &[&[Param::Value]])];
    const SAME_TWICE: &[Opt] = &[opt("aa", "", &[&[Param::Value]]), opt("aa", "a", &[&[Param::Value]])];
    const SHARED_SHORT: &[Opt] = &[opt("aa", "a", &[&[Param::Value]]), opt("bb", "a", &[&[Param::Value]])];
    const TWO_SAME_ARITY: &[Opt] = &[opt("aa", "", &[&[Param::Value], &[Param::Column]])];

    struct Probe(&'static [&'static str], bool);

    impl crate::formats::Output for Probe {
        fn extensions(&self) -> &'static [&'static str] {
            self.0
        }
        fn content_type(&self) -> &'static str {
            "application/x-test"
        }
        fn overrides(&self) -> bool {
            self.1
        }
        fn rows(&self, ctx: &mut StageCtx, _: &crate::url::ParsedUrl) -> anyhow::Result<sea_query::SelectStatement> {
            Ok(sea_query::Query::select().expr(sea_query::Expr::cust("1")).from(ctx.data()).take())
        }
    }

    #[test]
    fn the_core_grammar_registers_the_built_in_output() {
        let g = Grammar::core();
        assert_eq!(g.outputs.len(), 1);
        assert_eq!(g.outputs[0].extensions(), &["geojson", "json"]);
    }

    #[test]
    #[should_panic(expected = "is already claimed")]
    fn registering_an_output_over_a_claimed_extension_without_overrides_panics() {
        Grammar::core().register_output(Probe(&["json"], false));
    }

    #[test]
    fn an_output_that_declares_the_override_may_share_an_extension() {
        let mut g = Grammar::core();
        g.register_output(Probe(&["json"], true));
        assert_eq!(g.outputs.len(), 2);
    }

    #[test]
    #[should_panic(expected = "must be lowercase")]
    fn registering_an_uppercase_extension_panics() {
        Grammar::default().register_output(Probe(&["GeoJSON"], false));
    }

    #[test]
    #[should_panic(expected = "an output needs at least one extension")]
    fn registering_an_output_with_no_extension_panics() {
        Grammar::default().register_output(Probe(&[], false));
    }

    #[test]
    fn the_core_grammar_registers_the_built_in_filters() {
        let g = Grammar::core();
        let names: Vec<&str> = g.filters.iter().map(SegmentDef::canonical).collect();
        assert_eq!(names, vec!["id", "prop", "type", "valid", "empty", "simple", "closed"]);
    }

    fn kinds(def: &SegmentDef) -> Vec<Vec<Param>> {
        def.shapes.clone()
    }

    fn shapes(option: &Opt) -> Vec<Vec<Param>> {
        option.shapes.iter().map(|shape| shape.to_vec()).collect()
    }

    fn registered(action: impl Action + 'static) -> Grammar {
        let mut g = Grammar::default();
        g.register_action(action);
        g
    }

    #[test]
    fn the_core_filters_declare_the_kinds_their_parameters_take() {
        let g = Grammar::core();
        assert_eq!(kinds(g.filter_for("id").unwrap()), vec![vec![Param::Value], vec![Param::Operator, Param::Value]]);
        assert_eq!(
            kinds(g.filter_for("prop").unwrap()),
            vec![vec![Param::Column, Param::Value], vec![Param::Column, Param::Operator, Param::Value]]
        );
    }

    #[rstest]
    #[case("type", Param::GeometryType)]
    #[case("valid", Param::Boolean)]
    #[case("empty", Param::Boolean)]
    #[case("simple", Param::Boolean)]
    #[case("closed", Param::Boolean)]
    fn a_geometry_predicate_takes_one_parameter_of_its_own_kind(#[case] name: &str, #[case] kind: Param) {
        assert_eq!(kinds(Grammar::core().filter_for(name).unwrap()), vec![vec![kind]]);
    }

    #[rstest]
    #[case("id", 1, true)]
    #[case("id", 2, true)]
    #[case("id", 3, false)]
    #[case("prop", 1, false)]
    #[case("prop", 2, true)]
    #[case("prop", 3, true)]
    #[case("prop", 4, false)]
    #[case("valid", 1, true)]
    #[case("valid", 2, false)]
    #[case("type", 1, true)]
    #[case("type", 2, false)]
    fn a_filter_accepts_only_the_arities_it_registered(
        #[case] name: &str,
        #[case] params: usize,
        #[case] accepted: bool,
    ) {
        assert_eq!(Grammar::core().filter_for(name).unwrap().accepts(params), accepted);
    }

    #[test]
    fn a_short_name_resolves_to_the_canonical_one() {
        let mut g = Grammar::default();
        g.filter("property", "prop", &[&[Param::Column, Param::Value]]);
        for name in ["property", "prop"] {
            assert_eq!(g.filter_for(name).unwrap().canonical(), "property");
        }
        assert!(g.filter_for("nope").is_none());
        assert!(g.filter_for("p").is_none(), "only one short name is registered");
    }

    #[test]
    #[should_panic(expected = "filter \"id\" is already registered")]
    fn registering_a_filter_name_twice_panics() {
        let mut g = Grammar::default();
        g.filter("id", "", &[&[Param::Value]]);
        g.filter("id", "", &[&[Param::Value]]);
    }

    #[test]
    #[should_panic(expected = "filter \"p\" is already registered")]
    fn registering_an_alias_that_another_filter_owns_panics() {
        let mut g = Grammar::default();
        g.filter("prop", "p", &[&[Param::Column, Param::Value]]);
        g.filter("point", "p", &[&[Param::Value]]);
    }

    #[test]
    #[should_panic(expected = "short name \"p\" repeats the canonical name")]
    fn a_filter_short_name_repeating_its_canonical_one_panics() {
        Grammar::default().filter("p", "p", &[&[Param::Value]]);
    }

    #[rstest]
    #[case("enc")]
    #[case("encoding")]
    #[should_panic(expected = "is reserved for the source encoding")]
    fn registering_a_reserved_name_panics(#[case] name: &'static str) {
        Grammar::default().filter(name, "", &[&[Param::Value]]);
    }

    #[rstest]
    #[case("")]
    #[case("a:b")]
    #[case("a,b")]
    #[case("a/b")]
    #[should_panic(expected = "can never be parsed from a url")]
    fn registering_a_name_the_scanner_cannot_produce_panics(#[case] name: &'static str) {
        Grammar::default().filter(name, "", &[&[Param::Value]]);
    }

    #[test]
    #[should_panic(expected = "a filter shape needs at least one parameter")]
    fn registering_a_shape_with_no_parameters_panics() {
        Grammar::default().filter("flag", "", &[&[]]);
    }

    #[test]
    fn an_option_declares_the_kinds_its_parameters_take() {
        const OPTIONS: &[Opt] = &[
            flag("none", ""),
            opt("one", "", &[&[Param::Value]]),
            opt("flag", "", &[&[Param::Boolean]]),
            opt("two", "", &[&[Param::Column, Param::Value]]),
            opt("three", "", &[&[Param::Column, Param::Operator, Param::Value]]),
        ];
        let g = registered(Noop("test", "", OPTIONS));
        let def = g.action_for("test").unwrap();
        assert_eq!(shapes(def.option("none").unwrap()), vec![Vec::new()]);
        assert_eq!(shapes(def.option("one").unwrap()), vec![vec![Param::Value]]);
        assert_eq!(shapes(def.option("flag").unwrap()), vec![vec![Param::Boolean]]);
        assert_eq!(shapes(def.option("two").unwrap()), vec![vec![Param::Column, Param::Value]]);
        assert_eq!(shapes(def.option("three").unwrap()), vec![vec![Param::Column, Param::Operator, Param::Value]]);
    }

    #[test]
    fn an_option_declaring_two_shapes_accepts_both_arities() {
        const OPTIONS: &[Opt] = &[opt("id", "", &[&[Param::Value], &[Param::Column, Param::Value]])];
        let g = registered(Noop("test", "", OPTIONS));
        let def = g.action_for("test").unwrap();
        assert_eq!(shapes(def.option("id").unwrap()), vec![vec![Param::Value], vec![Param::Column, Param::Value]]);
        assert!(def.option("id").unwrap().accepts(1));
        assert!(def.option("id").unwrap().accepts(2));
        assert!(!def.option("id").unwrap().accepts(3));
    }

    #[rstest]
    #[case("true", Some(true))]
    #[case("TRUE", Some(true))]
    #[case("1", Some(true))]
    #[case("false", Some(false))]
    #[case("0", Some(false))]
    #[case("yes", None)]
    #[case("", None)]
    fn a_boolean_parameter_reads_the_same_values_the_filters_accept(#[case] raw: &str, #[case] expected: Option<bool>) {
        assert_eq!(Boolean::from_param(raw).map(|b| b.0), expected);
    }

    #[rstest]
    #[case("Point", Some("POINT"))]
    #[case("linestring", Some("LINESTRING"))]
    #[case("Banana", None)]
    fn a_geometry_type_parameter_is_held_to_the_known_types(#[case] raw: &str, #[case] expected: Option<&str>) {
        assert_eq!(GeometryType::from_param(raw).map(|t| t.0), expected);
    }

    #[test]
    fn a_stage_reads_the_relation_it_was_given() {
        let columns = vec![("geom".to_string(), "GEOMETRY".to_string())];
        let mut pipeline = Pipeline::source("/x.geojson", "UTF-8", &columns, backend()).unwrap();
        let mut ctx = StageCtx::new(&mut pipeline, "test");
        assert_eq!(ctx.geometry(), "geom");
        assert!(ctx.cte("extent").is_none());
        ctx.cte_once("extent", |from| {
            sea_query::Query::select().expr(sea_query::Expr::cust("1")).from(from.clone()).take()
        });
        assert!(ctx.cte("extent").is_some());
    }

    #[test]
    #[should_panic(expected = "action \"test\" has no options")]
    fn registering_an_action_without_options_panics() {
        registered(Noop("test", "", &[]));
    }

    #[test]
    fn an_action_resolves_its_aliases_to_canonical_names() {
        let g = registered(Noop("test", "p", AA_SHORT));
        let def = g.action_for("test").unwrap();
        assert!(def.matches("test") && def.matches("p"));
        assert_eq!(def.canonical(), "test");
        assert_eq!(def.option("a").unwrap().canonical(), "aa");
        assert!(def.option("zz").is_none());
    }

    #[test]
    #[should_panic(expected = "action \"test\" is already registered")]
    fn registering_an_action_name_twice_panics() {
        let mut g = Grammar::default();
        g.register_action(Noop("test", "", ONE_AA));
        g.register_action(Noop("test", "", ONE_BB));
    }

    #[test]
    #[should_panic(expected = "action \"p\" is already registered")]
    fn registering_an_action_alias_another_action_owns_panics() {
        let mut g = Grammar::default();
        g.register_action(Noop("test", "p", ONE_AA));
        g.register_action(Noop("point", "p", ONE_BB));
    }

    #[test]
    #[should_panic(expected = "is already registered on this action")]
    fn reusing_an_option_alias_for_a_different_option_panics() {
        registered(Noop("test", "", SHARED_SHORT));
    }

    #[test]
    #[should_panic(expected = "is already registered on this action")]
    fn redeclaring_an_option_with_different_aliases_panics() {
        registered(Noop("test", "", SAME_TWICE));
    }

    #[test]
    #[should_panic(expected = "registered two shapes of the same arity")]
    fn registering_two_shapes_of_the_same_arity_panics() {
        registered(Noop("test", "", TWO_SAME_ARITY));
    }

    #[test]
    #[should_panic(expected = "short name \"p\" repeats the canonical name")]
    fn repeating_an_action_name_inside_one_registration_panics() {
        registered(Noop("p", "p", ONE_AA));
    }

    #[test]
    #[should_panic(expected = "short name \"aa\" repeats the canonical name")]
    fn an_option_whose_short_name_repeats_its_canonical_one_panics() {
        registered(Noop("test", "", AA_SELF));
    }

    #[test]
    #[should_panic(expected = "can never be parsed from a url")]
    fn registering_an_action_name_the_scanner_cannot_produce_panics() {
        registered(Noop("a/b", "", ONE_AA));
    }

    #[test]
    #[should_panic(expected = "can never be parsed from a url")]
    fn registering_an_option_name_the_scanner_cannot_produce_panics() {
        registered(Noop("test", "", BAD_OPT));
    }

    #[test]
    fn a_flag_takes_no_parameters_unlike_a_filter() {
        let g = registered(Noop("test", "", AA_FLAG));
        let def = g.action_for("test").unwrap();
        assert!(def.option("aa").unwrap().accepts(0));
        assert!(!def.option("aa").unwrap().accepts(1));
    }

    #[test]
    #[should_panic(expected = "can never be parsed from a url")]
    fn registering_a_filter_with_no_name_panics() {
        Grammar::default().filter("", "", &[&[Param::Value]]);
    }
}
