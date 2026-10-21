//! IEEE 1800-2023 21.6 command-line arguments. Argument order is significant.
use num_bigint::BigUint;

#[derive(Clone, Debug, Default)]
pub struct Plusargs(pub Vec<String>);

impl Plusargs {
    /// Match a prefix, ignoring a single leading `+` on configured arguments.
    pub fn test(&self, query: &[u8]) -> bool {
        self.find(query).is_some()
    }

    fn find(&self, query: &[u8]) -> Option<&[u8]> {
        self.0.iter().find_map(|arg| {
            arg.strip_prefix('+')
                .unwrap_or(arg)
                .as_bytes()
                .strip_prefix(query)
        })
    }

    /// Return no value for an unmatched query, preserving the caller's target.
    /// A conversion containing an illegal character returns an all-X value.
    pub fn value(&self, query: &[u8], width: usize) -> Result<Option<(BigUint, BigUint)>, String> {
        let percent = query
            .iter()
            .position(|b| *b == b'%')
            .ok_or("$value$plusargs requires a conversion")?;
        let mut format = &query[percent + 1..];
        if format.first() == Some(&b'0') {
            format = &format[1..];
        }
        if format.len() != 1 {
            return Err("$value$plusargs requires exactly one trailing conversion".into());
        }
        let conversion = format[0].to_ascii_lowercase();
        let radix = match conversion {
            b'b' => 2,
            b'o' => 8,
            b'h' | b'x' => 16,
            b'd' => 10,
            b's' => 0,
            b'e' | b'f' | b'g' => {
                return Err("real conversion of $value$plusargs is unsupported".into());
            }
            _ => return Err("unsupported conversion of $value$plusargs".into()),
        };
        let Some(input) = self.find(&query[..percent]) else {
            return Ok(None);
        };
        let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        if radix == 0 {
            return Ok(Some((
                BigUint::from_bytes_be(input) & &mask,
                BigUint::default(),
            )));
        }
        let mut digits = input;
        while digits.first().is_some_and(u8::is_ascii_whitespace) {
            digits = &digits[1..];
        }
        let negative = digits.first() == Some(&b'-');
        let signed = negative || digits.first() == Some(&b'+');
        if signed {
            digits = &digits[1..];
        }
        // Separators may follow a digit, but cannot replace the first digit
        // (IEEE 1800-2023 5.7.1). Validate before discarding separators.
        if digits.first() == Some(&b'_') {
            return Ok(Some((BigUint::default(), mask)));
        }
        let compact: Vec<_> = digits.iter().copied().filter(|b| *b != b'_').collect();
        let digits = compact.as_slice();
        // A single decimal unknown digit denotes an entire X/Z value;
        // a sign followed by an unknown digit is not a decimal input field
        // (21.3.4.3, Table 21-7).
        if radix == 10 && !signed && digits.len() == 1 {
            match digits[0] {
                b'x' | b'X' => return Ok(Some((BigUint::default(), mask))),
                b'z' | b'Z' | b'?' => return Ok(Some((mask.clone(), mask))),
                _ => {}
            }
        }
        if radix != 10
            && digits
                .iter()
                .any(|b| matches!(b, b'x' | b'X' | b'z' | b'Z' | b'?'))
        {
            let bits = match radix {
                2 => 1,
                8 => 3,
                _ => 4,
            };
            let mut value = BigUint::default();
            let mut unknown = BigUint::default();
            for &digit in digits {
                value <<= bits;
                unknown <<= bits;
                match digit {
                    b'x' | b'X' => unknown |= BigUint::from((1u8 << bits) - 1),
                    b'z' | b'Z' | b'?' => {
                        let group = BigUint::from((1u8 << bits) - 1);
                        value |= &group;
                        unknown |= group;
                    }
                    _ => match (digit as char).to_digit(radix) {
                        Some(digit) => value |= BigUint::from(digit),
                        None => return Ok(Some((BigUint::default(), mask))),
                    },
                }
            }
            // IEEE 1800-2023 11.4.3: negation of a value with any X/Z
            // bit produces an entirely unknown result. Apply it before
            // narrowing, including unknown digits above the target width.
            if negative {
                return Ok(Some((BigUint::default(), mask)));
            }
            return Ok(Some((value & &mask, unknown & mask)));
        }
        let value = if digits.is_empty() {
            Some(BigUint::default())
        } else {
            BigUint::parse_bytes(digits, radix)
        };
        match value {
            Some(value) => {
                let value = value & &mask;
                let value = if negative && value != BigUint::default() {
                    (&mask + BigUint::from(1u8)) - value
                } else {
                    value
                };
                Ok(Some((value, BigUint::default())))
            }
            None => Ok(Some((BigUint::default(), mask))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negative_nondecimal_unknowns_propagate_through_negation() {
        for (format, input, width) in [
            ("%h", "-1x", 32),
            ("%x", "-1Z", 65),
            ("%o", "-7?", 17),
            ("%b", "-10x", 8),
            ("%h", "-x10", 4),
        ] {
            let args = Plusargs(vec![format!("+number={input}")]);
            let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            assert_eq!(
                args.value(format!("number={format}").as_bytes(), width)
                    .unwrap(),
                Some((BigUint::default(), mask)),
                "{input} as {format}"
            );
        }
        // The sign must not change the existing positive four-state parser,
        // and known negative magnitudes still use ordinary two's complement.
        let args = Plusargs(vec!["+x=1x".into(), "+z=1z".into(), "+negative=-1f".into()]);
        assert_eq!(
            args.value(b"x=%h", 8).unwrap(),
            Some((16u32.into(), 15u32.into()))
        );
        assert_eq!(
            args.value(b"z=%h", 8).unwrap(),
            Some((31u32.into(), 15u32.into()))
        );
        assert_eq!(
            args.value(b"negative=%h", 8).unwrap(),
            Some((225u32.into(), 0u32.into()))
        );
    }

    #[test]
    fn decimal_unknown_digits_retain_their_four_state_value() {
        for (input, z) in [
            ("x", false),
            ("X", false),
            ("z", true),
            ("Z", true),
            ("?", true),
            ("z_", true),
        ] {
            let args = Plusargs(vec![format!("+number={input}")]);
            let mask = (BigUint::from(1u8) << 65usize) - BigUint::from(1u8);
            assert_eq!(
                args.value(b"number=%D", 65).unwrap(),
                Some((if z { mask.clone() } else { BigUint::default() }, mask)),
                "{input}"
            );
        }
        for input in [
            "+z", "-z", "+Z", "-Z", "+x", "-x", "+X", "-X", "+?", "-?", "zz", "1z", "z1",
        ] {
            let args = Plusargs(vec![format!("+number={input}")]);
            assert_eq!(
                args.value(b"number=%d", 8).unwrap(),
                Some((0u32.into(), 255u32.into())),
                "{input}"
            );
        }
    }

    #[test]
    fn numeric_separators_follow_the_first_digit() {
        for format in ["%d", "%h", "%o", "%b"] {
            for input in ["_12", "-_12", "+_12", "___", "  _1", "-_x"] {
                let args = Plusargs(vec![format!("+number={input}")]);
                assert_eq!(
                    args.value(format!("number={format}").as_bytes(), 8)
                        .unwrap(),
                    Some((0u32.into(), 255u32.into())),
                    "{input} as {format}"
                );
            }
            let args = Plusargs(vec!["+number=1__0_".into()]);
            let expected = match format {
                "%d" => 10,
                "%h" => 16,
                "%o" => 8,
                _ => 2,
            };
            assert_eq!(
                args.value(format!("number={format}").as_bytes(), 8)
                    .unwrap(),
                Some((BigUint::from(expected as u32), BigUint::default()))
            );
        }
        let args = Plusargs(vec!["+number=-1_2_".into(), "+text=_12".into()]);
        assert_eq!(
            args.value(b"number=%d", 8).unwrap(),
            Some((244u32.into(), 0u32.into()))
        );
        assert_eq!(
            args.value(b"text=%s", 24).unwrap(),
            Some((0x5f3132u32.into(), 0u32.into()))
        );
    }

    #[test]
    fn ordered_prefixes_and_conversions() {
        let args = Plusargs(vec![
            "+HELLO".into(),
            "+n=-12".into(),
            "+n=9".into(),
            "+hex=ab".into(),
            "+bad=12!".into(),
            "+s=abc".into(),
            "+empty=".into(),
        ]);
        assert!(args.test(b"HE"));
        assert!(!args.test(b"LO"));
        assert!(!args.test(b"+HE"));
        assert_eq!(
            args.value(b"n=%d", 8).unwrap(),
            Some((244u32.into(), 0u32.into()))
        );
        assert_eq!(
            args.value(b"hex=%0H", 4).unwrap(),
            Some((11u32.into(), 0u32.into()))
        );
        assert_eq!(
            args.value(b"bad=%d", 8).unwrap(),
            Some((0u32.into(), 255u32.into()))
        );
        assert_eq!(
            args.value(b"s=%s", 16).unwrap(),
            Some((0x6263u32.into(), 0u32.into()))
        );
        assert_eq!(
            args.value(b"empty=%d", 32).unwrap(),
            Some((0u32.into(), 0u32.into()))
        );
        assert_eq!(args.value(b"missing=%d", 32).unwrap(), None);
        assert!(args.value(b"real=%f", 32).is_err());
    }
}
