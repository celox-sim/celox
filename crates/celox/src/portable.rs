//! Portable IR types decoupled from the Veryl analyzer.
//!
//! These types replace analyzer-internal IDs (`VarId`, `StrId`) with
//! self-contained identifiers backed by a [`StringTable`], enabling
//! serialization and caching of SLT/SIR without analyzer dependencies.
//!
//! When `serde` is added as a dependency, add `Serialize`/`Deserialize`
//! derives and `#[serde(skip)]` on `StringTable::lookup`.

#![allow(dead_code)]

use std::fmt;

use crate::HashMap;

// ---------------------------------------------------------------------------
// StringTable — intern pool for variable/instance paths
// ---------------------------------------------------------------------------

/// A compact string intern table.
///
/// Strings are interned once and referred to by [`StrIdx`] elsewhere in the
/// portable IR.  The lookup map must be rebuilt after deserialization via
/// [`rebuild_lookup`](StringTable::rebuild_lookup).
#[derive(Clone, Debug)]
pub struct StringTable {
    strings: Vec<String>,
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
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

// ---------------------------------------------------------------------------
// PortableVariable — minimal variable metadata for IR
// ---------------------------------------------------------------------------

/// Stripped-down variable descriptor carrying only what the SLT/SIR pipeline
/// needs, with no references to analyzer-internal types.
#[derive(Clone, Debug)]
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
// Conversion from analyzer types
// ---------------------------------------------------------------------------

use veryl_analyzer::ir::{VarId, VarKind, Variable};

impl From<VarKind> for PortableVarKind {
    fn from(kind: VarKind) -> Self {
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

/// Context for converting analyzer types to portable types.
pub struct PortableConversion {
    pub string_table: StringTable,
    var_id_map: HashMap<VarId, StrIdx>,
}

impl PortableConversion {
    pub fn new() -> Self {
        Self {
            string_table: StringTable::new(),
            var_id_map: HashMap::default(),
        }
    }

    /// Intern a VarId by resolving its Variable path to a string.
    pub fn intern_var(&mut self, var: &Variable) -> StrIdx {
        if let Some(&idx) = self.var_id_map.get(&var.id) {
            return idx;
        }
        let path_str = var
            .path
            .0
            .iter()
            .map(|str_id| {
                veryl_parser::resource_table::get_str_value(*str_id)
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(".");
        let idx = self.string_table.intern(&path_str);
        self.var_id_map.insert(var.id, idx);
        idx
    }

    /// Convert a Variable to a PortableVariable.
    ///
    /// Returns `None` if width or array dimensions cannot be resolved
    /// (contains unsized parameters).
    pub fn convert_variable(&mut self, var: &Variable) -> Option<PortableVariable> {
        let path = self.intern_var(var);
        let width = var.total_width()?;
        let array_dims: Option<Vec<usize>> =
            var.r#type.array.as_slice().iter().copied().collect();
        let array_dims = array_dims.unwrap_or_default();
        let total_array = array_dims.iter().product::<usize>().max(1);
        Some(PortableVariable {
            path,
            kind: var.kind.into(),
            width,
            array_dims,
            total_width: width * total_array,
            signed: var.r#type.signed,
            is_4state: var.r#type.is_4state(),
        })
    }

    /// Look up the StrIdx for a VarId that was already interned.
    pub fn get(&self, var_id: &VarId) -> Option<StrIdx> {
        self.var_id_map.get(var_id).copied()
    }
}
