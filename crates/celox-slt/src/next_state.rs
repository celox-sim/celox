//! The next-state form of edge-triggered processes: for each register bit
//! range a process writes nonblockingly, its value at the edge as an
//! expression of the state before it, and for each nonblocking write of a
//! memory element at a run-time index, a write port.
//!
//! Frontends lower a process into this form ([`NextStateForm`]); assembly
//! relocates the forms of a clock domain into one DAG and generates its
//! code. The roots of a form are computed before any of them is written
//! (IEEE 1800-2023 10.4.2); the ports apply after the roots, in order.

mod lower;
pub use lower::{
    Publication, SampledStore, lower_transition, sample_enabled_transition, sample_transition,
};

use std::hash::Hash;

use celox_design::BitAccess;

use crate::{HashMap, NodeId, SLTNode, SLTNodeArena, SLTNodeFactsError};

/// A value to publish for a register or observable wire at the sampled edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueRoot<A> {
    pub target: A,
    pub access: BitAccess,
    pub value: NodeId,
    /// A conditional NBA with a private working slot shared by its bank.
    /// The total value above remains available to direct transitions.
    pub enable: Option<WriteEnable<A>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteEnable<A> {
    pub slot: A,
    pub condition: NodeId,
    pub data: NodeId,
}

/// A nonblocking write of an unpacked-array element at a run-time index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RamWritePort<A> {
    pub array: A,
    /// The element index, 64 bits wide.
    pub index: NodeId,
    pub element_width: usize,
    /// The bits of the element written.
    pub access: BitAccess,
    pub value: NodeId,
    /// The condition the write happens under: the path to the statement
    /// and an index inside the array.
    pub guard: NodeId,
}

/// The next-state form of one process, or of the processes of a domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NextStateForm<A> {
    pub roots: Vec<ValueRoot<A>>,
    pub ports: Vec<RamWritePort<A>>,
}

impl<A> Default for NextStateForm<A> {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            ports: Vec::new(),
        }
    }
}

impl<A> NextStateForm<A> {
    /// The nodes the roots and ports compute.
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.roots
            .iter()
            .map(|root| root.value)
            .chain(
                self.ports
                    .iter()
                    .flat_map(|port| [port.index, port.value, port.guard]),
            )
            .chain(
                self.roots
                    .iter()
                    .filter_map(|root| root.enable.as_ref())
                    .flat_map(|enable| [enable.condition, enable.data]),
            )
    }

    /// The form with each node mapped by `node` and each variable by `var`.
    pub fn map<B, E>(
        &self,
        mut var: impl FnMut(&A) -> B,
        mut node: impl FnMut(NodeId) -> Result<NodeId, E>,
    ) -> Result<NextStateForm<B>, E> {
        let mut mapped = NextStateForm::default();
        for root in &self.roots {
            mapped.roots.push(ValueRoot {
                enable: match &root.enable {
                    Some(enable) => Some(WriteEnable {
                        slot: var(&enable.slot),
                        condition: node(enable.condition)?,
                        data: node(enable.data)?,
                    }),
                    None => None,
                },
                target: var(&root.target),
                access: root.access,
                value: node(root.value)?,
            });
        }
        for port in &self.ports {
            mapped.ports.push(RamWritePort {
                array: var(&port.array),
                index: node(port.index)?,
                element_width: port.element_width,
                access: port.access,
                value: node(port.value)?,
                guard: node(port.guard)?,
            });
        }
        Ok(mapped)
    }

    /// The form moved from `arena` into `target`, its variables relocated
    /// by `locate`.
    pub fn relocate<B: Hash + Eq + Clone>(
        &self,
        arena: &SLTNodeArena<A>,
        target: &mut SLTNodeArena<B>,
        locate: &impl Fn(&A) -> B,
    ) -> Result<NextStateForm<B>, SLTNodeFactsError>
    where
        A: std::fmt::Debug + std::fmt::Display + Hash + Eq + Clone,
    {
        let mut cache = HashMap::default();
        self.map(
            locate,
            |node: NodeId| -> Result<NodeId, SLTNodeFactsError> {
                let source: &SLTNode<A> = arena.get(node);
                source.map_addr(node, arena, target, &mut cache, locate)
            },
        )
    }
}
