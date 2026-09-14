//! SLT boundary for the affine scheduling experiment.
//!
//! These statements retain array-assignment and loop provenance before a
//! frontend folds an entire array into scalar state. Existing ForFold trees
//! are deliberately not reinterpreted as independent array writes. Frontend
//! discovery of these regions is separate from the scheduler experiment.

use crate::{HashMap, HashSet, NodeId, SLTNode, SLTNodeArena, SLTToSIRLowerer};
use celox_analysis::polyhedral::{Affine, Domain, Error};
use celox_design::VarAtomBase;
use celox_sir::{
    SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    affine::{Kernel, MemoryObject, StatementBody},
};
use std::{
    fmt::{Debug, Display},
    hash::Hash,
};

#[derive(Clone, Debug)]
pub struct ArrayStore<A> {
    pub address: A,
    /// Flattened element index; its expression is checked for wraparound by
    /// the SIR adapter after ordinary SLT arithmetic lowering.
    pub index: NodeId,
    pub value: NodeId,
}

#[derive(Clone, Debug)]
pub struct ArrayStatement<A: Clone + Eq + Hash> {
    pub domain: Domain,
    pub original_schedule: Vec<Affine>,
    /// Separate 64-bit signed semantic inputs represent mathematical integer
    /// iterators. Source unsigned/narrow induction needs explicit provenance
    /// conversion before entering this experimental boundary.
    pub induction: Vec<VarAtomBase<A>>,
    pub stores: Vec<ArrayStore<A>>,
}

/// Lower statement expressions with the existing SLT semantics, then derive
/// affine accesses and dependence constraints through the shared SIR adapter.
/// The result can be scheduled before generic SLT-to-SIR region lowering.
pub fn extract<A: Clone + Eq + Hash + Debug + Display>(
    arena: &SLTNodeArena<A>,
    statements: &[ArrayStatement<A>],
    objects: &HashMap<A, MemoryObject>,
    four_state: bool,
) -> Result<Kernel<A>, Error> {
    let element_widths = objects
        .iter()
        .map(|(address, object)| (address.clone(), object.element_width))
        .collect();
    let lowerer =
        SLTToSIRLowerer::new(four_state).with_unpacked_input_types(arena, &element_widths);
    let mut bodies = Vec::new();
    for statement in statements {
        let mut seen = HashSet::default();
        let mut pending = statement
            .stores
            .iter()
            .flat_map(|store| [store.index, store.value])
            .collect::<Vec<_>>();
        while let Some(node) = pending.pop() {
            if !seen.insert(node) {
                continue;
            }
            match arena
                .get_checked(node)
                .ok_or(Error::Invalid("SLT statement node"))?
            {
                SLTNode::Input {
                    variable,
                    signed,
                    index,
                    access,
                } => {
                    if statement.induction.iter().any(|i| &i.id == variable)
                        && (!signed || !index.is_empty() || access.lsb != 0 || access.msb != 63)
                    {
                        return Err(Error::Invalid("SLT induction input representation"));
                    }
                    pending.extend(index.iter().map(|entry| entry.node))
                }
                SLTNode::Constant(..) => {}
                SLTNode::Binary(left, _, right) => pending.extend([*left, *right]),
                SLTNode::Unary(_, value) | SLTNode::Slice { expr: value, .. } => {
                    pending.push(*value)
                }
                SLTNode::Concat(parts) => pending.extend(parts.iter().map(|(node, _)| *node)),
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => pending.extend([*cond, *then_expr, *else_expr]),
                SLTNode::ForFold { .. }
                | SLTNode::ForFoldGroup { .. }
                | SLTNode::Capture { .. } => {
                    return Err(Error::Invalid(
                        "SLT statement contains a fold or an evaluation-position capture",
                    ));
                }
            }
        }
        let mut builder = SIRBuilder::new();
        let mut inputs = HashMap::default();
        let mut induction = Vec::new();
        for variable in &statement.induction {
            if variable.access.lsb != 0
                || variable.access.msb != 63
                || objects.contains_key(&variable.id)
            {
                return Err(Error::Invalid(
                    "SLT induction must be a separate 64-bit value",
                ));
            }
            let register = builder.alloc_bit(64, true);
            if inputs.insert(variable.clone(), register).is_some() {
                return Err(Error::Invalid("duplicate SLT induction"));
            }
            induction.push(register);
        }
        for store in &statement.stores {
            let object = objects
                .get(&store.address)
                .ok_or(Error::Invalid("SLT output memory shape"))?;
            if crate::get_width(store.value, arena) != object.element_width {
                return Err(Error::Invalid("SLT partial element store"));
            }
            let mut cache = HashMap::default();
            let index = lowerer.lower_with_inputs(
                &mut builder,
                store.index,
                arena,
                &mut cache,
                inputs.clone(),
            );
            let value = lowerer.lower_with_inputs(
                &mut builder,
                store.value,
                arena,
                &mut cache,
                inputs.clone(),
            );
            builder.emit(SIRInstruction::Store(
                store.address.clone(),
                SIROffset::Element {
                    index,
                    element_width: object.element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                object.element_width,
                value,
                vec![],
                vec![],
            ));
        }
        let entry = builder.seal_block(SIRTerminator::Return);
        let (mut blocks, register_types, _) = builder.drain();
        if blocks.len() != 1 {
            return Err(Error::Invalid("SLT statement lowered into control flow"));
        }
        let instructions = blocks
            .remove(&entry)
            .ok_or(Error::Invalid("SLT statement entry"))?
            .instructions;
        bodies.push(StatementBody {
            domain: statement.domain.clone(),
            original_schedule: statement.original_schedule.clone(),
            induction,
            register_types,
            instructions,
        });
    }
    Kernel::from_bodies(bodies, objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_design::BitAccess;

    #[test]
    fn requires_explicit_signed_induction_and_preserves_capture_positions() {
        let mut arena = SLTNodeArena::<u32>::new();
        let unsigned = arena
            .alloc(SLTNode::Input {
                variable: 100,
                signed: false,
                index: vec![],
                access: BitAccess::new(0, 63),
            })
            .unwrap();
        let signed = arena
            .alloc(SLTNode::Input {
                variable: 100,
                signed: true,
                index: vec![],
                access: BitAccess::new(0, 63),
            })
            .unwrap();
        let value = arena
            .alloc(SLTNode::Constant(7u64.into(), 0u64.into(), 64, false))
            .unwrap();
        let captured = arena
            .alloc(SLTNode::Capture {
                expr: value,
                key: 0,
            })
            .unwrap();
        let objects = [(
            0u32,
            MemoryObject {
                element_width: 64,
                elements: 4,
            },
        )]
        .into_iter()
        .collect();
        let mut statement = ArrayStatement {
            domain: Domain::rectangular(std::iter::once(0..4).collect::<Vec<_>>()),
            original_schedule: vec![Affine::axis(1, 0)],
            induction: vec![VarAtomBase::new(100, 0, 63)],
            stores: vec![ArrayStore {
                address: 0,
                index: unsigned,
                value,
            }],
        };
        assert!(matches!(
            extract(&arena, &[statement.clone()], &objects, false),
            Err(Error::Invalid("SLT induction input representation"))
        ));
        statement.stores[0].index = signed;
        assert!(extract(&arena, &[statement.clone()], &objects, false).is_ok());
        statement.stores[0].value = captured;
        assert!(matches!(
            extract(&arena, &[statement], &objects, false),
            Err(Error::Invalid(
                "SLT statement contains a fold or an evaluation-position capture"
            ))
        ));
    }
}
