use crate::HashMap;
use crate::HashSet;
use crate::ir::*;
use crate::optimizer::PassOptions;

use super::pass_manager::ExecutionUnitPass;

pub(super) struct CoalesceStoresPass;

impl ExecutionUnitPass for CoalesceStoresPass {
    fn name(&self) -> &'static str {
        "coalesce_stores"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut reg_counter = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);

        for block in eu.blocks.values_mut() {
            coalesce_block(block, &mut eu.register_map, &mut reg_counter);
        }
    }
}

/// A candidate Store instruction within a block.
struct StoreCandidate {
    inst_index: usize,
    offset: usize,
    width: usize,
    src_reg: RegisterId,
}

fn coalesce_block(
    block: &mut BasicBlock<RegionedAbsoluteAddr>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    reg_counter: &mut usize,
) {
    // Step 1: Collect Store groups, sealing on reads.
    //
    // groups: addr -> vec of candidates (current open group)
    // sealed_groups: completed groups ready for coalescing analysis
    let mut groups: HashMap<RegionedAbsoluteAddr, Vec<StoreCandidate>> = HashMap::default();
    let mut sealed_groups: Vec<Vec<StoreCandidate>> = Vec::new();

    for (i, inst) in block.instructions.iter().enumerate() {
        match inst {
            SIRInstruction::Store(addr, SIROffset::Static(off), width, src, triggers)
                if triggers.is_empty() =>
            {
                groups
                    .entry(addr.clone())
                    .or_default()
                    .push(StoreCandidate {
                        inst_index: i,
                        offset: *off,
                        width: *width,
                        src_reg: *src,
                    });
            }
            SIRInstruction::Load(_, addr, _, _) => {
                seal_group(&mut groups, addr, &mut sealed_groups);
            }
            SIRInstruction::Commit(src, _, _, _, _) => {
                // Commit reads from src region
                seal_group(&mut groups, src, &mut sealed_groups);
            }
            // A Store with Dynamic offset could alias any static offset
            SIRInstruction::Store(addr, SIROffset::Dynamic(_), _, _, _) => {
                seal_group(&mut groups, addr, &mut sealed_groups);
            }
            _ => {}
        }
    }

    // Flush remaining open groups
    for (_, candidates) in groups.drain() {
        if candidates.len() >= 2 {
            sealed_groups.push(candidates);
        }
    }

    // Step 2: Find contiguous sub-runs in each group
    struct Replacement {
        removed_indices: Vec<usize>,
        anchor_index: usize,
        new_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    }

    let mut replacements: Vec<Replacement> = Vec::new();

    for mut group in sealed_groups {
        if group.len() < 2 {
            continue;
        }

        group.sort_by_key(|c| c.offset);

        let mut run_start = 0;
        while run_start < group.len() {
            let mut run_end = run_start;
            while run_end + 1 < group.len()
                && group[run_end + 1].offset == group[run_end].offset + group[run_end].width
            {
                run_end += 1;
            }

            let run_len = run_end - run_start + 1;
            if run_len >= 2 {
                let sub_run = &group[run_start..=run_end];
                let merged_lsb = sub_run[0].offset;
                let total_width: usize = sub_run.iter().map(|c| c.width).sum();
                let anchor_index = sub_run.iter().map(|c| c.inst_index).max().unwrap();
                let removed_indices: Vec<usize> =
                    sub_run.iter().map(|c| c.inst_index).collect();

                    // Allocate new register for Concat result
                    *reg_counter += 1;
                    while register_map.contains_key(&RegisterId(*reg_counter)) {
                        *reg_counter += 1;
                    }
                    let concat_reg = RegisterId(*reg_counter);
                    register_map.insert(
                        concat_reg,
                        RegisterType::Bit {
                            width: total_width,
                            signed: false,
                        },
                    );

                    // Concat order: MSB first, so reverse the LSB-sorted order
                    let concat_args: Vec<RegisterId> =
                        sub_run.iter().rev().map(|c| c.src_reg).collect();

                    // Get the addr from the first candidate (all share the same addr)
                    let addr = if let SIRInstruction::Store(addr, _, _, _, _) =
                        &block.instructions[sub_run[0].inst_index]
                    {
                        addr.clone()
                    } else {
                        unreachable!()
                    };

                    let mut new_instructions = Vec::with_capacity(2);
                    new_instructions
                        .push(SIRInstruction::Concat(concat_reg, concat_args));
                    new_instructions.push(SIRInstruction::Store(
                        addr,
                        SIROffset::Static(merged_lsb),
                        total_width,
                        concat_reg,
                        Vec::new(),
                    ));

                    replacements.push(Replacement {
                        removed_indices,
                        anchor_index,
                        new_instructions,
                    });
            }

            run_start = run_end + 1;
        }
    }

    if replacements.is_empty() {
        return;
    }

    // Step 4: Rebuild instruction vector
    let mut removed_set: HashSet<usize> = HashSet::default();
    // Map from anchor index -> new instructions to insert (replacing the anchor)
    let mut insert_map: HashMap<usize, Vec<SIRInstruction<RegionedAbsoluteAddr>>> =
        HashMap::default();

    for repl in replacements {
        for &idx in &repl.removed_indices {
            removed_set.insert(idx);
        }
        insert_map
            .entry(repl.anchor_index)
            .or_default()
            .extend(repl.new_instructions);
    }

    let old_instructions = std::mem::take(&mut block.instructions);
    let mut new_instructions = Vec::with_capacity(old_instructions.len());

    for (i, inst) in old_instructions.into_iter().enumerate() {
        if let Some(replacement) = insert_map.remove(&i) {
            // This is an anchor point: insert Concat + wide Store(s)
            new_instructions.extend(replacement);
            // The anchor itself is also a removed store — don't re-add it
        } else if !removed_set.contains(&i) {
            new_instructions.push(inst);
        }
    }

    block.instructions = new_instructions;
}

