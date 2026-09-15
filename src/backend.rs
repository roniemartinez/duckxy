use sea_query::{Expr, Func, SimpleExpr};
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
    fn core_only_depends_on_two_dialect_methods() {
        let d = DuckDb;
        assert_eq!(rendered(d.as_geojson(geom())), "ST_AsGeoJSON(\"geom\")");
        assert_eq!(
            rendered(d.transform(geom(), "EPSG:25832", "EPSG:3857")),
            "ST_Transform(\"geom\", 'EPSG:25832', 'EPSG:3857', always_xy := true)"
        );
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

    fn seeded() -> Backend {
        Backend::default().extend(|v| {
            v.register("bounds", |mut args: Vec<SimpleExpr>| Func::cust("ST_Extent_Agg").arg(args.remove(0)).into());
        })
    }

    #[test]
    fn a_dialect_seeds_no_vocabulary_of_its_own() {
        assert!(Backend::default().operations().is_empty(), "core named an operation it never calls");
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
        struct Bare;
        impl Dialect for Bare {
            fn transform(&self, g: SimpleExpr, _: &str, _: &str) -> SimpleExpr {
                g
            }
            fn as_geojson(&self, g: SimpleExpr) -> SimpleExpr {
                g
            }
        }
        assert!(Backend::new(Bare).operations().is_empty());
    }
}
