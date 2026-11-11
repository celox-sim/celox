use super::*;
use num_bigint::BigUint;

fn make_var_id(n: usize) -> celox_design::StateObjectId {
    celox_design::StateObjectId(n as u32)
}

fn make_test_addr(region: u32, inst_id: usize, var_id_val: usize) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region,
        instance_id: InstanceId(inst_id),
        var_id: make_var_id(var_id_val),
    }
}

#[test]
fn reverse_postorder_places_loop_dominators_before_lower_numbered_uses() {
    let mut blocks = HashMap::default();
    let mut insert = |id, terminator| {
        blocks.insert(
            BlockId(id),
            BasicBlock {
                id: BlockId(id),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator,
            },
        );
    };
    insert(
        0,
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(3), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    );
    insert(
        1,
        SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    );
    insert(2, SIRTerminator::Return);
    insert(3, SIRTerminator::Jump(BlockId(1), Vec::new()));
    insert(8, SIRTerminator::Return);
    insert(9, SIRTerminator::Jump(BlockId(8), Vec::new()));

    assert_eq!(
        reverse_postorder_blocks(&blocks, BlockId(0)),
        vec![
            BlockId(0),
            BlockId(3),
            BlockId(1),
            BlockId(2),
            BlockId(9),
            BlockId(8),
        ]
    );
}

/// Create a large single-block EU with many Store instructions.
fn make_large_eu(num_stores: usize) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut instructions = Vec::new();
    let mut register_map = HashMap::default();
    let mut reg_counter = 0;

    let addr = make_test_addr(0, 0, 0);

    for i in 0..num_stores {
        let load_reg = RegisterId(reg_counter);
        register_map.insert(
            load_reg,
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Load(
            load_reg,
            addr,
            SIROffset::Static(0),
            32,
        ));
        reg_counter += 1;

        let imm_reg = RegisterId(reg_counter);
        register_map.insert(
            imm_reg,
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Imm(
            imm_reg,
            SIRValue::new(BigUint::from(1u32)),
        ));
        reg_counter += 1;

        let result_reg = RegisterId(reg_counter);
        register_map.insert(
            result_reg,
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Binary(
            result_reg,
            load_reg,
            BinaryOp::Add,
            imm_reg,
        ));
        reg_counter += 1;

        let store_addr = make_test_addr(0, 0, i + 1);
        instructions.push(SIRInstruction::Store(
            store_addr,
            SIROffset::Static(0),
            32,
            result_reg,
            Vec::new(),
            Vec::new(),
        ));
    }

    let block = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions,
        terminator: SIRTerminator::Return,
    };

    let mut blocks = HashMap::default();
    blocks.insert(BlockId(0), block);

    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

#[test]
fn test_no_split_below_threshold() {
    let eu = make_large_eu(2);
    let result = split_with_threshold(&[eu], false, 1_000_000, usize::MAX);
    assert!(result.is_none());
}

#[test]
fn test_eu_boundary_split() {
    // Create multiple small EUs that together exceed a low threshold
    let eu1 = make_large_eu(10);
    let eu2 = make_large_eu(10);
    let eu3 = make_large_eu(10);

    // Use a threshold that fits one EU but not two
    let single_eu_cost = crate::cost_model::estimate_eu_cost(&eu1, false);
    let threshold = single_eu_cost + single_eu_cost / 2; // ~1.5× single EU

    let result = split_with_threshold(&[eu1, eu2, eu3], false, threshold, usize::MAX);
    assert!(result.is_some());
    let chunks = result.unwrap();
    assert!(chunks.len() >= 2);

    // EU-boundary splits have no live regs
    for chunk in &chunks {
        assert!(chunk.incoming_live_regs.is_empty());
        assert!(chunk.outgoing_live_regs.is_empty());
    }
}

#[test]
fn test_intra_eu_split() {
    // Create a single large EU that exceeds a low threshold
    let eu = make_large_eu(20);

    // Use a threshold that's about 1/3 the EU cost to force splitting
    let eu_cost = crate::cost_model::estimate_eu_cost(&eu, false);
    let threshold = eu_cost / 3;

    let result = split_with_threshold(&[eu], false, threshold, usize::MAX);
    assert!(result.is_some());
    let chunks = result.unwrap();
    assert!(chunks.len() >= 2);
}

#[test]
fn spilled_single_block_accounts_for_the_final_store_remainder() {
    for four_state in [false, true] {
        let threshold = estimate_eu_cost(&make_large_eu(2), four_state);
        let eu = make_large_eu(3);
        let instructions = eu.blocks[&BlockId(0)].instructions.len();
        let plan = split_multi_block_with_threshold(&[eu], four_state, threshold, threshold)
            .expect("the final third Store must trigger a cut");
        assert_eq!(plan.entry_chunks, vec![0]);
        assert_eq!(plan.chunks.len(), 2);
        assert_eq!(
            plan.chunks
                .iter()
                .flat_map(|chunk| chunk.eu.blocks.values())
                .map(|block| block.instructions.len())
                .sum::<usize>(),
            instructions
        );
        for chunk in &plan.chunks {
            assert!(estimate_eu_value_count(&chunk.eu, four_state) <= threshold);
        }
    }
}

