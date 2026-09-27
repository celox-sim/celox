use super::*;

#[test]
fn recognizes_full_domain_selector_branch_spine() {
    let fixture = dense_branch_table_fixture();
    let constants = collect_exact_sir_constants(&fixture);
    let uses = collect_sir_use_sites(&fixture);
    let plans = find_selector_branch_table_plans(&fixture, &constants, &uses);

    let plan = &plans.roots[&SirBlockId(0)];
    assert_eq!(plan.selector, RegisterId(0));
    assert_eq!(plan.selector_width, 2);
    assert_eq!(
        plan.targets.as_ref(),
        &[SirBlockId(4), SirBlockId(5), SirBlockId(6), SirBlockId(7)]
    );
    assert_eq!(
        plans.removed_blocks,
        [SirBlockId(1), SirBlockId(2), SirBlockId(3)]
            .into_iter()
            .collect()
    );
    assert_eq!(plan.skip_indices, [0, 1, 2, 3].into_iter().collect());
}

#[test]
fn recognizes_partial_selector_branch_spine_with_original_default() {
    let mut fixture = dense_branch_table_fixture();
    let bit3 = RegisterType::Bit {
        width: 3,
        signed: false,
    };
    fixture.register_map.insert(RegisterId(0), bit3.clone());
    for key_register in [1, 5, 9, 13].map(RegisterId) {
        fixture.register_map.insert(key_register, bit3.clone());
    }
    let constants = collect_exact_sir_constants(&fixture);
    let uses = collect_sir_use_sites(&fixture);
    let plans = find_selector_branch_table_plans(&fixture, &constants, &uses);

    let plan = &plans.roots[&SirBlockId(0)];
    assert_eq!(plan.selector_width, 3);
    assert_eq!(
        plan.targets.as_ref(),
        &[
            SirBlockId(4),
            SirBlockId(5),
            SirBlockId(6),
            SirBlockId(7),
            SirBlockId(7),
            SirBlockId(7),
            SirBlockId(7),
            SirBlockId(7),
        ]
    );
    assert_eq!(
        plans.removed_blocks,
        [SirBlockId(1), SirBlockId(2), SirBlockId(3)]
            .into_iter()
            .collect()
    );
}

#[test]
fn lowers_a_single_target_switch_to_an_unconditional_jump() {
    let selector = RegisterId(0);
    let blocks = [
        (
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Imm(selector, SIRValue::new(0_u8))],
                terminator: SIRTerminator::Switch {
                    selector,
                    cases: vec![
                        crate::SIRSwitchCase {
                            value: BigUint::from(1_u8),
                            target: SirBlockId(1),
                        },
                        crate::SIRSwitchCase {
                            value: BigUint::from(7_u8),
                            target: SirBlockId(1),
                        },
                    ],
                    default: SirBlockId(1),
                },
            },
        ),
        (
            SirBlockId(1),
            BasicBlock {
                id: SirBlockId(1),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Return,
            },
        ),
    ]
    .into_iter()
    .collect();
    let eu = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks,
        register_map: [(
            selector,
            RegisterType::Bit {
                width: 7,
                signed: false,
            },
        )]
        .into_iter()
        .collect(),
    };

    let function = lower_execution_unit(&eu, &empty_layout(), false);
    assert!(matches!(
        function.blocks[0].insts.last(),
        Some(MInst::Jump { target: BlockId(1) })
    ));
    assert!(
        !function.blocks[0]
            .insts
            .iter()
            .any(|instruction| matches!(instruction, MInst::JumpTable { .. }))
    );
}

#[test]
fn recognizes_full_domain_dense_lookup_with_global_constants_and_zero_extended_conditions() {
    let fixture = dense_lookup_fixture(2);
    let plans = lookup_plans(&fixture);
    assert_eq!(plans.roots.len(), 2);

    let first = &plans.roots[&fixture.roots[0].1];
    assert_eq!(first.selector, fixture.selector);
    assert_eq!(first.selector_width, 2);
    assert_eq!(first.entries, vec![10, 11, 12, 13]);
    assert_eq!(first.default, fixture.defaults[0]);
    for &(_, compare_idx, concat_idx) in &fixture.conditions {
        assert!(plans.skip_indices.contains(&compare_idx));
        assert!(plans.skip_indices.contains(&concat_idx));
    }
    for indices in &fixture.mux_indices {
        assert!(indices.iter().all(|idx| plans.skip_indices.contains(idx)));
    }
}

