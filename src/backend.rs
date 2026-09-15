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

fn one(name: &'static str) -> impl SqlFn {
    move |mut args: Vec<SimpleExpr>| Func::cust(name).arg(args.remove(0)).into()
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

    fn vocabulary(&self) -> Vocabulary {
        let mut v = Vocabulary::default();
        v.register("extent_agg", one("ST_Extent_Agg"));
        v.register("x", one("ST_X"));
        v.register("y", one("ST_Y"));
        v.register("x_min", one("ST_XMin"));
        v.register("y_min", one("ST_YMin"));
        v.register("x_max", one("ST_XMax"));
        v.register("y_max", one("ST_YMax"));
        v.register("area_meters", |mut args: Vec<SimpleExpr>| {
            Func::cust("SUM").arg(Func::cust("ST_Area_Spheroid").arg(args.remove(0))).into()
        });
        v.register("geojson_coordinates", |mut args: Vec<SimpleExpr>| {
            Expr::cust_with_exprs(
                "json_extract(CAST($1 AS JSON), '$.coordinates')",
                [Func::cust("ST_AsGeoJSON").arg(args.remove(0)).into()],
            )
        });
        v.register("array", |args: Vec<SimpleExpr>| Func::cust("json_array").args(args).into());
        v.register("object", |args: Vec<SimpleExpr>| Func::cust("json_object").args(args).into());
        v.register("as_text", |mut args: Vec<SimpleExpr>| Func::cast_as(args.remove(0), "VARCHAR").into());
        v
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

    fn duckdb() -> Backend {
        Backend::default()
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

    #[rstest]
    #[case("extent_agg", "ST_Extent_Agg(\"geom\")")]
    #[case("x", "ST_X(\"geom\")")]
    #[case("y", "ST_Y(\"geom\")")]
    #[case("x_min", "ST_XMin(\"geom\")")]
    #[case("y_min", "ST_YMin(\"geom\")")]
    #[case("x_max", "ST_XMax(\"geom\")")]
    #[case("y_max", "ST_YMax(\"geom\")")]
    #[case("area_meters", "SUM(ST_Area_Spheroid(\"geom\"))")]
    #[case("as_text", "CAST(\"geom\" AS VARCHAR)")]
    fn the_duckdb_vocabulary_spells_each_operation(#[case] op: &str, #[case] expected: &str) {
        assert_eq!(rendered(duckdb().call(op, vec![geom()]).unwrap()), expected);
    }

    #[test]
    fn an_unknown_operation_names_itself() {
        let err = duckdb().call("banana", vec![geom()]).unwrap_err();
        assert_eq!(err, UnknownOp("banana".to_string()));
        assert_eq!(err.to_string(), "this backend has no operation named \"banana\"");
    }

    #[test]
    fn a_vocabulary_reports_what_it_holds() {
        let held = duckdb().operations();
        assert!(held.contains(&"extent_agg"), "{held:?}");
        assert!(!held.contains(&"banana"), "{held:?}");
        assert!(duckdb().has("array"));
        assert!(!duckdb().has("banana"));
    }

    #[test]
    fn a_downstream_can_add_an_operation_core_never_named() {
        let backend = Backend::default().extend(|v| {
            v.register("downstream_op", |mut args: Vec<SimpleExpr>| {
                Func::cust("ST_Whatever").arg(Func::cust("ST_Union_Agg").arg(args.remove(0))).into()
            });
        });
        assert_eq!(
            rendered(backend.call("downstream_op", vec![geom()]).unwrap()),
            "ST_Whatever(ST_Union_Agg(\"geom\"))"
        );
        assert!(backend.has("extent_agg"), "extending must not drop the seeded vocabulary");
    }

    #[test]
    fn a_downstream_can_replace_an_operation() {
        let backend = Backend::default().extend(|v| {
            v.replace("x", |mut args: Vec<SimpleExpr>| Func::cust("ST_XCoord").arg(args.remove(0)).into());
        });
        assert_eq!(rendered(backend.call("x", vec![geom()]).unwrap()), "ST_XCoord(\"geom\")");
    }

    #[test]
    #[should_panic(expected = "operation \"x\" is already registered")]
    fn registering_an_operation_twice_panics() {
        Backend::default().extend(|v| {
            v.register("x", |mut args: Vec<SimpleExpr>| args.remove(0));
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
