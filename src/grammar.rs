use sea_query::{Alias, SelectStatement, SimpleExpr};

use crate::sql::Pipeline;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    Column,
    Operator,
    Value,
    Boolean,
    GeometryType,
}

impl Param {
    pub fn label(self) -> &'static str {
        match self {
            Param::Column => "a column name",
            Param::Operator => "an operator",
            Param::Value => "a value",
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
}

impl<'a> StageCtx<'a> {
    pub fn new(pipeline: &'a mut Pipeline) -> Self {
        Self { pipeline }
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

    pub fn geom(&self) -> SimpleExpr {
        sea_query::Expr::col(Alias::new(self.pipeline.geometry()))
    }

    pub fn data(&self) -> Alias {
        self.pipeline.input()
    }

    pub fn cte_once(&mut self, name: &str, build: impl FnOnce(&Alias) -> SelectStatement) -> Alias {
        self.pipeline.cte_once(name, build)
    }

    pub fn cte(&self, name: &str) -> Option<Alias> {
        self.pipeline.cte(name)
    }
}

pub type ErasedFragment = Box<dyn Fn(&mut StageCtx, &[String]) -> Result<SimpleExpr, ParamError> + Send + Sync>;

pub trait IntoFragment<Args> {
    fn shape() -> Vec<Param>;
    fn erase(self) -> ErasedFragment;
}

macro_rules! impl_into_fragment {
    ($($name:ident : $T:ident),*) => {
        impl<F, $($T),*> IntoFragment<($($T,)*)> for F
        where
            F: Fn(&mut StageCtx, $($T),*) -> SimpleExpr + Send + Sync + 'static,
            $($T: FromParam,)*
        {
            fn shape() -> Vec<Param> {
                vec![$($T::KIND),*]
            }

            #[allow(unused_variables, unused_mut, unused_assignments)]
            fn erase(self) -> ErasedFragment {
                Box::new(move |ctx, raw| {
                    let mut at = 0usize;
                    $(
                        let $name = match raw.get(at).and_then(|value| $T::from_param(value)) {
                            Some(typed) => typed,
                            None => {
                                return Err(ParamError {
                                    at,
                                    expected: $T::KIND,
                                    got: raw.get(at).cloned().unwrap_or_default(),
                                });
                            }
                        };
                        at += 1;
                    )*
                    Ok(self(ctx, $($name),*))
                })
            }
        }
    };
}

impl_into_fragment!();
impl_into_fragment!(a: A);
impl_into_fragment!(a: A, b: B);
impl_into_fragment!(a: A, b: B, c: C);

pub struct Shape {
    pub params: Vec<Param>,
    pub fragment: Option<ErasedFragment>,
}

pub type Assemble = fn(&mut StageCtx, Vec<(&'static str, SimpleExpr)>) -> SelectStatement;

pub struct SegmentDef {
    pub names: Vec<&'static str>,
    pub shapes: Vec<Shape>,
}

impl SegmentDef {
    pub fn matches(&self, name: &str) -> bool {
        self.names.contains(&name)
    }

    pub fn canonical(&self) -> &'static str {
        self.names[0]
    }

    pub fn accepts(&self, params: usize) -> bool {
        self.shape_for(params).is_some()
    }

    pub fn shape_for(&self, params: usize) -> Option<&Shape> {
        self.shapes.iter().find(|shape| shape.params.len() == params)
    }
}

pub struct ActionDef {
    pub names: Vec<&'static str>,
    pub options: Vec<SegmentDef>,
    pub assemble: Assemble,
}

impl ActionDef {
    pub fn matches(&self, name: &str) -> bool {
        self.names.contains(&name)
    }

    pub fn canonical(&self) -> &'static str {
        self.names[0]
    }

    pub fn option(&self, name: &str) -> Option<&SegmentDef> {
        self.options.iter().find(|option| option.matches(name))
    }
}

pub fn action(name: &'static str) -> ActionBuilder {
    ActionBuilder { names: vec![name], options: Vec::new(), assemble: None }
}

pub struct ActionBuilder {
    names: Vec<&'static str>,
    options: Vec<SegmentDef>,
    assemble: Option<Assemble>,
}

impl ActionBuilder {
    pub fn alias(mut self, name: &'static str) -> Self {
        self.names.push(name);
        self
    }

    pub fn terminal(mut self, assemble: Assemble) -> Self {
        self.assemble = Some(assemble);
        self
    }