#[test]
fn rejects_duplicate_missing_masked_and_mixed_selector_keys() {
    let mut duplicate = dense_lookup_fixture(1);
    let duplicate_key_idx = duplicate.key_defs[3].1;
    duplicate
        .eu
        .blocks
        .get_mut(&SirBlockId(0))
        .unwrap()
        .instructions[duplicate_key_idx] =
        SIRInstruction::Imm(duplicate.key_defs[3].0, SIRValue::new(2u8));
    assert!(lookup_plans(&duplicate).roots.is_empty());

    let mut missing = dense_lookup_fixture(1);
    let missing_key_idx = missing.key_defs[3].1;
    missing
        .eu
        .blocks
        .get_mut(&SirBlockId(0))
        .unwrap()
        .instructions[missing_key_idx] =
        SIRInstruction::Imm(missing.key_defs[3].0, SIRValue::new(4u8));
    assert!(lookup_plans(&missing).roots.is_empty());

    let mut masked = dense_lookup_fixture(1);
    let masked_key_idx = masked.key_defs[2].1;
    masked
        .eu
        .blocks
        .get_mut(&SirBlockId(0))
        .unwrap()
        .instructions[masked_key_idx] =
        SIRInstruction::Imm(masked.key_defs[2].0, SIRValue::new_four_state(2u8, 1u8));
    assert!(lookup_plans(&masked).roots.is_empty());

    let mut mixed = dense_lookup_fixture(1);
    let compare_idx = mixed.conditions[0].1;
    let key = mixed.key_defs[2].0;
    let compare_dst =
        sir_def_reg(&mixed.eu.blocks[&mixed.block_id].instructions[compare_idx]).unwrap();
    mixed
        .eu
        .blocks
        .get_mut(&mixed.block_id)
        .unwrap()
        .instructions[compare_idx] = SIRInstruction::Binary(
        compare_dst,
        mixed.alternate_selector,
        BinaryOp::EqWildcard,
        key,
    );
    assert!(lookup_plans(&mixed).roots.is_empty());
}

#[test]
fn group_dce_retains_shared_condition_when_unrecognized_code_uses_it() {
    let mut fixture = dense_lookup_fixture(2);
    let (condition, compare_idx, concat_idx) = fixture.conditions[0];
    let outside = RegisterId(
        fixture
            .eu
            .register_map
            .keys()
            .map(|reg| reg.0)
            .max()
            .unwrap()
            + 1,
    );
    fixture.eu.register_map.insert(
        outside,
        RegisterType::Bit {
            width: 2,
            signed: false,
        },
    );
    fixture
        .eu
        .blocks
        .get_mut(&fixture.block_id)
        .unwrap()
        .instructions
        .push(SIRInstruction::Unary(outside, UnaryOp::Ident, condition));

    let plans = lookup_plans(&fixture);
    assert_eq!(plans.roots.len(), 2);
    assert!(!plans.skip_indices.contains(&concat_idx));
    assert!(!plans.skip_indices.contains(&compare_idx));
}

#[test]
fn group_dce_retains_old_mux_and_its_inputs_for_an_outside_use() {
    let mut fixture = dense_lookup_fixture(1);
    let old_mux_idx = fixture.mux_indices[0][0];
    let old_mux = match fixture.eu.blocks[&fixture.block_id].instructions[old_mux_idx] {
        SIRInstruction::Mux(dst, ..) => dst,
        _ => unreachable!(),
    };
    let outside = RegisterId(
        fixture
            .eu
            .register_map
            .keys()
            .map(|reg| reg.0)
            .max()
            .unwrap()
            + 1,
    );
    fixture.eu.register_map.insert(
        outside,
        RegisterType::Bit {
            width: 8,
            signed: false,
        },
    );
    fixture
        .eu
        .blocks
        .get_mut(&fixture.block_id)
        .unwrap()
        .instructions
        .push(SIRInstruction::Unary(outside, UnaryOp::Ident, old_mux));

    let plans = lookup_plans(&fixture);
    assert_eq!(plans.roots.len(), 1);
    assert!(!plans.skip_indices.contains(&old_mux_idx));
    assert!(!plans.skip_indices.contains(&fixture.conditions[0].2));
}

