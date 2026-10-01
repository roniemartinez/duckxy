use sea_query::{Alias, Expr, ExprTrait, Func, Query, SimpleExpr};
use std::collections::HashMap;

pub trait SqlFn: Send + Sync {
    fn build(&self, args: Vec<SimpleExpr>) -> SimpleExpr;
}

impl<F> SqlFn for F
where
    F: Fn(Vec<SimpleExpr>) -> SimpleExpr + Send + Sync,
{
    fn build(&self, args: Vec<SimpleExpr>) -> SimpleExpr {
        self(args)
    }
}

#[derive(Default)]
pub struct Vocabulary {
    ops: HashMap<&'static str, Box<dyn SqlFn>>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct UnknownOp(pub String);

impl std::fmt::Display for UnknownOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "this backend has no operation named {:?}", self.0)
    }
}

impl std::error::Error for UnknownOp {}

impl Vocabulary {
    pub fn register(&mut self, name: &'static str, op: impl SqlFn + 'static) -> &mut Self {
        if self.ops.insert(name, Box::new(op)).is_some() {
            panic!("operation {name:?} is already registered on this backend");
        }
        self
    }

    pub fn replace(&mut self, name: &'static str, op: impl SqlFn + 'static) -> &mut Self {
        self.ops.insert(name, Box::new(op));
        self
    }

    pub fn has(&self, name: &str) -> bool {
        self.ops.contains_key(name)
    }

    pub fn call(&self, name: &str, args: Vec<SimpleExpr>) -> Result<SimpleExpr, UnknownOp> {
        match self.ops.get(name) {
            Some(op) => Ok(op.build(args)),
            None => Err(UnknownOp(name.to_string())),
        }
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = self.ops.keys().copied().collect();
        out.sort_unstable();
        out
    }
}

pub trait Dialect: Send + Sync {
    fn transform(&self, geometry: SimpleExpr, from: &str, to: &str) -> SimpleExpr;
    fn as_geojson(&self, geometry: SimpleExpr) -> SimpleExpr;
    fn metres_per_unit(&self, geometry: SimpleExpr, crs: &str) -> SimpleExpr;

    fn vocabulary(&self) -> Vocabulary {
        Vocabulary::default()
    }
}

pub struct DuckDb;

impl Dialect for DuckDb {
    fn transform(&self, geometry: SimpleExpr, from: &str, to: &str) -> SimpleExpr {
        Func::cust("ST_Transform")
            .arg(geometry)
            .arg(from.to_uppercase())
            .arg(to.to_uppercase())
            .arg(Expr::cust("always_xy := true"))
            .into()
    }

    fn as_geojson(&self, geometry: SimpleExpr) -> SimpleExpr {
        Func::cust("ST_AsGeoJSON").arg(geometry).into()
    }

    fn metres_per_unit(&self, geometry: SimpleExpr, crs: &str) -> SimpleExpr {
        let here = Query::select().expr_as(Func::cust("ST_Centroid").arg(geometry), Alias::new("p")).take();
        let at = || Expr::col(Alias::new("p"));
        let north = Func::cust("ST_Point")
            .arg(Func::cust("ST_X").arg(at()))
            .arg(SimpleExpr::from(Func::cust("ST_Y").arg(at())).add(1))
            .into();
        let shifted = Query::select()
            .expr_as(self.transform(at(), crs, crate::sql::WGS84), Alias::new("a"))
            .expr_as(self.transform(north, crs, crate::sql::WGS84), Alias::new("b"))
            .from_subquery(here, Alias::new("centre"))
            .take();
        let latitude_first = |name: &'static str| {
            Func::cust("ST_Point")
                .arg(Func::cust("ST_Y").arg(Expr::col(Alias::new(name))))
                .arg(Func::cust("ST_X").arg(Expr::col(Alias::new(name))))
        };
        let measured = Query::select()
            .expr(Func::cust("ST_Distance_Spheroid").arg(latitude_first("a")).arg(latitude_first("b")))
            .from_subquery(shifted, Alias::new("apart"))
            .take();
        SimpleExpr::SubQuery(None, Box::new(sea_query::SubQueryStatement::SelectStatement(measured)))
    }

    fn vocabulary(&self) -> Vocabulary {
        let mut vocabulary = Vocabulary::default();
        let called = crate::process::UNARY
            .iter()
            .map(|op| op.call)
            .chain(crate::process::PARAMETERISED.iter().map(|op| op.call));
        for call in called {
            vocabulary.register(call, move |args: Vec<SimpleExpr>| Func::cust(call).args(args).into());
        }
        vocabulary
    }
}

pub struct Backend {
    dialect: Box<dyn Dialect>,
    vocabulary: Vocabulary,
}

impl Backend {
    pub fn new(dialect: impl Dialect + 'static) -> Self {
        let vocabulary = dialect.vocabulary();
        Self { dialect: Box::new(dialect), vocabulary }
    }

    pub fn extend(mut self, with: impl FnOnce(&mut Vocabulary)) -> Self {
        with(&mut self.vocabulary);
        self
    }

    pub fn dialect(&self) -> &dyn Dialect {
        self.dialect.as_ref()
    }

    pub fn call(&self, name: &str, args: Vec<SimpleExpr>) -> Result<SimpleExpr, UnknownOp> {
        self.vocabulary.call(name, args)
    }

    pub fn has(&self, name: &str) -> bool {
        self.vocabulary.has(name)
    }

    pub fn operations(&self) -> Vec<&'static str> {
        self.vocabulary.names()
    }
}

