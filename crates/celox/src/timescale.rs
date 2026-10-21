//! Simulation default time unit and scheduler precision.

/// Default SV time unit/precision. Explicit source declarations take precedence;
/// the precision also contributes to the global scheduler resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvTimescale {
    pub(crate) unit: i32,
    pub(crate) precision: i32,
}

impl std::str::FromStr for SvTimescale {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        fn exponent(value: &str) -> Option<i32> {
            let value = value.trim();
            let split = value.find(|c: char| !c.is_ascii_digit())?;
            let magnitude = match &value[..split] {
                "1" => 0,
                "10" => 1,
                "100" => 2,
                _ => return None,
            };
            let unit = match &value[split..] {
                "s" => 0,
                "ms" => -3,
                "us" => -6,
                "ns" => -9,
                "ps" => -12,
                "fs" => -15,
                _ => return None,
            };
            Some(unit + magnitude)
        }
        let invalid = || {
            format!(
                "invalid timescale `{value}`: expected UNIT/PRECISION (1, 10, or 100 s/ms/us/ns/ps/fs), with precision no coarser than unit"
            )
        };
        let (unit, precision) = value.split_once('/').ok_or_else(invalid)?;
        let unit = exponent(unit).ok_or_else(invalid)?;
        let precision = exponent(precision).ok_or_else(invalid)?;
        if precision > unit {
            return Err(invalid());
        }
        Ok(Self { unit, precision })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_time_magnitudes_units_and_precision_order() {
        for (text, unit, precision) in [
            ("1ns/1fs", -9, -15),
            ("100s / 10fs", 2, -14),
            ("10ps/1ps", -11, -12),
        ] {
            assert_eq!(
                text.parse::<SvTimescale>().unwrap(),
                SvTimescale { unit, precision }
            );
        }
        for text in [
            "1ns",
            "1ns/1ps/1fs",
            "2ns/1fs",
            "1ps/1ns",
            "1ns/0fs",
            "1ns/1as",
            "1.0ns/1fs",
        ] {
            assert!(text.parse::<SvTimescale>().is_err(), "{text}");
        }
    }
}