#[test]
fn lowers_shared_selector_roots_to_cached_indexed_table_loads() {
    let mut fixture = dense_lookup_fixture(2);
    // Make the executable fixture verifier-valid; truncation of the
    // deliberately malformed payload is covered by the recognizer test.
    for inst in &mut fixture
        .eu
        .blocks
        .get_mut(&SirBlockId(0))
        .unwrap()
        .instructions
    {
        if let SIRInstruction::Imm(_, value) = inst
            && value.payload == BigUint::from(0x10du16)
        {
            value.payload = BigUint::from(13u8);
        }
    }

    let input_var = VarId::default();
    let mut first_output_var = input_var;
    first_output_var.0 += 1;
    let mut second_output_var = first_output_var;
    second_output_var.0 += 1;
    let input_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: input_var,
    };
    let first_output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: first_output_var,
    };
    let second_output_abs = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: second_output_var,
    };
    let input_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
    let first_output_addr =
        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, first_output_abs);
    let second_output_addr =
        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, second_output_abs);

    let mut instructions = vec![SIRInstruction::Load(
        fixture.selector,
        input_addr,
        SIROffset::Static(0),
        2,
    )];
    instructions.extend(std::mem::take(
        &mut fixture
            .eu
            .blocks
            .get_mut(&SirBlockId(0))
            .unwrap()
            .instructions,
    ));
    instructions.extend(std::mem::take(
        &mut fixture
            .eu
            .blocks
            .get_mut(&fixture.block_id)
            .unwrap()
            .instructions,
    ));
    instructions.push(SIRInstruction::Store(
        first_output_addr,
        SIROffset::Static(0),
        8,
        fixture.roots[0].0,
        vec![],
        vec![],
    ));
    instructions.push(SIRInstruction::Store(
        second_output_addr,
        SIROffset::Static(0),
        8,
        fixture.roots[1].0,
        vec![],
        vec![],
    ));
    fixture.eu.blocks = [(
        SirBlockId(0),
        BasicBlock {
            id: SirBlockId(0),
            params: vec![],
            instructions,
            terminator: SIRTerminator::Return,
        },
    )]
    .into_iter()
    .collect();
    fixture.eu.entry_block_id = SirBlockId(0);
    fixture.eu.verify();

    let layout = MemoryLayout {
        trace: None,
        four_state: false,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets: [
            (input_abs, 0),
            (first_output_abs, 8),
            (second_output_abs, 16),
        ]
        .into_iter()
        .collect(),
        widths: [
            (input_abs, 2),
            (first_output_abs, 8),
            (second_output_abs, 8),
        ]
        .into_iter()
        .collect(),
        is_4states: [
            (input_abs, false),
            (first_output_abs, false),
            (second_output_abs, false),
        ]
        .into_iter()
        .collect(),
        total_size: 24,
        working_offsets: HashMap::default(),
        working_base_offset: 24,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: 24,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: 24,
        sparse_active_capacity: 0,
        merged_total_size: 24,
        triggered_bits_offset: 24,
        triggered_bits_total_size: 0,
        scratch_base_offset: 24,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: vec![],
    };

    let mut function = lower_execution_unit(&fixture.eu, &layout, false);
    function.verify();
    assert_eq!(function.constant_tables().len(), 2);
    assert!(
        function
            .constant_tables()
            .iter()
            .any(|table| table == &[10, 11, 12, 13])
    );
    assert!(
        function
            .constant_tables()
            .iter()
            .any(|table| table == &[26, 27, 28, 29])
    );
    let insts = function.blocks.iter().flat_map(|block| &block.insts);
    let (mut masks, mut scales, mut addresses, mut loads, mut comparisons) = (0, 0, 0, 0, 0);
    for inst in insts {
        match inst {
            MInst::AndImm { imm: 3, .. } => masks += 1,
            MInst::ShlImm { imm: 3, .. } => scales += 1,
            MInst::LoadConstantTableAddr { .. } => addresses += 1,
            MInst::LoadPtrIndexed {
                size: OpSize::S64, ..
            } => loads += 1,
            MInst::Cmp { .. } | MInst::CmpImm { .. } | MInst::Select { .. } => comparisons += 1,
            _ => {}
        }
    }
    assert_eq!(
        (masks, scales, addresses, loads, comparisons),
        (1, 1, 2, 2, 0)
    );

    mir_legalize::legalize(&mut function);
    function.verify();
    mir_opt::optimize(&mut function);
    function.verify();
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
    function.verify();
    let emitted = emit::emit(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
    )
    .unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    for selector in 0u8..4 {
        let mut state = vec![0u8; 24];
        state[0] = selector;
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(state[8], 10 + selector);
        assert_eq!(state[16], 26 + selector);
    }
}