impl Default for Backend {
    fn default() -> Self {
        Backend::new(DuckDb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use sea_query::{Alias, PostgresQueryBuilder, Query};

    fn rendered(expr: SimpleExpr) -> String {
        Query::select().expr(expr).to_owned().to_string(PostgresQueryBuilder).trim_start_matches("SELECT ").to_string()
    }

    fn geom() -> SimpleExpr {
        Expr::col(Alias::new("geom"))
    }

    #[test]
    fn core_only_depends_on_three_dialect_methods() {
        let d = DuckDb;
        assert_eq!(rendered(d.as_geojson(geom())), "ST_AsGeoJSON(\"geom\")");
        assert_eq!(
            rendered(d.transform(geom(), "EPSG:25832", "EPSG:3857")),
            "ST_Transform(\"geom\", 'EPSG:25832', 'EPSG:3857', always_xy := true)"
        );
        let measured = rendered(d.metres_per_unit(geom(), "epsg:25832"));
        assert!(measured.starts_with("(SELECT ST_Distance_Spheroid("), "{measured}");
        assert_eq!(measured.matches("'EPSG:25832'").count(), 2, "the crs is not measured at both ends: {measured}");
        assert_eq!(measured.matches("ST_Centroid").count(), 1, "the geometry is walked more than once: {measured}");
    }

    #[rstest]
    #[case("epsg:25832", "esri:54030", "'EPSG:25832', 'ESRI:54030'")]
    #[case("ogc:crs84", "ignf:lamb93", "'OGC:CRS84', 'IGNF:LAMB93'")]
    fn duckdb_uppercases_both_ends_because_proj_is_case_sensitive(
        #[case] from: &str,
        #[case] to: &str,
        #[case] expected: &str,
    ) {
        assert!(rendered(DuckDb.transform(geom(), from, to)).contains(expected));
    }

    struct Bare;

    impl Dialect for Bare {
        fn transform(&self, g: SimpleExpr, _: &str, _: &str) -> SimpleExpr {
            g
        }
        fn metres_per_unit(&self, _: SimpleExpr, _: &str) -> SimpleExpr {
            Expr::val(1.0)
        }

        fn as_geojson(&self, g: SimpleExpr) -> SimpleExpr {
            g
        }
    }

    fn seeded() -> Backend {
        Backend::new(Bare).extend(|v| {
            v.register("bounds", |mut args: Vec<SimpleExpr>| Func::cust("ST_Extent_Agg").arg(args.remove(0)).into());
        })
    }

    #[test]
    fn duckdb_registers_exactly_the_operations_core_calls() {
        let mut called: Vec<&str> = crate::process::UNARY
            .iter()
            .map(|op| op.call)
            .chain(crate::process::PARAMETERISED.iter().map(|op| op.call))
            .collect();
        called.sort_unstable();
        let registered = Backend::default().operations();
        let unregistered: Vec<&&str> = called.iter().filter(|name| !registered.contains(name)).collect();
        let uncalled: Vec<&&str> = registered.iter().filter(|name| !called.contains(name)).collect();
        assert!(unregistered.is_empty(), "core calls an operation duckdb never registered: {unregistered:?}");
        assert!(uncalled.is_empty(), "duckdb registered an operation core never calls: {uncalled:?}");
    }

    #[rstest]
    #[case("ST_Centroid", "ST_Centroid(\"geom\")")]
    #[case("ST_MinimumRotatedRectangle", "ST_MinimumRotatedRectangle(\"geom\")")]
    fn a_duckdb_operation_renders_as_its_function(#[case] name: &str, #[case] expected: &str) {
        assert_eq!(rendered(Backend::default().call(name, vec![geom()]).unwrap()), expected);
    }

    #[test]
    fn an_unknown_operation_names_itself() {
        let err = Backend::default().call("banana", vec![geom()]).unwrap_err();
        assert_eq!(err, UnknownOp("banana".to_string()));
        assert_eq!(err.to_string(), "this backend has no operation named \"banana\"");
    }

    #[test]
    fn a_vocabulary_reports_what_it_holds() {
        let backend = seeded();
        assert_eq!(backend.operations(), vec!["bounds"]);
        assert!(backend.has("bounds"));
        assert!(!backend.has("banana"));
    }

    #[test]
    fn an_operation_core_never_named_can_be_added() {
        let backend = seeded().extend(|v| {
            v.register("merged", |mut args: Vec<SimpleExpr>| {
                Func::cust("ST_Whatever").arg(Func::cust("ST_Union_Agg").arg(args.remove(0))).into()
            });
        });
        assert_eq!(rendered(backend.call("merged", vec![geom()]).unwrap()), "ST_Whatever(ST_Union_Agg(\"geom\"))");
        assert!(backend.has("bounds"), "extending must not drop what was registered before it");
    }

    #[test]
    fn an_operation_can_be_replaced() {
        let backend = seeded().extend(|v| {
            v.replace("bounds", |mut args: Vec<SimpleExpr>| Func::cust("ST_Envelope_Agg").arg(args.remove(0)).into());
        });
        assert_eq!(rendered(backend.call("bounds", vec![geom()]).unwrap()), "ST_Envelope_Agg(\"geom\")");
    }

    #[test]
    #[should_panic(expected = "operation \"bounds\" is already registered")]
    fn registering_an_operation_twice_panics() {
        seeded().extend(|v| {
            v.register("bounds", |mut args: Vec<SimpleExpr>| args.remove(0));
        });
    }

    #[test]
    fn an_empty_dialect_starts_with_no_vocabulary() {
        assert!(Backend::new(Bare).operations().is_empty());
    }
}
