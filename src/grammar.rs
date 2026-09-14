#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    Column,
    Operator,
    Value,
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

pub struct ActionDef {
    pub names: Vec<&'static str>,
    pub options: Vec<SegmentDef>,
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
    ActionBuilder { names: vec![name], options: Vec::new() }
}

pub struct ActionBuilder {
    names: Vec<&'static str>,
    options: Vec<SegmentDef>,
}

impl ActionBuilder {
    pub fn alias(mut self, name: &'static str) -> Self {
        self.names.push(name);
        self
    }

    pub fn option(mut self, names: &[&'static str], shapes: &[&[Param]]) -> Self {
        assert!(!names.is_empty(), "an option needs at least one name");
        for (at, name) in names.iter().enumerate() {
            assert_scannable(name);
            if self.options.iter().any(|option| option.matches(name)) || names[..at].contains(name) {
                panic!("option {name:?} is already registered on this action");
            }
        }
        self.options.push(SegmentDef { names: names.to_vec(), shapes: shapes.iter().map(|s| s.to_vec()).collect() });
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
        ActionDef { names: self.names, options: self.options }
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
        self.filters
            .push(SegmentDef { names: names.to_vec(), shapes: shapes.iter().map(|shape| shape.to_vec()).collect() });
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
    use rstest::rstest;

    #[test]
    fn the_core_grammar_registers_the_built_in_filters() {
        let g = Grammar::core();
        let names: Vec<&str> = g.filters.iter().map(SegmentDef::canonical).collect();
        assert_eq!(names, vec!["id", "prop"]);
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
    #[case("id", 1, true)]
    #[case("id", 2, true)]
    #[case("id", 3, false)]
    #[case("prop", 1, false)]
    #[case("prop", 2, true)]
    #[case("prop", 3, true)]
    #[case("prop", 4, false)]
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
    fn an_action_resolves_its_aliases_to_canonical_names() {
        let def = action("probe").alias("p").option(&["aa", "a"], &[&[]]).build();
        assert!(def.matches("probe") && def.matches("p"));
        assert_eq!(def.canonical(), "probe");
        assert_eq!(def.option("a").unwrap().canonical(), "aa");
        assert!(def.option("zz").is_none());
    }

    #[test]
    #[should_panic(expected = "action \"probe\" is already registered")]
    fn registering_an_action_name_twice_panics() {
        let mut g = Grammar::default();
        g.action(action("probe").option(&["aa"], &[&[]]).build());
        g.action(action("probe").option(&["bb"], &[&[]]).build());
    }

    #[test]
    #[should_panic(expected = "action \"p\" is already registered")]
    fn registering_an_action_alias_another_action_owns_panics() {
        let mut g = Grammar::default();
        g.action(action("probe").alias("p").option(&["aa"], &[&[]]).build());
        g.action(action("point").alias("p").option(&["bb"], &[&[]]).build());
    }

    #[test]
    #[should_panic(expected = "action \"probe\" has no options")]
    fn building_an_action_with_no_options_panics() {
        action("probe").build();
    }

    #[test]
    #[should_panic(expected = "option \"aa\" is already registered on this action")]
    fn registering_an_option_name_twice_panics() {
        action("probe").option(&["aa"], &[&[]]).option(&["aa"], &[&[Param::Value]]).build();
    }

    #[test]
    #[should_panic(expected = "action name \"p\" is repeated")]
    fn repeating_an_action_name_inside_one_registration_panics() {
        action("p").alias("p").option(&["aa"], &[&[]]).build();
    }

    #[test]
    #[should_panic(expected = "option \"aa\" is already registered on this action")]
    fn repeating_an_option_name_inside_one_registration_panics() {
        action("probe").option(&["aa", "aa"], &[&[]]).build();
    }

    #[test]
    #[should_panic(expected = "can never be parsed from a url")]
    fn registering_an_action_name_the_scanner_cannot_produce_panics() {
        action("a/b").option(&["aa"], &[&[]]).build();
    }

    #[test]
    #[should_panic(expected = "can never be parsed from a url")]
    fn registering_an_option_name_the_scanner_cannot_produce_panics() {
        action("probe").option(&["a/b"], &[&[]]).build();
    }

    #[test]
    fn an_action_option_may_take_no_parameters_unlike_a_filter() {
        let def = action("probe").option(&["aa"], &[&[]]).build();
        assert!(def.option("aa").unwrap().accepts(0));
    }

    #[test]
    #[should_panic(expected = "a filter needs at least one name")]
    fn registering_a_filter_with_no_name_panics() {
        Grammar::default().filter(&[], &[&[Param::Value]]);
    }
}