struct LookupFixture {
    eu: ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: SirBlockId,
    roots: Vec<(RegisterId, usize)>,
    selector: RegisterId,
    alternate_selector: RegisterId,
    defaults: Vec<RegisterId>,
    key_defs: Vec<(RegisterId, usize)>,
    conditions: Vec<(RegisterId, usize, usize)>,
    mux_indices: Vec<Vec<usize>>,
}

struct LookupFixtureBuilder {
    next_reg: usize,
    register_map: HashMap<RegisterId, RegisterType>,
    constants: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
}

impl LookupFixtureBuilder {
    fn new() -> Self {
        Self {
            next_reg: 0,
            register_map: HashMap::default(),
            constants: Vec::new(),
            instructions: Vec::new(),
        }
    }

    fn register(&mut self, width: usize) -> RegisterId {
        let reg = RegisterId(self.next_reg);
        self.next_reg += 1;
        self.register_map.insert(
            reg,
            RegisterType::Bit {
                width,
                signed: false,
            },
        );
        reg
    }

    fn constant(&mut self, width: usize, value: u64, mask: u64) -> (RegisterId, usize) {
        let reg = self.register(width);
        let idx = self.constants.len();
        self.constants.push(SIRInstruction::Imm(
            reg,
            SIRValue::new_four_state(value, mask),
        ));
        (reg, idx)
    }

    fn instruction(
        &mut self,
        width: usize,
        make: impl FnOnce(RegisterId) -> SIRInstruction<RegionedAbsoluteAddr>,
    ) -> (RegisterId, usize) {
        let reg = self.register(width);
        let idx = self.instructions.len();
        self.instructions.push(make(reg));
        (reg, idx)
    }
}

fn dense_lookup_fixture(root_count: usize) -> LookupFixture {
    let mut builder = LookupFixtureBuilder::new();
    let selector = builder.register(2);
    let alternate_selector = builder.register(2);
    let (zero, _) = builder.constant(1, 0, 0);
    let mut key_defs = Vec::new();
    for key in 0..4 {
        key_defs.push(builder.constant(2, key, 0));
    }
    let mut defaults = Vec::new();
    let mut value_regs = Vec::new();
    for root in 0..root_count {
        defaults.push(builder.constant(8, 0xe0 + root as u64, 0).0);
        let mut values = Vec::new();
        for key in 0..4 {
            // Key 3 deliberately carries a payload bit outside the
            // logical result width.  Table construction must truncate it
            // exactly like ordinary SIR immediate lowering.
            let value = if root == 0 && key == 3 {
                0x100 + 13
            } else {
                10 + root as u64 * 16 + key as u64
            };
            values.push(builder.constant(8, value, 0).0);
        }
        value_regs.push(values);
    }

    // Use a non-sorted stage order so the test observes that key/value
    // association, rather than mux position, defines the table entry.
    let stage_keys = [2usize, 0, 3, 1];
    let mut conditions = Vec::new();
    for (stage, &key) in stage_keys.iter().enumerate() {
        let key_reg = key_defs[key].0;
        let (compare, compare_idx) = if stage % 2 == 0 {
            builder.instruction(1, |dst| {
                SIRInstruction::Binary(dst, selector, BinaryOp::EqWildcard, key_reg)
            })
        } else {
            // Exact equality is symmetric; exercise a constant LHS too.
            builder.instruction(1, |dst| {
                SIRInstruction::Binary(dst, key_reg, BinaryOp::Eq, selector)
            })
        };
        let (condition, concat_idx) =
            builder.instruction(2, |dst| SIRInstruction::Concat(dst, vec![zero, compare]));
        conditions.push((condition, compare_idx, concat_idx));
    }

    let mut roots = Vec::new();
    let mut mux_indices = Vec::new();
    for root in 0..root_count {
        let mut previous = defaults[root];
        let mut indices = Vec::new();
        for (stage, &key) in stage_keys.iter().enumerate() {
            let condition = conditions[stage].0;
            let then_value = value_regs[root][key];
            let (next, idx) = builder.instruction(8, |dst| {
                SIRInstruction::Mux(dst, condition, then_value, previous)
            });
            previous = next;
            indices.push(idx);
        }
        roots.push((previous, *indices.last().unwrap()));
        mux_indices.push(indices);
    }

    let constants_block = BasicBlock {
        id: SirBlockId(0),
        params: vec![],
        instructions: builder.constants,
        terminator: SIRTerminator::Jump(SirBlockId(1), vec![]),
    };
    let lookup_block = BasicBlock {
        id: SirBlockId(1),
        params: vec![],
        instructions: builder.instructions,
        terminator: SIRTerminator::Return,
    };
    LookupFixture {
        eu: ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [
                (SirBlockId(0), constants_block),
                (SirBlockId(1), lookup_block),
            ]
            .into_iter()
            .collect(),
            register_map: builder.register_map,
        },
        block_id: SirBlockId(1),
        roots,
        selector,
        alternate_selector,
        defaults,
        key_defs,
        conditions,
        mux_indices,
    }
}