#[test]
fn spilled_mixed_units_split_single_block_siblings_too() {
    for four_state in [false, true] {
        let threshold = estimate_eu_cost(&make_large_eu(2), four_state);
        let units = [
            make_large_eu(9),
            make_multi_block_chain_eu(5, 1),
            make_large_eu(7),
        ];
        let instructions: usize = units
            .iter()
            .flat_map(|eu| eu.blocks.values())
            .map(|block| block.instructions.len())
            .sum();
        let plan = split_multi_block_with_threshold(&units, four_state, threshold, threshold)
            .expect("all three oversized units must be split");
        assert_eq!(plan.entry_chunks.len(), 3);
        let ends = plan
            .entry_chunks
            .iter()
            .copied()
            .skip(1)
            .chain([plan.chunks.len()]);
        for (&start, end) in plan.entry_chunks.iter().zip(ends) {
            assert!(
                end - start > 1,
                "single-block siblings must not remain oversized"
            );
        }
        assert_eq!(
            plan.chunks
                .iter()
                .flat_map(|chunk| chunk.eu.blocks.values())
                .map(|block| block.instructions.len())
                .sum::<usize>(),
            instructions
        );
        for chunk in &plan.chunks {
            assert!(estimate_eu_value_count(&chunk.eu, four_state) <= threshold);
        }
    }
}

/// Create a multi-block EU with a chain: b0 → b1 → b2 → ... → bN (Return).
/// Each block has `stores_per_block` Load/Add/Store sequences.
/// Register r0 is defined in b0 and used (via block params) in all subsequent blocks,
/// testing inter-chunk liveness.
fn make_multi_block_chain_eu(
    num_blocks: usize,
    stores_per_block: usize,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut blocks = HashMap::default();
    let mut register_map = HashMap::default();
    let mut reg_counter = 0;
    let addr = make_test_addr(0, 0, 0);

    // Create a "shared" register defined in block 0
    let shared_reg = RegisterId(reg_counter);
    register_map.insert(
        shared_reg,
        RegisterType::Bit {
            width: 32,
            signed: false,
        },
    );
    reg_counter += 1;

    for b in 0..num_blocks {
        let block_id = BlockId(b);
        let mut instructions = Vec::new();
        let mut params = Vec::new();

        if b == 0 {
            // Define shared_reg in block 0
            instructions.push(SIRInstruction::Imm(
                shared_reg,
                SIRValue::new(BigUint::from(42u32)),
            ));
        } else {
            // Block params: receive shared_reg from predecessor
            let param_reg = RegisterId(reg_counter);
            register_map.insert(
                param_reg,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            );
            reg_counter += 1;
            params.push(param_reg);

            // Use the param in a store
            instructions.push(SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                32,
                param_reg,
                Vec::new(),
                Vec::new(),
            ));
        }

        // Each block has load/add/store sequences
        for i in 0..stores_per_block {
            let load_reg = RegisterId(reg_counter);
            register_map.insert(
                load_reg,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Load(
                load_reg,
                addr,
                SIROffset::Static(0),
                32,
            ));
            reg_counter += 1;

            let imm_reg = RegisterId(reg_counter);
            register_map.insert(
                imm_reg,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Imm(
                imm_reg,
                SIRValue::new(BigUint::from(1u32)),
            ));
            reg_counter += 1;

            let result_reg = RegisterId(reg_counter);
            register_map.insert(
                result_reg,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Binary(
                result_reg,
                load_reg,
                BinaryOp::Add,
                imm_reg,
            ));
            reg_counter += 1;

            let store_addr = make_test_addr(0, 0, b * stores_per_block + i + 1);
            instructions.push(SIRInstruction::Store(
                store_addr,
                SIROffset::Static(0),
                32,
                result_reg,
                Vec::new(),
                Vec::new(),
            ));
        }

        let terminator = if b + 1 < num_blocks {
            // Pass shared_reg (or its param) to next block
            let pass_reg = if b == 0 { shared_reg } else { params[0] };
            SIRTerminator::Jump(BlockId(b + 1), vec![pass_reg])
        } else {
            SIRTerminator::Return
        };

        blocks.insert(
            block_id,
            BasicBlock {
                id: block_id,
                params,
                instructions,
                terminator,
            },
        );
    }

    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

