//! Source-independent symbolic logic tree primitives.
//!
//! This crate owns semantic bit-range state independently of any HDL
//! frontend. Frontends provide their own address identity and SLT node type.

use std::collections::BTreeSet;

use celox_design::VarAtomBase;
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

pub mod range_store;

pub use range_store::{RangeStore, RangeStoreError};

/// Symbolic state keyed by a frontend-independent semantic address.
///
/// `N` is the symbolic expression identity. It remains generic until the SLT
/// arena itself moves into this crate.
pub type SymbolicStore<A, N> = HashMap<A, RangeStore<Option<(N, HashSet<VarAtomBase<A>>)>>>;

/// Bit boundaries discovered while constructing symbolic state.
pub type BoundaryMap<A> = HashMap<A, BTreeSet<usize>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_store_key_is_a_semantic_address_not_a_frontend_id() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        struct DesignAddress(u32);

        let mut store = SymbolicStore::<DesignAddress, u32>::default();
        store.insert(DesignAddress(7), RangeStore::new(None, 8));

        assert!(store.contains_key(&DesignAddress(7)));
    }
}