    pub fn option<F, Args>(mut self, names: &[&'static str], fragment: F) -> Self
    where
        F: IntoFragment<Args>,
    {
        assert!(!names.is_empty(), "an option needs at least one name");
        for (at, name) in names.iter().enumerate() {
            assert_scannable(name);
            if names[..at].contains(name) {
                panic!("option {name:?} is repeated in its own registration");
            }
        }
        let shape = Shape { params: F::shape(), fragment: Some(fragment.erase()) };
        match self.options.iter_mut().find(|option| names.iter().any(|name| option.matches(name))) {
            Some(existing) if existing.names == names => existing.shapes.push(shape),
            Some(existing) => {
                panic!("option {:?} is already registered on this action as {:?}", names, existing.names)
            }
            None => self.options.push(SegmentDef { names: names.to_vec(), shapes: vec![shape] }),
        }
        if let Some(existing) = self.options.iter().find(|option| option.names == names) {
            let mut arities: Vec<usize> = existing.shapes.iter().map(|shape| shape.params.len()).collect();
            let total = arities.len();
            arities.sort_unstable();
            arities.dedup();
            assert_eq!(arities.len(), total, "option {:?} registered two shapes of the same arity", names[0]);
        }
        self
    }

    pub fn build(self) -> ActionDef {
        assert!(!self.options.is_empty(), "action {:?} has no options", self.names[0]);
        for (at, name) in self.names.iter().enumerate() {
            assert_scannable(name);
            if self.names[..at].contains(name) {
                panic!("action name {name:?} is repeated in its own registration");
            }
        }
        ActionDef {
            assemble: self.assemble.unwrap_or_else(|| panic!("action {:?} has no terminal", self.names[0])),
            names: self.names,
            options: self.options,
        }
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
    pub actions: Vec<ActionDef>,
}

impl Grammar {
    pub const RESERVED: [&'static str; 2] = ["enc", "encoding"];

    pub fn core() -> Grammar {
        let mut g = Grammar::default();
        g.filter(&["id"], &[&[Param::Value], &[Param::Operator, Param::Value]]);
        g.filter(&["prop"], &[&[Param::Column, Param::Value], &[Param::Column, Param::Operator, Param::Value]]);
        g.filter(&["type"], &[&[Param::GeometryType]]);
        g.filter(&["valid"], &[&[Param::Boolean]]);
        g.filter(&["empty"], &[&[Param::Boolean]]);
        g.filter(&["simple"], &[&[Param::Boolean]]);
        g.filter(&["closed"], &[&[Param::Boolean]]);
        g
    }

    pub fn filter(&mut self, names: &[&'static str], shapes: &[&[Param]]) -> &mut Self {
        assert!(!names.is_empty(), "a filter needs at least one name");
        assert!(!shapes.iter().any(|shape| shape.is_empty()), "a filter shape needs at least one parameter");
        for (at, name) in names.iter().enumerate() {
            assert_scannable(name);
            if Grammar::RESERVED.contains(name) {
                panic!("filter name {name:?} is reserved for the source encoding");
            }
            if self.filter_for(name).is_some() {
                panic!("filter {name:?} is already registered");
            }
            if names[..at].contains(name) {
                panic!("filter name {name:?} is repeated in its own registration");
            }
        }
        self.filters.push(SegmentDef {
            names: names.to_vec(),
            shapes: shapes.iter().map(|params| Shape { params: params.to_vec(), fragment: None }).collect(),
        });
        self
    }

    pub fn action(&mut self, def: ActionDef) -> &mut Self {
        for name in &def.names {
            if self.action_for(name).is_some() {
                panic!("action {name:?} is already registered");
            }
        }
        self.actions.push(def);
        self
    }

    pub fn filter_for(&self, name: &str) -> Option<&SegmentDef> {
        self.filters.iter().find(|filter| filter.matches(name))
    }

    pub fn action_for(&self, name: &str) -> Option<&ActionDef> {
        self.actions.iter().find(|action| action.matches(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> std::sync::Arc<crate::backend::Backend> {
        std::sync::Arc::new(crate::backend::Backend::default())
    }
    use rstest::rstest;
    use sea_query::{Expr, Query};

    fn nothing(_: &mut StageCtx) -> SimpleExpr {
        Expr::cust("1")
    }

    fn one_value(_: &mut StageCtx, _value: Value) -> SimpleExpr {
        Expr::cust("1")
    }

    fn assemble(_: &mut StageCtx, _parts: Vec<(&'static str, SimpleExpr)>) -> SelectStatement {
        Query::select().expr(Expr::cust("1")).take()
    }

    #[test]
    fn the_core_grammar_registers_the_built_in_filters() {
        let g = Grammar::core();
        let names: Vec<&str> = g.filters.iter().map(SegmentDef::canonical).collect();
        assert_eq!(names, vec!["id", "prop", "type", "valid", "empty", "simple", "closed"]);
    }

    fn kinds(def: &SegmentDef) -> Vec<Vec<Param>> {
        def.shapes.iter().map(|shape| shape.params.clone()).collect()
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
    fn an_alias_resolves_to_the_canonical_name() {
        let mut g = Grammar::default();
        g.filter(&["property", "prop", "p"], &[&[Param::Column, Param::Value]]);
        for name in ["property", "prop", "p"] {
            assert_eq!(g.filter_for(name).unwrap().canonical(), "property");
        }
        assert!(g.filter_for("nope").is_none());
    }

    #[test]
    #[should_panic(expected = "filter \"id\" is already registered")]
    fn registering_a_filter_name_twice_panics() {
        let mut g = Grammar::default();
        g.filter(&["id"], &[&[Param::Value]]);
        g.filter(&["id"], &[&[Param::Value]]);
    }

    #[test]
    #[should_panic(expected = "filter \"p\" is already registered")]
    fn registering_an_alias_that_another_filter_owns_panics() {
        let mut g = Grammar::default();
        g.filter(&["prop", "p"], &[&[Param::Column, Param::Value]]);
        g.filter(&["point", "p"], &[&[Param::Value]]);
    }

    #[test]
    #[should_panic(expected = "filter name \"p\" is repeated")]
    fn repeating_a_name_inside_one_registration_panics() {
        Grammar::default().filter(&["p", "p"], &[&[Param::Value]]);
    }

    #[rstest]
    #[case("enc")]
    #[case("encoding")]
    #[should_panic(expected = "is reserved for the source encoding")]
    fn registering_a_reserved_name_panics(#[case] name: &'static str) {
        Grammar::default().filter(&[name], &[&[Param::Value]]);
    }

    #[rstest]
    #[case("")]
    #[case("a:b")]
    #[case("a,b")]
    #[case("a/b")]
    #[should_panic(expected = "can never be parsed from a url")]
    fn registering_a_name_the_scanner_cannot_produce_panics(#[case] name: &'static str) {
        Grammar::default().filter(&[name], &[&[Param::Value]]);
    }

    #[test]
    #[should_panic(expected = "a filter shape needs at least one parameter")]
    fn registering_a_shape_with_no_parameters_panics() {
        Grammar::default().filter(&["flag"], &[&[]]);
    }

    fn one_boolean(_: &mut StageCtx, _flag: Boolean) -> SimpleExpr {
        Expr::cust("1")
    }

    fn two_params(_: &mut StageCtx, _column: Column, _value: Value) -> SimpleExpr {
        Expr::cust("1")
    }

    fn three_params(_: &mut StageCtx, _c: Column, _o: Operator, _v: Value) -> SimpleExpr {
        Expr::cust("1")
    }

    #[test]
    fn a_fragment_declares_the_shape_of_its_own_signature() {
        let def = action("probe")
            .option(&["none"], nothing)
            .option(&["one"], one_value)
            .option(&["flag"], one_boolean)
            .option(&["two"], two_params)
            .option(&["three"], three_params)
            .terminal(assemble)
            .build();
        assert_eq!(kinds(def.option("none").unwrap()), vec![Vec::new()]);
        assert_eq!(kinds(def.option("one").unwrap()), vec![vec![Param::Value]]);
        assert_eq!(kinds(def.option("flag").unwrap()), vec![vec![Param::Boolean]]);
        assert_eq!(kinds(def.option("two").unwrap()), vec![vec![Param::Column, Param::Value]]);
        assert_eq!(kinds(def.option("three").unwrap()), vec![vec![Param::Column, Param::Operator, Param::Value]]);
    }

    #[test]
    fn registering_an_option_twice_adds_an_alternative_shape() {
        let def = action("probe").option(&["id"], one_value).option(&["id"], two_params).terminal(assemble).build();
        assert_eq!(kinds(def.option("id").unwrap()), vec![vec![Param::Value], vec![Param::Column, Param::Value]]);
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
    fn a_fragment_reports_which_parameter_was_wrong() {
        let def = action("probe").option(&["flag"], one_boolean).terminal(assemble).build();
        let shape = def.option("flag").unwrap().shape_for(1).unwrap();
        let fragment = shape.fragment.as_ref().unwrap();
        let mut pipeline =
            Pipeline::source("/x.geojson", "UTF-8", &[("geom".to_string(), "GEOMETRY".to_string())], backend())
                .unwrap();
        let mut ctx = StageCtx { pipeline: &mut pipeline };
        let err = fragment(&mut ctx, &["banana".to_string()]).unwrap_err();
        assert_eq!(err, ParamError { at: 0, expected: Param::Boolean, got: "banana".to_string() });
        assert_eq!(err.to_string(), "parameter 1 expected true or false, got \"banana\"");
    }

    #[test]
    #[should_panic(expected = "action \"probe\" has no terminal")]
    fn building_an_action_without_a_terminal_panics() {
        action("probe").option(&["aa"], nothing).build();
    }

    #[test]
    fn an_action_resolves_its_aliases_to_canonical_names() {
        let def = action("probe").alias("p").option(&["aa", "a"], nothing).terminal(assemble).build();
        assert!(def.matches("probe") && def.matches("p"));
        assert_eq!(def.canonical(), "probe");
        assert_eq!(def.option("a").unwrap().canonical(), "aa");
        assert!(def.option("zz").is_none());
    }

    #[test]
    #[should_panic(expected = "action \"probe\" is already registered")]
    fn registering_an_action_name_twice_panics() {
        let mut g = Grammar::default();
        g.action(action("probe").option(&["aa"], nothing).terminal(assemble).build());
        g.action(action("probe").option(&["bb"], nothing).terminal(assemble).build());
    }

    #[test]
    #[should_panic(expected = "action \"p\" is already registered")]
    fn registering_an_action_alias_another_action_owns_panics() {
        let mut g = Grammar::default();
        g.action(action("probe").alias("p").option(&["aa"], nothing).terminal(assemble).build());
        g.action(action("point").alias("p").option(&["bb"], nothing).terminal(assemble).build());
    }

    #[test]
    #[should_panic(expected = "action \"probe\" has no options")]
    fn building_an_action_with_no_options_panics() {
        action("probe").terminal(assemble).build();
    }

    #[test]
    #[should_panic(expected = "is already registered on this action")]
    fn reusing_an_option_alias_for_a_different_option_panics() {
        action("probe").option(&["aa", "a"], nothing).option(&["bb", "a"], one_value).terminal(assemble).build();
    }

    #[test]
    #[should_panic(expected = "is already registered on this action")]
    fn redeclaring_an_option_with_different_aliases_panics() {
        action("probe").option(&["aa"], nothing).option(&["aa", "a"], one_value).terminal(assemble).build();
    }

    #[test]
    #[should_panic(expected = "registered two shapes of the same arity")]
    fn registering_two_shapes_of_the_same_arity_panics() {
        action("probe").option(&["aa"], one_value).option(&["aa"], one_boolean).terminal(assemble).build();
    }

    #[test]
    #[should_panic(expected = "action name \"p\" is repeated")]
    fn repeating_an_action_name_inside_one_registration_panics() {
        action("p").alias("p").option(&["aa"], nothing).terminal(assemble).build();
    }

    #[test]
    #[should_panic(expected = "option \"aa\" is repeated in its own registration")]
    fn repeating_an_option_name_inside_one_registration_panics() {
        action("probe").option(&["aa", "aa"], nothing).terminal(assemble).build();
    }

    #[test]
    #[should_panic(expected = "can never be parsed from a url")]
    fn registering_an_action_name_the_scanner_cannot_produce_panics() {
        action("a/b").option(&["aa"], nothing).terminal(assemble).build();
    }

    #[test]
    #[should_panic(expected = "can never be parsed from a url")]
    fn registering_an_option_name_the_scanner_cannot_produce_panics() {
        action("probe").option(&["a/b"], nothing).terminal(assemble).build();
    }

    #[test]
    fn an_action_option_may_take_no_parameters_unlike_a_filter() {
        let def = action("probe").option(&["aa"], nothing).terminal(assemble).build();
        assert!(def.option("aa").unwrap().accepts(0));
    }

    #[test]
    #[should_panic(expected = "a filter needs at least one name")]
    fn registering_a_filter_with_no_name_panics() {
        Grammar::default().filter(&[], &[&[Param::Value]]);
    }
}
