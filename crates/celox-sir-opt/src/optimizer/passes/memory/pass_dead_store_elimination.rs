use crate::ir::*;
use crate::{HashMap, HashSet, OptimizationContext};

/// Remove stores from `eval_comb` and its lane-partitioned alternative whose
/// target addresses are not live.
///
/// A store's address is considered live if:
/// - It is in `externally_live` (user-specified observable signals), OR
/// - Any execution unit Loads from it (or Commits from it), or calls a
///   function that may read it, OR
/// - It has a dynamic offset (conservative), OR
/// - The store has non-empty triggers (edge-detection side effect), OR
/// - The store has non-empty comb capture sites (observer activation side effect).
///
/// `shared_storage` maps an address to the representative of the addresses
/// whose state shares its home, such as an identity alias and its canonical
/// address. Liveness is decided per home: a Load of an alias reads what the
/// canonical address's Store wrote.
pub(crate) fn eliminate_dead_stores(
    program: &mut OptimizationContext,
    externally_live: &HashSet<AbsoluteAddr>,
    shared_storage: &HashMap<AbsoluteAddr, AbsoluteAddr>,
) {
    let home = |addr: AbsoluteAddr| shared_storage.get(&addr).copied().unwrap_or(addr);
    let externally_live = externally_live
        .iter()
        .map(|&addr| home(addr))
        .collect::<HashSet<_>>();

    // 1. Collect all homes loaded across ALL execution units.
    let mut loaded_addrs: HashSet<AbsoluteAddr> = HashSet::default();
    let mut dynamic_addrs: HashSet<AbsoluteAddr> = HashSet::default();

    let all_eus = program
        .sir
        .eval_comb
        .iter()
        .chain(
            program
                .sir
                .eval_apply_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .sir
                .eval_comb_apply_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .sir
                .eval_only_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .sir
                .apply_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .sir
                .parallel
                .iter()
                .flat_map(|parallel| parallel.units().map(|unit| &unit.unit)),
        )
        .chain(&program.sir.processes)
        // A function's loads read what the units calling it stored.
        .chain(program.sir.functions.iter().map(|function| &function.unit));

    for eu in all_eus {
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(_, addr, offset, _)
                        if offset.constant_bit_offset().is_some() =>
                    {
                        loaded_addrs.insert(home(addr.absolute_addr()));
                    }
                    SIRInstruction::Load(
                        _,
                        addr,
                        SIROffset::Dynamic(_) | SIROffset::Element { .. },
                        _,
                    ) => {
                        let key = home(addr.absolute_addr());
                        loaded_addrs.insert(key);
                        dynamic_addrs.insert(key);
                    }
                    SIRInstruction::Commit(src, _, offset, _, _)
                        if offset.constant_bit_offset().is_some() =>
                    {
                        loaded_addrs.insert(home(src.absolute_addr()));
                    }
                    SIRInstruction::Commit(
                        src,
                        _,
                        SIROffset::Dynamic(_) | SIROffset::Element { .. },
                        _,
                        _,
                    ) => {
                        let key = home(src.absolute_addr());
                        loaded_addrs.insert(key);
                        dynamic_addrs.insert(key);
                    }
                    // A call reads any part of the objects its effects list.
                    other => match other.state_reads() {
                        StateAccess::Objects(objects) => {
                            loaded_addrs
                                .extend(objects.iter().map(|object| home(object.absolute_addr())));
                        }
                        // A native kernel may read any state, so every store
                        // stays.
                        StateAccess::Everything => return,
                        StateAccess::None | StateAccess::Access(_) => {}
                    },
                }
            }
        }
    }

    // 2. Remove dead stores from eval_comb.
    let parallel_comb = program
        .sir
        .parallel
        .iter_mut()
        .flat_map(|parallel| parallel.eval_comb.iter_mut().map(|unit| &mut unit.unit));
    for eu in program.sir.eval_comb.iter_mut().chain(parallel_comb) {
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|inst| {
                match inst {
                    SIRInstruction::Store(addr, offset, _, _, triggers, comb_capture_sites)
                        if offset.constant_bit_offset().is_some()
                            && triggers.is_empty()
                            && comb_capture_sites.is_empty() =>
                    {
                        let abs = home(addr.absolute_addr());
                        externally_live.contains(&abs)
                            || loaded_addrs.contains(&abs)
                            || dynamic_addrs.contains(&abs)
                    }
                    // Keep stores with dynamic offsets or triggers unconditionally.
                    _ => true,
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, BlockId, InstanceId, SIRTerminator, STABLE_REGION, StateObjectId};

    fn address(id: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: StateObjectId(id),
        }
    }

    fn unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: [(
                RegisterId(0),
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    fn store(id: u32) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(id),
            SIROffset::Static(0),
            8,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        )
    }

    fn call(reads: Vec<RegionedAbsoluteAddr>) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Call {
            function: celox_sir::FunctionId(0),
            frame: RegisterId(0),
            args: Vec::new(),
            outputs: Vec::new(),
            dst: None,
            effects: std::sync::Arc::new(celox_sir::CallEffects {
                reads,
                ..Default::default()
            }),
        }
    }

    /// The stores of objects 1 to 4 that survive when a call reads object 2
    /// and a function unit loads object 3.
    fn surviving_stores() -> Vec<u32> {
        let mut sir = SirProgram {
            eval_comb: vec![unit(vec![
                store(1),
                store(2),
                store(3),
                store(4),
                call(vec![address(2)]),
            ])],
            eval_apply_ffs: HashMap::default(),
            eval_comb_apply_ffs: HashMap::default(),
            merge_comb_apply: false,
            eval_comb_stateful: None,
            eval_only_ffs: HashMap::default(),
            apply_ffs: HashMap::default(),
            parallel: None,
            processes: Vec::new(),
            functions: vec![celox_sir::FunctionUnit {
                unit: unit(vec![SIRInstruction::Load(
                    RegisterId(0),
                    address(3),
                    SIROffset::Static(0),
                    8,
                )]),
                frame: celox_sir::FrameLayout {
                    size: 0,
                    slots: Vec::new(),
                },
                effects: Default::default(),
            }],
        };
        let design = celox_design::ElaboratedDesign::default();
        let runtime_schema = celox_design::RuntimeSchema::default();
        let mut layout_requirements = celox_state_layout::LayoutRequirements::default();
        let mut program = OptimizationContext {
            sir: &mut sir,
            design: &design,
            runtime_schema: &runtime_schema,
            layout_requirements: &mut layout_requirements,
        };
        eliminate_dead_stores(&mut program, &HashSet::default(), &HashMap::default());
        sir.eval_comb[0].blocks[&BlockId(0)]
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(address, ..) => Some(address.var_id.0),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn stores_read_by_a_call_or_a_function_unit_stay() {
        assert_eq!(surviving_stores(), [2, 3]);
    }
}
