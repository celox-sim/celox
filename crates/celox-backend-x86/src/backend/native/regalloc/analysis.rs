//! Target-independent liveness and next-use analysis adapter.
//!
//! The x86 backend owns its MIR and exports opcode-free facts. The fixed-point
//! algorithm lives in `celox-backend-common`, where AArch64 can use it without
//! importing x86 instruction types.

use celox_backend_common::regalloc::{NextUseAnalysis, analyze_next_uses};

use crate::HashSet;
use crate::native::mir::{MFunction, VReg};

use super::assignment::{AssignmentMap, EdgeLocation};

pub(super) type AnalysisResult = NextUseAnalysis<VReg>;

/// Compute ordinary target-MIR liveness and next-use distances.
pub(super) fn analyze(function: &MFunction) -> AnalysisResult {
    let facts = super::facts::build(function, |_| Default::default())
        .expect("verified x86 MIR must export valid allocation facts");
    analyze_next_uses(&facts).expect("verified allocation facts must support next-use analysis")
}

/// Rebuild verifier liveness from destination-qualified phi locations.
/// Semantic source VRegs remain in MIR for out-of-SSA identity, but an exact
/// stack/immediate row is not a register use on that edge.
pub(super) fn analyze_for_assignment(
    function: &MFunction,
    assignment: &AssignmentMap,
) -> AnalysisResult {
    let mut ignored_phi_sources = assignment
        .phi_edge_locations
        .iter()
        .filter_map(|(&edge, &location)| {
            (!matches!(location, EdgeLocation::Register(_))).then_some(edge)
        })
        .collect::<HashSet<_>>();
    for block in &function.blocks {
        for phi in &block.phis {
            if assignment.is_semantic_phi_definition(phi.dst) {
                ignored_phi_sources.extend(
                    phi.sources
                        .iter()
                        .map(|&(predecessor, source)| (predecessor, block.id, phi.dst, source)),
                );
            }
        }
    }
    let stack_values = assignment
        .edge_spill_slots
        .keys()
        .copied()
        .filter(|value| assignment.get(*value).is_none())
        .collect::<HashSet<_>>();

    let mut facts = super::facts::build(function, |_| Default::default())
        .expect("verified x86 MIR must export valid allocation facts");
    for (successor_index, block) in facts.blocks.iter_mut().enumerate() {
        let successor = function.blocks[successor_index].id;
        for phi in &mut block.phis {
            phi.sources.retain(|source| {
                let predecessor = function.blocks[source.predecessor].id;
                !ignored_phi_sources.contains(&(
                    predecessor,
                    successor,
                    phi.destination,
                    source.value,
                )) && !stack_values.contains(&source.value)
            });
        }
    }
    analyze_next_uses(&facts).expect("filtered allocation facts must remain structurally valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::mir::{
        BaseReg, BlockId, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

    fn empty_func(blocks: Vec<MBlock>) -> MFunction {
        let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
        function.blocks = blocks;
        function
    }

    #[test]
    fn lower_index_successor_is_not_backedge_in_dag() {
        let mut b0 = MBlock::new(BlockId(0));
        b0.push(MInst::Jump { target: BlockId(1) });

        let mut b2 = MBlock::new(BlockId(2));
        b2.push(MInst::Return);

        let mut b1 = MBlock::new(BlockId(1));
        b1.push(MInst::Jump { target: BlockId(2) });

        let analysis = analyze(&empty_func(vec![b0, b2, b1]));
        assert!(analysis.backedge_successors[2].is_empty());
    }

    #[test]
    fn dfs_gray_edge_is_backedge() {
        let mut b0 = MBlock::new(BlockId(0));
        b0.push(MInst::Jump { target: BlockId(1) });

        let mut b1 = MBlock::new(BlockId(1));
        b1.push(MInst::Jump { target: BlockId(0) });

        let analysis = analyze(&empty_func(vec![b0, b1]));
        assert_eq!(analysis.backedge_successors[1], vec![0]);
    }

    #[test]
    fn long_reverse_layout_dag_converges_without_iteration_cap() {
        let block_count = 160usize;
        let live = VReg(0);
        let mut blocks = Vec::new();

        for index in (0..block_count).rev() {
            let mut block = MBlock::new(BlockId(index as u32));
            if index + 1 == block_count {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: live,
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
            } else {
                block.push(MInst::Jump {
                    target: BlockId((index + 1) as u32),
                });
            }
            blocks.push(block);
        }

        let analysis = analyze(&empty_func(blocks));
        assert!(
            analysis.entry_distances[block_count - 1].contains_key(&live),
            "live value should propagate to BlockId(0) through the long DAG"
        );
    }

    #[test]
    fn completed_assignment_filters_phi_sources_by_exact_destination_row() {
        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        predecessor.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.phis.push(PhiNode {
            dst: VReg(1),
            sources: vec![(BlockId(0), VReg(0))],
        });
        successor.phis.push(PhiNode {
            dst: VReg(2),
            sources: vec![(BlockId(0), VReg(0))],
        });
        successor.push(MInst::Return);
        let mut values = VRegAllocator::new();
        for _ in 0..3 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 3]);
        function.blocks = vec![predecessor, successor];

        let mut assignment = AssignmentMap::default();
        assignment.set_phi_edge_location(
            BlockId(0),
            BlockId(1),
            VReg(1),
            VReg(0),
            EdgeLocation::Stack(0),
        );
        let one_register_row = analyze_for_assignment(&function, &assignment);
        assert!(one_register_row.exit_distances[0].contains_key(&VReg(0)));

        assignment.set_phi_edge_location(
            BlockId(0),
            BlockId(1),
            VReg(2),
            VReg(0),
            EdgeLocation::Immediate(7),
        );
        let no_register_rows = analyze_for_assignment(&function, &assignment);
        assert!(!no_register_rows.exit_distances[0].contains_key(&VReg(0)));
    }
}