fn seal_group(
    groups: &mut HashMap<RegionedAbsoluteAddr, Vec<StoreCandidate>>,
    addr: &RegionedAbsoluteAddr,
    sealed_groups: &mut Vec<Vec<StoreCandidate>>,
) {
    if let Some(candidates) = groups.remove(addr) {
        if candidates.len() >= 2 {
            sealed_groups.push(candidates);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn make_var_id(n: u32) -> veryl_analyzer::ir::VarId {
        let mut id = veryl_analyzer::ir::VarId::default();
        for _ in 0..n {
            id.inc();
        }
        id
    }

    fn make_addr(var_id: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(0),
            var_id: make_var_id(var_id),
        }
    }

    fn make_eu(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        register_map: HashMap<RegisterId, RegisterType>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    #[test]
    fn test_basic_contiguous_coalescing() {
        // 3 contiguous 1-bit stores to bits 0, 1, 2 of the same variable
        let addr = make_addr(0);
        let mut register_map = HashMap::default();
        for i in 0..3 {
            register_map.insert(
                RegisterId(i),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }

        let instructions = vec![
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(0),
                1,
                RegisterId(0),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(1),
                1,
                RegisterId(1),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(2),
                1,
                RegisterId(2),
                Vec::new(),
            ),
        ];

        let mut eu = make_eu(instructions, register_map);
        let options = PassOptions::default();
        CoalesceStoresPass.run(&mut eu, &options);

        let block = eu.blocks.get(&BlockId(0)).unwrap();
        // Should be 2 instructions: Concat + wide Store
        assert_eq!(block.instructions.len(), 2);
        match &block.instructions[0] {
            SIRInstruction::Concat(dst, args) => {
                assert_eq!(args.len(), 3);
                // MSB first: r2, r1, r0
                assert_eq!(args[0], RegisterId(2));
                assert_eq!(args[1], RegisterId(1));
                assert_eq!(args[2], RegisterId(0));
                // Verify the concat register has correct width
                let reg_type = eu.register_map.get(dst).unwrap();
                assert_eq!(reg_type.width(), 3);
            }
            other => panic!("Expected Concat, got {:?}", other),
        }
        match &block.instructions[1] {
            SIRInstruction::Store(_, SIROffset::Static(0), 3, _, triggers) => {
                assert!(triggers.is_empty());
            }
            other => panic!("Expected wide Store, got {:?}", other),
        }
    }

    #[test]
    fn test_interleaved_stores_different_vars() {
        // Stores to var0 interleaved with stores to var1 — both should coalesce
        let addr0 = make_addr(0);
        let addr1 = make_addr(1);
        let mut register_map = HashMap::default();
        for i in 0..4 {
            register_map.insert(
                RegisterId(i),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }

        let instructions = vec![
            SIRInstruction::Store(
                addr0.clone(),
                SIROffset::Static(0),
                1,
                RegisterId(0),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr1.clone(),
                SIROffset::Static(0),
                1,
                RegisterId(2),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr0.clone(),
                SIROffset::Static(1),
                1,
                RegisterId(1),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr1.clone(),
                SIROffset::Static(1),
                1,
                RegisterId(3),
                Vec::new(),
            ),
        ];

        let mut eu = make_eu(instructions, register_map);
        let options = PassOptions::default();
        CoalesceStoresPass.run(&mut eu, &options);

        let block = eu.blocks.get(&BlockId(0)).unwrap();
        // 4 instructions: Concat+Store for addr0, Concat+Store for addr1
        assert_eq!(block.instructions.len(), 4);
    }

    #[test]
    fn test_seal_on_load() {
        // Store bit 0, Load from same addr, Store bit 1 — should NOT coalesce
        let addr = make_addr(0);
        let mut register_map = HashMap::default();
        for i in 0..3 {
            register_map.insert(
                RegisterId(i),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }

        let instructions = vec![
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(0),
                1,
                RegisterId(0),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(2), addr.clone(), SIROffset::Static(0), 1),
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(1),
                1,
                RegisterId(1),
                Vec::new(),
            ),
        ];

        let mut eu = make_eu(instructions, register_map);
        let options = PassOptions::default();
        CoalesceStoresPass.run(&mut eu, &options);

        let block = eu.blocks.get(&BlockId(0)).unwrap();
        // No coalescing should happen — 3 original instructions remain
        assert_eq!(block.instructions.len(), 3);
    }

    #[test]
    fn test_non_contiguous_stores_not_coalesced() {
        // Stores at offset 0 and offset 2 (gap at 1) — should NOT coalesce
        let addr = make_addr(0);
        let mut register_map = HashMap::default();
        for i in 0..2 {
            register_map.insert(
                RegisterId(i),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }

        let instructions = vec![
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(0),
                1,
                RegisterId(0),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(2),
                1,
                RegisterId(1),
                Vec::new(),
            ),
        ];

        let mut eu = make_eu(instructions, register_map);
        let options = PassOptions::default();
        CoalesceStoresPass.run(&mut eu, &options);

        let block = eu.blocks.get(&BlockId(0)).unwrap();
        assert_eq!(block.instructions.len(), 2);
    }

    #[test]
    fn test_stores_with_triggers_not_coalesced() {
        let addr = make_addr(0);
        let mut register_map = HashMap::default();
        for i in 0..2 {
            register_map.insert(
                RegisterId(i),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }

        let trigger = TriggerIdWithKind {
            id: 0,
            kind: DomainKind::ClockPosedge,
        };

        let instructions = vec![
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(0),
                1,
                RegisterId(0),
                vec![trigger.clone()],
            ),
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(1),
                1,
                RegisterId(1),
                vec![trigger],
            ),
        ];

        let mut eu = make_eu(instructions, register_map);
        let options = PassOptions::default();
        CoalesceStoresPass.run(&mut eu, &options);

        let block = eu.blocks.get(&BlockId(0)).unwrap();
        // Triggered stores should not be coalesced
        assert_eq!(block.instructions.len(), 2);
    }

    #[test]
    fn test_partial_contiguous_run() {
        // 4 stores: bits 0,1,2 are contiguous, bit 5 is separate
        let addr = make_addr(0);
        let mut register_map = HashMap::default();
        for i in 0..4 {
            register_map.insert(
                RegisterId(i),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
        }

        let instructions = vec![
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(0),
                1,
                RegisterId(0),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(1),
                1,
                RegisterId(1),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(2),
                1,
                RegisterId(2),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr.clone(),
                SIROffset::Static(5),
                1,
                RegisterId(3),
                Vec::new(),
            ),
        ];

        let mut eu = make_eu(instructions, register_map);
        let options = PassOptions::default();
        CoalesceStoresPass.run(&mut eu, &options);

        let block = eu.blocks.get(&BlockId(0)).unwrap();
        // 3 instructions: Concat + wide Store (for bits 0-2) + original Store(bit 5)
        assert_eq!(block.instructions.len(), 3);
    }
}