#[test]
fn test_multi_block_spilled_split() {
    // Create a multi-block EU with enough blocks/instructions to exceed a low threshold
    let eu = make_multi_block_chain_eu(6, 5);
    let eu_cost = crate::cost_model::estimate_eu_cost(&eu, false);
    // Use ~1/4 of EU cost as threshold to force multiple chunks
    let threshold = eu_cost / 4;
    assert!(
        eu_cost > threshold,
        "EU cost should exceed our test threshold, got {eu_cost}"
    );

    let result = split_multi_block_with_threshold(&[eu], false, threshold, usize::MAX);
    assert!(result.is_some(), "Should produce a spilled plan");

    let plan = result.unwrap();
    assert!(
        plan.chunks.len() >= 2,
        "Should have at least 2 chunks, got {}",
        plan.chunks.len()
    );

    // Each chunk should have a single entry block
    for (i, chunk) in plan.chunks.iter().enumerate() {
        assert!(
            !chunk.eu.blocks.is_empty(),
            "Chunk {} should have at least one block",
            i
        );
    }

    // Per-chunk spills: not all chunks should have the same spills
    // (first chunk has no incoming spills since it's the entry)
    assert!(
        plan.chunks[0].incoming_spills.is_empty()
            || plan.chunks[0].incoming_spills.len()
                < plan.chunks.last().unwrap().incoming_spills.len()
            || plan.chunks.len() == 2, // 2 chunks: first may have no incoming, second has some
        "First chunk should generally have fewer incoming spills"
    );

    // Scratch bytes should be non-zero (we have inter-chunk live regs)
    assert!(plan.scratch_bytes > 0, "Should need scratch memory");
}

#[test]
fn test_partition_single_pass_basic() {
    // Chain: b0 → b1 → b2 → b3 → Return
    let eu = make_multi_block_chain_eu(4, 3);
    let block_order = reverse_postorder_blocks(&eu.blocks, eu.entry_block_id);

    let block_costs: HashMap<BlockId, (usize, usize)> = block_order
        .iter()
        .map(|&bid| {
            (
                bid,
                (
                    estimate_block_cost(&eu, bid, false),
                    estimate_block_value_count(&eu, bid, false),
                ),
            )
        })
        .collect();

    // Use threshold smaller than any single block cost to force many chunks
    let max_block_cost = block_costs.values().map(|&(ic, _)| ic).max().unwrap_or(1);
    let threshold = max_block_cost; // fits exactly one block per chunk
    let groups = partition_single_pass(&eu, &block_order, &block_costs, threshold, usize::MAX);
    assert!(
        groups.len() >= 2,
        "Should have multiple chunks with low threshold"
    );

    // Each chunk's first block should be the only entry point
    // (single-entry guarantee)
    let mut block_to_chunk: HashMap<BlockId, usize> = HashMap::default();
    for (ci, group) in groups.iter().enumerate() {
        for &bid in group {
            block_to_chunk.insert(bid, ci);
        }
    }

    // Verify: for each cross-chunk edge, the target is the first block of its chunk
    for group in &groups {
        for &bid in group {
            let block = &eu.blocks[&bid];
            for succ in block_successors(&block.terminator) {
                if let Some(&succ_chunk) = block_to_chunk.get(&succ) {
                    let src_chunk = block_to_chunk[&bid];
                    if succ_chunk != src_chunk {
                        assert_eq!(
                            succ, groups[succ_chunk][0],
                            "Cross-chunk target b{} should be head of chunk {}",
                            succ.0, succ_chunk
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn test_multi_block_cross_chunk_edges_complete() {
    // Verify that cross_chunk_edges include ALL block params (no silent dropping)
    let eu = make_multi_block_chain_eu(4, 3);
    // Use threshold that forces at least 2 chunks
    let eu_cost = crate::cost_model::estimate_eu_cost(&eu, false);
    let threshold = eu_cost / 3;
    let result =
        split_multi_block_with_threshold(std::slice::from_ref(&eu), false, threshold, usize::MAX);

    if let Some(plan) = result {
        for (ci, chunk) in plan.chunks.iter().enumerate() {
            for (target_bid, edge) in &chunk.cross_chunk_edges {
                let target_block = &eu.blocks.get(target_bid).or_else(|| {
                    // Target might be in a different chunk's sub-EU
                    plan.chunks.iter().find_map(|c| c.eu.blocks.get(target_bid))
                });
                if let Some(target_block) = target_block {
                    assert_eq!(
                        edge.param_scratch_offsets.len(),
                        target_block.params.len(),
                        "Chunk {} edge to b{}: scratch offsets count ({}) should match param count ({})",
                        ci,
                        target_bid.0,
                        edge.param_scratch_offsets.len(),
                        target_block.params.len(),
                    );
                }
            }
        }
    }
}
