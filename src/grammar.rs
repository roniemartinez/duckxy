#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    Column,
    Operator,
    Value,
    Boolean,
    GeometryType,
}

pub struct SegmentDef {
    pub names: Vec<&'static str>,
    pub shapes: Vec<Vec<Param>>,
}

impl SegmentDef {
    pub fn matches(&self, name: &str) -> bool {
        self.names.contains(&name)
    }

    pub fn canonical(&self) -> &'static str {
        self.names[0]
    }

    pub fn accepts(&self, params: usize) -> bool {
        self.shapes.iter().any(|shape| shape.len() == params)
    }
}

#[derive(Default)]
pub struct Grammar {
    pub filters: Vec<SegmentDef>,
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
            if name.is_empty() || name.contains([':', ',', '/']) {
                panic!("filter name {name:?} can never be parsed from a url");
            }
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
        self.filters
            .push(SegmentDef { names: names.to_vec(), shapes: shapes.iter().map(|shape| shape.to_vec()).collect() });
        self
    }

    pub fn filter_for(&self, name: &str) -> Option<&SegmentDef> {
        self.filters.iter().find(|filter| filter.matches(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn the_core_grammar_registers_the_built_in_filters() {
        let g = Grammar::core();
        let names: Vec<&str> = g.filters.iter().map(SegmentDef::canonical).collect();
        assert_eq!(names, vec!["id", "prop", "type", "valid", "empty", "simple", "closed"]);
    }

    #[test]
    fn the_core_filters_declare_the_kinds_their_parameters_take() {
        let g = Grammar::core();
        assert_eq!(g.filter_for("id").unwrap().shapes, vec![vec![Param::Value], vec![Param::Operator, Param::Value]]);
        assert_eq!(
            g.filter_for("prop").unwrap().shapes,
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
        assert_eq!(Grammar::core().filter_for(name).unwrap().shapes, vec![vec![kind]]);
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

    #[test]
    #[should_panic(expected = "a filter needs at least one name")]
    fn registering_a_filter_with_no_name_panics() {
        Grammar::default().filter(&[], &[&[Param::Value]]);
    }
}
