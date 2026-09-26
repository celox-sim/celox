use crate::BigUint;

mod sealed {
    pub trait Sealed {}
}

/// Integer types accepted by the test driver. Signed integers are transferred
/// as their two's-complement bit patterns; reads truncate to the scalar width.
pub trait Scalar: sealed::Sealed + Copy {
    fn to_bits(self) -> BigUint;
    fn from_bits(value: &BigUint) -> Self;
}

macro_rules! scalars {
    ($($ty:ty),* $(,)?) => {$(
        impl sealed::Sealed for $ty {}
        impl Scalar for $ty {
            fn to_bits(self) -> BigUint {
                BigUint::from_bytes_le(&self.to_le_bytes())
            }
            fn from_bits(value: &BigUint) -> Self {
                let bytes = value.to_bytes_le();
                let mut scalar = [0; size_of::<Self>()];
                let len = scalar.len().min(bytes.len());
                scalar[..len].copy_from_slice(&bytes[..len]);
                Self::from_le_bytes(scalar)
            }
        }
    )*};
}
scalars!(
    u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize
);

impl sealed::Sealed for bool {}
impl Scalar for bool {
    fn to_bits(self) -> BigUint {
        BigUint::from(u8::from(self))
    }
    fn from_bits(value: &BigUint) -> Self {
        value.bit(0)
    }
}
