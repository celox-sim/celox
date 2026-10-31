use num_bigint::BigUint;

/// Testbench value: integer bits or a dynamically sized byte string.
#[derive(Clone, Debug)]
pub enum TestbenchValue {
    U64(u64),
    Wide(BigUint),
    String(Vec<u8>),
}

impl TestbenchValue {
    #[inline]
    pub fn to_u64(&self) -> u64 {
        match self {
            TestbenchValue::String(v) => v
                .iter()
                .rev()
                .take(8)
                .enumerate()
                .fold(0, |value, (i, byte)| value | (u64::from(*byte) << (i * 8))),
            TestbenchValue::U64(v) => *v,
            TestbenchValue::Wide(v) => {
                let digits = v.to_u64_digits();
                digits.first().copied().unwrap_or(0)
            }
        }
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        match self {
            TestbenchValue::String(v) => v.is_empty(),
            TestbenchValue::U64(v) => *v == 0,
            TestbenchValue::Wide(v) => *v == BigUint::ZERO,
        }
    }

    #[inline]
    pub fn to_biguint(&self) -> BigUint {
        match self {
            TestbenchValue::String(v) => BigUint::from_bytes_be(v),
            TestbenchValue::U64(v) => BigUint::from(*v),
            TestbenchValue::Wide(v) => v.clone(),
        }
    }
}

#[cfg(test)]
mod arithmetic_oracle;
#[cfg(test)]
pub(crate) use arithmetic_oracle::eval_typed_binop;
