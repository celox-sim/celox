//! CFG-exact first-write certificates for sparse next-state lowering.
//!
//! Sparse dirty metadata is clear at function entry when the function's full
//! commit run executes on every path following a sparse Store.  A Store may
//! therefore initialize its touched working chunk directly from stable state
//! when the Store's per-object MemorySSA use reaches LiveOnEntry.
//!
//! The alias domain is one sparse object (`AbsoluteAddr`) rather than one bit
//! or byte.  This is deliberately conservative for dynamic offsets, while the
//! shared pruned-SSA construction keeps both time and storage linear in CFG
//! edges, sparse Stores, and the MemoryPhi inputs it creates.

use celox_analysis::ssa::{self, Event, Version};

use crate::HashMap;
use crate::ir::cfg::SirCfg;
use crate::ir::{AbsoluteAddr, BlockId, ExecutionUnit, RegionedAbsoluteAddr, SIRInstruction};

type StorePoint = (usize, usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SparseWriteState {
    First,
    Active,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MemoryDefinitionKind {
    Store,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MemoryDefinition {
    point: StorePoint,
    kind: MemoryDefinitionKind,
}

#[derive(Debug, Default)]
pub(super) struct SparseFirstWrites {
    states: HashMap<(BlockId, usize), SparseWriteState>,
}

impl SparseFirstWrites {
    pub(super) fn analyze(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        commit_block: BlockId,
        commit_start: usize,
    ) -> Option<Self> {
        let cfg = SirCfg::analyze(eu).ok()?;
        let commit_index = cfg.block_index(commit_block)?;
        let mut events = vec![
            Vec::<Event<AbsoluteAddr, MemoryDefinition, StorePoint>>::new();
            cfg.block_ids.len()
        ];
        let mut stores = Vec::<(StorePoint, BlockId, AbsoluteAddr)>::new();

        for (block_index, &block_id) in cfg.block_ids.iter().enumerate() {
            for (instruction, inst) in eu.blocks[&block_id].instructions.iter().enumerate() {
                let point = (block_index, instruction);
                match inst {
                    SIRInstruction::Store(address, _, width, _, _, _)
                        if address.region == crate::ir::SPARSE_WORKING_REGION && *width != 0 =>
                    {
                        let object = address.absolute_addr();
                        events[block_index].push(Event::Use {
                            variable: object,
                            usage: point,
                        });
                        events[block_index].push(Event::Definition {
                            variable: object,
                            definition: MemoryDefinition {
                                point,
                                kind: MemoryDefinitionKind::Store,
                            },
                        });
                        stores.push((point, block_id, object));
                    }
                    SIRInstruction::Commit(source, destination, ..)
                        if source.region == crate::ir::SPARSE_WORKING_REGION
                            && destination.region == crate::ir::STABLE_REGION =>
                    {
                        events[block_index].push(Event::Definition {
                            variable: source.absolute_addr(),
                            definition: MemoryDefinition {
                                point,
                                kind: MemoryDefinitionKind::Reset,
                            },
                        });
                    }
                    _ => {}
                }
            }
        }

        let memory_ssa = ssa::build(&cfg, &events).ok()?;
        let mut states = HashMap::default();
        for (point, block_id, object) in stores {
            let state = match memory_ssa.uses.get(&point) {
                Some(Version::Definition {
                    definition:
                        MemoryDefinition {
                            kind: MemoryDefinitionKind::Store,
                            ..
                        },
                    ..
                }) => SparseWriteState::Active,
                Some(Version::Entry(entry_object)) if *entry_object == object => {
                    let block_index = cfg.block_index(block_id)?;
                    let commit_follows_store = if block_id == commit_block {
                        point.1 < commit_start
                    } else {
                        cfg.scc_for_block[block_index] != cfg.scc_for_block[commit_index]
                            && cfg.postdominates(commit_block, block_id)
                    };
                    if commit_follows_store {
                        SparseWriteState::First
                    } else {
                        SparseWriteState::Unknown
                    }
                }
                Some(Version::Definition {
                    definition:
                        MemoryDefinition {
                            kind: MemoryDefinitionKind::Reset,
                            ..
                        },
                    ..
                })
                | Some(Version::Phi { .. })
                | Some(Version::Entry(_))
                | None => SparseWriteState::Unknown,
            };
            states.insert((block_id, point.1), state);
        }
        Some(Self { states })
    }

    pub(super) fn state(&self, block: BlockId, instruction: usize) -> SparseWriteState {
        self.states
            .get(&(block, instruction))
            .copied()
            .unwrap_or(SparseWriteState::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashMap;
    use crate::ir::{
        BasicBlock, InstanceId, RegisterId, RegisterType, SIROffset, SIRTerminator, STABLE_REGION,
    };
    use veryl_analyzer::ir::VarId;

    fn address(region: u32, variable: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(variable),
        }
    }

    fn store(variable: u32, source: RegisterId) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(crate::ir::SPARSE_WORKING_REGION, variable),
            SIROffset::Static(0),
            1,
            source,
            vec![],
            vec![],
        )
    }

    fn commit(variable: u32) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Commit(
            address(crate::ir::SPARSE_WORKING_REGION, variable),
            address(STABLE_REGION, variable),
            SIROffset::Static(0),
            1,
            vec![],
        )
    }

    fn eu(
        blocks: impl IntoIterator<Item = (BlockId, BasicBlock<RegionedAbsoluteAddr>)>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().collect(),
            register_map: [(
                RegisterId(0),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            )]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn straight_line_marks_only_each_objects_first_store() {
        let unit = eu([(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    store(0, RegisterId(0)),
                    store(0, RegisterId(0)),
                    store(1, RegisterId(0)),
                    commit(0),
                    commit(1),
                ],
                terminator: SIRTerminator::Return,
            },
        )]);
        let facts = SparseFirstWrites::analyze(&unit, BlockId(0), 3).unwrap();

        assert_eq!(facts.state(BlockId(0), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(0), 1), SparseWriteState::Active);
        assert_eq!(facts.state(BlockId(0), 2), SparseWriteState::First);
    }

    #[test]
    fn mutually_exclusive_arm_stores_are_both_first_writes() {
        let unit = eu([
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(2),
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(3),
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![commit(0)],
                    terminator: SIRTerminator::Return,
                },
            ),
        ]);
        let facts = SparseFirstWrites::analyze(&unit, BlockId(3), 0).unwrap();

        assert_eq!(facts.state(BlockId(1), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(2), 0), SparseWriteState::First);
    }

    #[test]
    fn join_after_a_maybe_store_is_not_a_first_write() {
        let unit = eu([
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(2),
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![]),
                },
            ),
            (
                BlockId(3),
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(4), vec![]),
                },
            ),
            (
                BlockId(4),
                BasicBlock {
                    id: BlockId(4),
                    params: vec![],
                    instructions: vec![commit(0)],
                    terminator: SIRTerminator::Return,
                },
            ),
        ]);
        let facts = SparseFirstWrites::analyze(&unit, BlockId(4), 0).unwrap();

        assert_eq!(facts.state(BlockId(1), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(3), 0), SparseWriteState::Unknown);
    }

    #[test]
    fn loop_backedge_prevents_a_first_write_certificate() {
        let unit = eu([
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Jump(BlockId(1), vec![]),
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(2), vec![]),
                        false_block: (BlockId(3), vec![]),
                    },
                },
            ),
            (
                BlockId(2),
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![store(0, RegisterId(0))],
                    terminator: SIRTerminator::Jump(BlockId(1), vec![]),
                },
            ),
            (
                BlockId(3),
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    instructions: vec![commit(0)],
                    terminator: SIRTerminator::Return,
                },
            ),
        ]);
        let facts = SparseFirstWrites::analyze(&unit, BlockId(3), 0).unwrap();

        assert_eq!(facts.state(BlockId(2), 0), SparseWriteState::Unknown);
    }

    #[test]
    fn commit_reset_prevents_active_state_from_crossing_it() {
        let unit = eu([(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    store(0, RegisterId(0)),
                    commit(0),
                    store(0, RegisterId(0)),
                    commit(0),
                ],
                terminator: SIRTerminator::Return,
            },
        )]);
        let facts = SparseFirstWrites::analyze(&unit, BlockId(0), 3).unwrap();

        assert_eq!(facts.state(BlockId(0), 0), SparseWriteState::First);
        assert_eq!(facts.state(BlockId(0), 2), SparseWriteState::Unknown);
    }
}
