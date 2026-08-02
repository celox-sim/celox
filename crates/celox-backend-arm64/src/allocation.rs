//! AArch64-owned register assignments and out-of-SSA copy plans.

use std::collections::HashMap;
use std::hash::Hash;

use crate::Arm64Reg;

/// Physical register assignment produced for one target MIR function.
#[derive(Debug, Clone)]
pub(crate) struct Assignment<V> {
    registers: HashMap<V, Arm64Reg>,
}

impl<V> Default for Assignment<V> {
    fn default() -> Self {
        Self {
            registers: HashMap::new(),
        }
    }
}

impl<V> Assignment<V>
where
    V: Eq + Hash,
{
    pub(crate) fn get(&self, value: &V) -> Option<Arm64Reg> {
        self.registers.get(value).copied()
    }

    pub(crate) fn set(&mut self, value: V, register: Arm64Reg) {
        self.registers.insert(value, register);
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&V, &Arm64Reg)> {
        self.registers.iter()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyDestination {
    Register(Arm64Reg),
    Stack(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopySource {
    Register(Arm64Reg),
    Stack(i32),
    Immediate(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyOperation {
    Move {
        destination: CopyDestination,
        source: CopySource,
    },
    SwapRegisters {
        left: Arm64Reg,
        right: Arm64Reg,
    },
    SaveTemporary(CopyDestination),
    RestoreTemporary(CopyDestination),
}

/// Dependency-ordered physical copies indexed by normalized CFG edge.
#[derive(Debug, Clone)]
pub(crate) struct EdgeCopyPlan<B> {
    edges: HashMap<(B, B), Vec<CopyOperation>>,
}

impl<B> Default for EdgeCopyPlan<B> {
    fn default() -> Self {
        Self {
            edges: HashMap::new(),
        }
    }
}

impl<B> EdgeCopyPlan<B>
where
    B: Copy + Eq + Hash,
{
    pub(crate) fn insert(&mut self, predecessor: B, successor: B, copies: Vec<CopyOperation>) {
        if !copies.is_empty() {
            self.edges.insert((predecessor, successor), copies);
        }
    }

    pub(crate) fn edge(&self, predecessor: B, successor: B) -> Option<&[CopyOperation]> {
        self.edges.get(&(predecessor, successor)).map(Vec::as_slice)
    }
}
