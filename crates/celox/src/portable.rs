//! Portable IR types decoupled from the Veryl analyzer.
//!
//! These types replace analyzer-internal IDs (`VarId`, `StrId`) with
//! self-contained identifiers backed by a [`StringTable`], enabling
//! serialization and caching of SLT/SIR without analyzer dependencies.
//!
//! All portable types derive `Serialize`/`Deserialize` for caching.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::HashMap;
use crate::ir::{AbsoluteAddrBase, RegionedAbsoluteAddrBase, RegionedVarAddrBase};

// ---------------------------------------------------------------------------
// StringTable — intern pool for variable/instance paths
// ---------------------------------------------------------------------------

/// A compact string intern table.
///
/// Strings are interned once and referred to by [`StrIdx`] elsewhere in the
/// portable IR.  The lookup map must be rebuilt after deserialization via
/// [`rebuild_lookup`](StringTable::rebuild_lookup).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StringTable {
    strings: Vec<String>,
    #[serde(skip, default = "HashMap::default")]
    lookup: HashMap<String, u32>,
}

impl StringTable {
    pub fn new() -> Self {
        Self {
            strings: Vec::new(),
            lookup: HashMap::default(),
        }
    }

    /// Intern a string, returning its index.  Deduplicates.
    pub fn intern(&mut self, s: &str) -> StrIdx {
        if let Some(&idx) = self.lookup.get(s) {
            return StrIdx(idx);
        }
        let idx = self.strings.len() as u32;
        self.strings.push(s.to_owned());
        self.lookup.insert(s.to_owned(), idx);
        StrIdx(idx)
    }

    /// Resolve an index back to a string slice.
    pub fn resolve(&self, idx: StrIdx) -> &str {
        &self.strings[idx.0 as usize]
    }

    /// Rebuild the lookup map (call after deserialization).
    pub fn rebuild_lookup(&mut self) {
        self.lookup.clear();
        for (i, s) in self.strings.iter().enumerate() {
            self.lookup.insert(s.clone(), i as u32);
        }
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

impl Default for StringTable {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// StrIdx — index into StringTable
// ---------------------------------------------------------------------------

/// A lightweight handle into a [`StringTable`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StrIdx(pub u32);

impl fmt::Debug for StrIdx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StrIdx({})", self.0)
    }
}

impl fmt::Display for StrIdx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// PortableVarKind — mirrors VarKind without analyzer dependency
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PortableVarKind {
    Param,
    Const,
    Input,
    Output,
    Inout,
    Variable,
    Let,
}

impl PortableVarKind {
    pub fn is_port(&self) -> bool {
        matches!(
            self,
            PortableVarKind::Input | PortableVarKind::Output | PortableVarKind::Inout
        )
    }
}

impl fmt::Display for PortableVarKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PortableVarKind::Param => "param",
            PortableVarKind::Const => "const",
            PortableVarKind::Input => "input",
            PortableVarKind::Output => "output",
            PortableVarKind::Inout => "inout",
            PortableVarKind::Variable => "var",
            PortableVarKind::Let => "let",
        };
        f.write_str(s)
    }
}

impl From<veryl_analyzer::ir::VarKind> for PortableVarKind {
    fn from(kind: veryl_analyzer::ir::VarKind) -> Self {
        use veryl_analyzer::ir::VarKind;
        match kind {
            VarKind::Param => PortableVarKind::Param,
            VarKind::Const => PortableVarKind::Const,
            VarKind::Input => PortableVarKind::Input,
            VarKind::Output => PortableVarKind::Output,
            VarKind::Inout => PortableVarKind::Inout,
            VarKind::Variable => PortableVarKind::Variable,
            VarKind::Let => PortableVarKind::Let,
        }
    }
}

// ---------------------------------------------------------------------------
// PortableVariable — minimal variable metadata for IR
// ---------------------------------------------------------------------------

/// Stripped-down variable descriptor carrying only what the SLT/SIR pipeline
/// needs, with no references to analyzer-internal types.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortableVariable {
    /// Fully qualified path, interned in the StringTable.
    pub path: StrIdx,
    /// Variable kind (input/output/var/…).
    pub kind: PortableVarKind,
    /// Scalar bit width (excluding array dimensions).
    pub width: usize,
    /// Array dimensions, outermost first.  Empty for scalars.
    pub array_dims: Vec<usize>,
    /// Total bit width = width × product(array_dims).
    pub total_width: usize,
    /// Whether the variable is signed.
    pub signed: bool,
    /// Whether the variable uses 4-state logic (has X/Z).
    pub is_4state: bool,
}

impl PortableVariable {
    pub fn total_array(&self) -> usize {
        self.array_dims.iter().product::<usize>().max(1)
    }
}

// ---------------------------------------------------------------------------
// Portable address aliases — reuse generic base types with StrIdx
// ---------------------------------------------------------------------------

/// Regioned variable address using portable `StrIdx`.
pub type PortableRegionedAddr = RegionedVarAddrBase<StrIdx>;

/// Absolute address (instance + variable) using portable `StrIdx`.
pub type PortableAbsoluteAddr = AbsoluteAddrBase<StrIdx>;

/// Regioned absolute address using portable `StrIdx`.
pub type PortableRegionedAbsoluteAddr = RegionedAbsoluteAddrBase<StrIdx>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_table_deduplication() {
        let mut table = StringTable::new();
        let idx1 = table.intern("clk");
        let idx2 = table.intern("clk");
        let idx3 = table.intern("rst");

        assert_eq!(idx1, idx2, "same string should return same index");
        assert_ne!(
            idx1, idx3,
            "different strings should return different indices"
        );
        assert_eq!(table.len(), 2);
        assert_eq!(table.resolve(idx1), "clk");
        assert_eq!(table.resolve(idx3), "rst");
    }

    #[test]
    fn string_table_rebuild_lookup() {
        let mut table = StringTable::new();
        let idx = table.intern("test");

        // Clear and rebuild
        table.lookup.clear();
        assert!(table.lookup.is_empty());
        table.rebuild_lookup();
        assert!(!table.lookup.is_empty());

        // Re-intern should return same index
        let idx2 = table.intern("test");
        assert_eq!(idx, idx2);
    }

    #[test]
    fn serde_impls_exist() {
        fn assert_serde<T: serde::Serialize + for<'de> serde::Deserialize<'de>>() {}
        assert_serde::<StringTable>();
        assert_serde::<StrIdx>();
        assert_serde::<PortableVariable>();
        assert_serde::<PortableVarKind>();
    }
}