fn lookup_plans(fixture: &LookupFixture) -> DenseLookupPlans {
    let constants = collect_exact_sir_constants(&fixture.eu);
    let uses = collect_sir_use_sites(&fixture.eu);
    find_dense_lookup_plans(
        &fixture.eu.blocks[&fixture.block_id],
        &fixture.eu.register_map,
        &constants,
        &uses,
    )
}

fn dense_branch_table_fixture() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let selector = RegisterId(0);
    let bit_type = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let mut register_map = [(selector, bit_type(2))]
        .into_iter()
        .collect::<HashMap<_, _>>();
    let mut blocks = HashMap::default();
    let mut next_register = 1usize;

    for key in 0..4usize {
        let key_register = RegisterId(next_register);
        let comparison = RegisterId(next_register + 1);
        let reduced = RegisterId(next_register + 2);
        let condition = RegisterId(next_register + 3);
        next_register += 4;
        register_map.insert(key_register, bit_type(2));
        register_map.insert(comparison, bit_type(1));
        register_map.insert(reduced, bit_type(1));
        register_map.insert(condition, bit_type(1));
        let false_target = if key + 1 < 4 {
            SirBlockId(key + 1)
        } else {
            SirBlockId(7)
        };
        blocks.insert(
            SirBlockId(key),
            BasicBlock {
                id: SirBlockId(key),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(key_register, SIRValue::new(BigUint::from(key as u64))),
                    SIRInstruction::Binary(
                        comparison,
                        selector,
                        BinaryOp::EqWildcard,
                        key_register,
                    ),
                    SIRInstruction::Unary(reduced, UnaryOp::Or, comparison),
                    SIRInstruction::Unary(condition, UnaryOp::ToTwoState, reduced),
                ],
                terminator: SIRTerminator::Branch {
                    cond: condition,
                    true_block: (SirBlockId(4 + key), vec![]),
                    false_block: (false_target, vec![]),
                },
            },
        );
    }
    for block in 4..8 {
        blocks.insert(
            SirBlockId(block),
            BasicBlock {
                id: SirBlockId(block),
                params: vec![],
                instructions: vec![],
                terminator: SIRTerminator::Return,
            },
        );
    }
    ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks,
        register_map,
    }
}

#[test]
fn rejects_width_default_and_direction_mismatches() {
    let mut default_width = dense_lookup_fixture(1);
    default_width.eu.register_map.insert(
        default_width.defaults[0],
        RegisterType::Bit {
            width: 7,
            signed: false,
        },
    );
    assert!(lookup_plans(&default_width).roots.is_empty());

    let mut wide_selector = dense_lookup_fixture(1);
    wide_selector.eu.register_map.insert(
        wide_selector.selector,
        RegisterType::Bit {
            width: usize::BITS as usize,
            signed: false,
        },
    );
    for &(key, _) in &wide_selector.key_defs {
        wide_selector.eu.register_map.insert(
            key,
            RegisterType::Bit {
                width: usize::BITS as usize,
                signed: false,
            },
        );
    }
    assert!(lookup_plans(&wide_selector).roots.is_empty());

    let mut reversed_wildcard = dense_lookup_fixture(1);
    let compare_idx = reversed_wildcard.conditions[0].1;
    let compare = &mut reversed_wildcard
        .eu
        .blocks
        .get_mut(&reversed_wildcard.block_id)
        .unwrap()
        .instructions[compare_idx];
    let (dst, selector, key) = match compare {
        SIRInstruction::Binary(dst, selector, BinaryOp::EqWildcard, key) => (*dst, *selector, *key),
        _ => unreachable!(),
    };
    *compare = SIRInstruction::Binary(dst, key, BinaryOp::EqWildcard, selector);
    assert!(lookup_plans(&reversed_wildcard).roots.is_empty());

    let mut wide_result = dense_lookup_fixture(1);
    wide_result.eu.register_map.insert(
        wide_result.roots[0].0,
        RegisterType::Bit {
            width: 65,
            signed: false,
        },
    );
    assert!(lookup_plans(&wide_result).roots.is_empty());
}
