#[derive(Debug, PartialEq)]
pub enum Measure {
    Units(f64),
    Metres(f64),
}

impl Measure {
    pub fn is_negative(&self) -> bool {
        match self {
            Measure::Units(value) | Measure::Metres(value) => *value < 0.0,
        }
    }
}

struct Unit {
    names: &'static [&'static str],
    metres: f64,
}

const UNITS: &[Unit] = &[
    Unit { names: &["m", "meter", "meters", "metre", "metres"], metres: 1.0 },
    Unit { names: &["km", "kilometer", "kilometers", "kilometre", "kilometres"], metres: 1000.0 },
    Unit { names: &["cm", "centimeter", "centimeters", "centimetre", "centimetres"], metres: 0.01 },
    Unit { names: &["mm", "millimeter", "millimeters", "millimetre", "millimetres"], metres: 0.001 },
    Unit { names: &["ft", "foot", "feet"], metres: 0.3048 },
    Unit { names: &["in", "inch", "inches"], metres: 0.0254 },
    Unit { names: &["yd", "yard", "yards"], metres: 0.9144 },
    Unit { names: &["mi", "mile", "miles"], metres: 1609.344 },
    Unit { names: &["nm", "nmi", "nauticalmile", "nauticalmiles"], metres: 1852.0 },
    Unit { names: &["deg", "degree", "degrees"], metres: 111_320.0 },
    Unit { names: &["rad", "radian", "radians"], metres: 6_378_137.0 },
    Unit { names: &["grad", "gradian", "gradians", "gon"], metres: 100_108.0 },
];

pub fn parse(input: &str) -> Result<Measure, String> {
    let at = input.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(input.len());
    let (number, unit) = input.split_at(at);
    let value = number.parse::<f64>().map_err(|_| format!("needs a number, got {input:?}"))?;
    if !value.is_finite() {
        return Err(format!("needs a finite number, got {input:?}"));
    }
    if unit.is_empty() {
        return Ok(Measure::Units(value));
    }
    let lower = unit.to_ascii_lowercase();
    match UNITS.iter().find(|held| held.names.contains(&lower.as_str())) {
        Some(held) => Ok(Measure::Metres(value * held.metres)),
        None => Err(format!("does not know the unit {unit:?}; use m, km, ft, mi, deg and the like")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("100", Measure::Units(100.0))]
    #[case("-10", Measure::Units(-10.0))]
    #[case("-1km", Measure::Metres(-1000.0))]
    #[case("0.5", Measure::Units(0.5))]
    #[case(".5", Measure::Units(0.5))]
    #[case("100m", Measure::Metres(100.0))]
    #[case("5km", Measure::Metres(5000.0))]
    #[case("5KM", Measure::Metres(5000.0))]
    #[case("1.5kilometres", Measure::Metres(1500.0))]
    #[case("1000ft", Measure::Metres(304.8))]
    #[case("2.5mi", Measure::Metres(4023.36))]
    #[case("10nmi", Measure::Metres(18520.0))]
    #[case("1deg", Measure::Metres(111_320.0))]
    #[case("2rad", Measure::Metres(12_756_274.0))]
    fn a_distance_carries_its_unit(#[case] input: &str, #[case] expected: Measure) {
        assert_eq!(parse(input).unwrap(), expected);
    }

    #[rstest]
    #[case("", "needs a number")]
    #[case("banana", "needs a number")]
    #[case("100xyz", "does not know the unit")]
    #[case("m", "needs a number")]
    #[case("1e400", "does not know the unit")]
    #[case("1.2.3", "needs a number")]
    fn an_unusable_distance_says_why(#[case] input: &str, #[case] expected: &str) {
        let held = parse(input).unwrap_err();
        assert!(held.contains(expected), "expected {expected:?} in {held:?}");
    }

    #[test]
    fn a_number_too_large_to_hold_is_refused() {
        let held = parse(&"9".repeat(400)).unwrap_err();
        assert!(held.contains("needs a finite number"), "{held}");
    }

    #[test]
    fn an_exponent_is_not_a_unit_duckxy_pretends_to_know() {
        assert!(parse("1e5").is_err(), "1e5 was read as a distance");
    }
}
