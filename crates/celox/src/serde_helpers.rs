//! Serde helpers for types without native serde support.
#![allow(dead_code)]

/// Serialize/deserialize `malachite_bigint::BigUint` as little-endian bytes.
pub mod biguint {
    use malachite_bigint::BigUint;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(val: &BigUint, s: S) -> Result<S::Ok, S::Error> {
        val.to_bytes_le().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BigUint, D::Error> {
        let bytes: Vec<u8> = Vec::deserialize(d)?;
        Ok(BigUint::from_bytes_le(&bytes))
    }
}
