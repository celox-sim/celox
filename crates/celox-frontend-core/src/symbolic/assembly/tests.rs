use super::*;

#[test]
fn clock_cones_include_procedural_reads_and_reject_opaque_dependencies() {
    let mut arena = SLTNodeArena::new();
    let zero = arena
        .alloc(celox_slt::SLTNode::Constant(
            0u8.into(),
            0u8.into(),
            1,
            false,
        ))
        .unwrap();
    let input = arena
        .alloc(celox_slt::SLTNode::Input {
            variable: addr(2),
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let mut builder = celox_sir::SIRBuilder::new();
    let auxiliary = arena
        .alloc(celox_slt::SLTNode::Input {
            variable: addr(4),
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let value = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Load(
        value,
        addr(1),
        SIROffset::Static(0),
        1,
    ));
    let unit = builder.flush_eu().unwrap();
    let mut path = LogicPath {
        target: LogicPathTarget::Process {
            process: addr(0),
            after_initials: false,
            sensitivity_counters: Vec::new(),
            written_excluded_inputs: Vec::new(),
            startup: None,
            target: VarAtomBase::new(addr(0), 0, 0),
            unit: Box::new(unit),
        },
        transparent_copy: false,
        sources: HashSet::default(),
        previous_sources: [VarAtomBase::new(addr(3), 0, 0)].into_iter().collect(),
        address_sources: HashSet::default(),
        local_inputs: Vec::new(),
        order_before: HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: vec![auxiliary],
        expr: zero,
    };
    let mut upstream = path.clone();
    upstream.target = LogicPathTarget::Var(VarAtomBase::new(addr(1), 0, 0));
    upstream.expr = input;
    upstream.previous_sources.clear();
    upstream.pre_lower_nodes.clear();
    let clocks = [addr(0)].into_iter().collect();
    assert_eq!(
        clock_cone_inputs(&[path.clone(), upstream.clone()], &arena, &clocks)[&addr(0)],
        vec![addr(0), addr(1), addr(2), addr(3), addr(4)]
    );
    let LogicPathTarget::Process { unit, .. } = &mut path.target else {
        unreachable!()
    };
    unit.blocks
        .values_mut()
        .next()
        .unwrap()
        .instructions
        .push(SIRInstruction::ExternCall {
            dst: None,
            func: 0,
            args: Vec::new(),
        });
    assert!(clock_cone_inputs(&[path, upstream], &arena, &clocks).is_empty());
}

/// A call of a function with these effects.
fn call<A>(reads: Vec<A>, writes: Vec<A>, extern_calls: bool) -> SIRInstruction<A> {
    SIRInstruction::Call {
        function: celox_sir::FunctionId(0),
        frame: celox_sir::RegisterId(0),
        args: Vec::new(),
        outputs: Vec::new(),
        dst: None,
        effects: std::sync::Arc::new(celox_sir::CallEffects {
            reads,
            writes,
            triggers: Vec::new(),
            host: celox_sir::HostEffects {
                extern_calls,
                ..Default::default()
            },
            kernel: false,
            may_fail: false,
        }),
    }
}

fn unit_of<A>(instructions: Vec<SIRInstruction<A>>) -> ExecutionUnit<A> {
    ExecutionUnit {
        entry_block_id: celox_sir::BlockId(0),
        blocks: [(
            celox_sir::BlockId(0),
            BasicBlock {
                id: celox_sir::BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: HashMap::default(),
    }
}

/// A clock cone reads what a procedural path's calls read, and an
/// external call in a callee makes it opaque as one in the path does.
#[test]
fn clock_cones_include_what_calls_read() {
    let mut arena = SLTNodeArena::new();
    let zero = arena
        .alloc(celox_slt::SLTNode::Constant(
            0u8.into(),
            0u8.into(),
            1,
            false,
        ))
        .unwrap();
    let path = |unit| LogicPath {
        target: LogicPathTarget::Process {
            process: addr(0),
            after_initials: false,
            sensitivity_counters: Vec::new(),
            written_excluded_inputs: Vec::new(),
            startup: None,
            target: VarAtomBase::new(addr(0), 0, 0),
            unit: Box::new(unit),
        },
        transparent_copy: false,
        sources: HashSet::default(),
        previous_sources: HashSet::default(),
        address_sources: HashSet::default(),
        local_inputs: Vec::new(),
        order_before: HashSet::default(),
        comb_capture_enable_sites: Vec::new(),
        comb_capture_enable_always: false,
        pre_lower_nodes: Vec::new(),
        expr: zero,
    };
    let clocks = [addr(0)].into_iter().collect();
    let reading = path(unit_of(vec![call(vec![addr(5)], vec![addr(6)], false)]));
    assert_eq!(
        clock_cone_inputs(&[reading], &arena, &clocks)[&addr(0)],
        vec![addr(0), addr(5)]
    );
    let opaque = path(unit_of(vec![call(Vec::new(), Vec::new(), true)]));
    assert!(clock_cone_inputs(&[opaque], &arena, &clocks).is_empty());
}

/// The loads and the written bits of a unit count a call's reads and
/// writes, each of a whole object.
#[test]
fn unit_loads_and_written_bits_include_calls() {
    let stable = |var| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr(var));
    let unit = unit_of(vec![
        call(vec![stable(1)], vec![stable(2)], false),
        SIRInstruction::Load(celox_sir::RegisterId(1), stable(3), SIROffset::Static(0), 1),
    ]);
    let mut loads = HashSet::default();
    unit_loads(&unit, &mut loads);
    assert_eq!(loads, [addr(1), addr(3)].into_iter().collect());
    let instructions = &unit.blocks[&celox_sir::BlockId(0)].instructions;
    assert_eq!(
        written_bits(&instructions[0]).collect::<Vec<_>>(),
        [(&stable(2), None)]
    );
    let store = SIRInstruction::Store(
        stable(4),
        SIROffset::Static(3),
        2,
        celox_sir::RegisterId(1),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(
        written_bits(&store).collect::<Vec<_>>(),
        [(&stable(4), Some(BitAccess::new(3, 4)))]
    );
    assert_eq!(written_bits(&instructions[1]).count(), 0);
}

fn addr(var: u32) -> AbsoluteAddr {
    AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: SourceVarId(var),
    }
}

/// The previous propagation, which scans every path for each probe.
fn add_copied_probes_by_scan(
    probes: &mut Vec<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>,
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
    same_kind: impl Fn(&AbsoluteAddr, &AbsoluteAddr) -> bool,
) {
    let mut next_probe = 0;
    while next_probe < probes.len() {
        let (source, counter) = probes[next_probe];
        next_probe += 1;
        for path in comb_blocks {
            let LogicPathTarget::Var(target) = &path.target else {
                continue;
            };
            if !path.transparent_copy
                || target.id != source.id
                || !target.access.overlaps(&source.access)
            {
                continue;
            }
            let celox_slt::SLTNode::Input {
                variable,
                access,
                index,
                ..
            } = arena.get(path.expr)
            else {
                continue;
            };
            if !index.is_empty()
                || !same_kind(variable, &target.id)
                || access.msb - access.lsb != target.access.msb - target.access.lsb
            {
                continue;
            }
            let lsb = source.access.lsb.max(target.access.lsb);
            let msb = source.access.msb.min(target.access.msb);
            let upstream = VarAtomBase::new(
                *variable,
                access.lsb + lsb - target.access.lsb,
                access.lsb + msb - target.access.lsb,
            );
            if !probes.contains(&(upstream, counter)) {
                probes.push((upstream, counter));
            }
        }
    }
}

/// Indexing the transparent copies by target finds the probes the scan
/// of every path finds, in the same order.
#[test]
fn copied_probes_match_path_scan() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let mut copied = 0;
    for _ in 0..50 {
        let mut arena = SLTNodeArena::new();
        let mut comb_blocks = Vec::new();
        for _ in 0..60 {
            let target_lsb = next(12);
            let target_width = 1 + next(4);
            let source_lsb = next(12);
            // Mostly equal widths, so that most copies are transparent.
            let source_width = if next(5) == 0 {
                1 + next(4)
            } else {
                target_width
            };
            let expr = if next(8) == 0 {
                arena
                    .alloc(celox_slt::SLTNode::Constant(
                        BigUint::from(0u32),
                        BigUint::from(0u32),
                        source_width,
                        false,
                    ))
                    .unwrap()
            } else {
                arena
                    .alloc(celox_slt::SLTNode::Input {
                        variable: addr(next(12) as u32),
                        signed: false,
                        index: Vec::new(),
                        access: BitAccess::new(source_lsb, source_lsb + source_width - 1),
                    })
                    .unwrap()
            };
            comb_blocks.push(LogicPath {
                target: LogicPathTarget::Var(VarAtomBase::new(
                    addr(next(12) as u32),
                    target_lsb,
                    target_lsb + target_width - 1,
                )),
                transparent_copy: next(6) != 0,
                sources: HashSet::default(),
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: Vec::new(),
                order_before: HashSet::default(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
                expr,
            });
        }
        let mut probes = Vec::new();
        for _ in 0..4 {
            let lsb = next(14);
            let probe = (
                VarAtomBase::new(addr(next(12) as u32), lsb, lsb + next(3)),
                addr(100 + next(3) as u32),
            );
            if !probes.contains(&probe) {
                probes.push(probe);
            }
        }
        // Variables 0..6 are four-state and the others two-state.
        let same_kind = |a: &AbsoluteAddr, b: &AbsoluteAddr| (a.var_id.0 < 6) == (b.var_id.0 < 6);
        let mut expected = probes.clone();
        add_copied_probes_by_scan(&mut expected, &comb_blocks, &arena, same_kind);
        let mut probed = probes.iter().copied().collect();
        add_copied_probes(&mut probes, &mut probed, &comb_blocks, &arena, same_kind);
        assert_eq!(probes, expected);
        assert_eq!(probed.len(), probes.len());
        copied += probes.len();
    }
    // The cases find copies, beyond the 4 initial probes each.
    assert!(copied > 50 * 4 + 50, "{copied} probes");
}

/// Following each reached part to the paths that read its variable reaches
/// the parts the previous fixpoint, which scanned every path against every
/// reached part until nothing changed, reaches.
#[test]
fn affected_by_writes_matches_the_path_scan_fixpoint() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let atom = |next: &mut dyn FnMut(usize) -> usize| {
        let lsb = next(12);
        VarAtomBase::new(addr(next(8) as u32), lsb, lsb + next(4))
    };
    let mut reached_some = false;
    for _ in 0..200 {
        let mut arena = SLTNodeArena::<AbsoluteAddr>::new();
        let zero = arena
            .alloc(celox_slt::SLTNode::Constant(
                BigUint::from(0u32),
                BigUint::from(0u32),
                1,
                false,
            ))
            .unwrap();
        let paths: Vec<LogicPath<AbsoluteAddr>> = (0..30)
            .map(|_| LogicPath {
                target: LogicPathTarget::Var(atom(&mut next)),
                transparent_copy: false,
                sources: (0..1 + next(3)).map(|_| atom(&mut next)).collect(),
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: Vec::new(),
                order_before: HashSet::default(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
                expr: zero,
            })
            .collect();
        let writes: Vec<_> = (0..1 + next(3)).map(|_| atom(&mut next)).collect();
        // The previous fixpoint over every path.
        let overlaps_any = |atom: &VarAtomBase<AbsoluteAddr>, set: &HashSet<_>| {
            set.iter().any(|other: &VarAtomBase<AbsoluteAddr>| {
                atom.id == other.id && atom.access.overlaps(&other.access)
            })
        };
        let mut expected: HashSet<VarAtomBase<AbsoluteAddr>> = writes.iter().copied().collect();
        let mut changed = true;
        while changed {
            changed = false;
            for path in &paths {
                let target = *path.target.var().unwrap();
                if path
                    .sources
                    .iter()
                    .any(|source| overlaps_any(source, &expected))
                    && expected.insert(target)
                {
                    changed = true;
                }
            }
        }
        reached_some |= expected.len() > writes.len();
        let affected = affected_by_writes(&Readers::new(&paths), &writes);
        for var in 0..8 {
            for lsb in 0..16 {
                for msb in lsb..16 {
                    let probe = VarAtomBase::new(addr(var), lsb, msb);
                    assert_eq!(
                        affected.overlaps(&probe),
                        overlaps_any(&probe, &expected),
                        "{probe:?}"
                    );
                }
            }
        }
    }
    assert!(reached_some, "the paths must carry writes onward");
}

/// Relocation renumbers the extern functions a module's units call into the
/// design's table and leaves the heap helper alone, under either of its
/// `func`s: it is in no module's table.
#[test]
fn relocation_keeps_heap_helper_calls() {
    use celox_sir::extern_abi::{COVERAGE_HELPER_ID, HEAP_HELPER_ID};
    let extern_call = |func| SIRInstruction::<u32>::ExternCall {
        dst: None,
        func,
        args: Vec::new(),
    };
    let unit = unit_of(vec![
        extern_call(0),
        extern_call(HEAP_HELPER_ID),
        extern_call(COVERAGE_HELPER_ID),
    ]);
    let relocated = relocate_executation_unit_with_errors(
        &unit,
        &|address: &u32| *address,
        &HashMap::default(),
        &HashMap::default(),
        &[(0, 5)].into_iter().collect(),
        super::relocation::FunctionIds {
            base: 0,
            local: 0,
            imported: &[],
        },
    );
    let functions: Vec<u32> = relocated.blocks[&celox_sir::BlockId(0)]
        .instructions
        .iter()
        .map(|instruction| match instruction {
            SIRInstruction::ExternCall { func, .. } => *func,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(functions, [5, HEAP_HELPER_ID, COVERAGE_HELPER_ID]);
}

#[test]
fn exclude_written_process_sensitivity_handles_full_partial_and_unguarded_cases() {
    use celox_sir::{BinaryOp, RegisterId, RegisterType};

    let make_guard_unit = || {
        let mut register_map = HashMap::default();
        for (id, width) in [(0, 8), (1, 8), (2, 1), (3, 8), (4, 8), (5, 1)] {
            register_map.insert(
                RegisterId(id),
                RegisterType::Bit {
                    width,
                    signed: false,
                },
            );
        }
        ExecutionUnit {
            entry_block_id: celox_sir::BlockId(0),
            blocks: [(
                celox_sir::BlockId(0),
                BasicBlock {
                    id: celox_sir::BlockId(0),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Load(RegisterId(0), addr(1), SIROffset::Static(0), 8),
                        SIRInstruction::Load(RegisterId(1), addr(300), SIROffset::Static(0), 8),
                        SIRInstruction::Binary(
                            RegisterId(2),
                            RegisterId(0),
                            BinaryOp::NeCase,
                            RegisterId(1),
                        ),
                        SIRInstruction::Store(
                            addr(300),
                            SIROffset::Static(0),
                            8,
                            RegisterId(0),
                            Vec::new(),
                            Vec::new(),
                        ),
                        SIRInstruction::Load(RegisterId(3), addr(2), SIROffset::Static(0), 8),
                        SIRInstruction::Load(RegisterId(4), addr(301), SIROffset::Static(0), 8),
                        SIRInstruction::Binary(
                            RegisterId(5),
                            RegisterId(3),
                            BinaryOp::NeCase,
                            RegisterId(4),
                        ),
                        SIRInstruction::Store(
                            addr(301),
                            SIROffset::Static(0),
                            8,
                            RegisterId(3),
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        }
    };

    let make_path = |target_lsb, target_msb, exclude_guard: bool| {
        let excluded = if exclude_guard {
            vec![
                (VarAtomBase::new(addr(1), 0, 7), addr(300)),
                (VarAtomBase::new(addr(2), 0, 7), addr(301)),
            ]
        } else {
            Vec::new()
        };
        LogicPath {
            target: LogicPathTarget::Process {
                process: addr(100),
                after_initials: true,
                sensitivity_counters: vec![
                    (VarAtomBase::new(addr(1), 0, 7), addr(200)),
                    (VarAtomBase::new(addr(2), 0, 7), addr(201)),
                ],
                written_excluded_inputs: excluded,
                startup: None,
                target: VarAtomBase::new(addr(1), target_lsb, target_msb),
                unit: Box::new(make_guard_unit()),
            },
            transparent_copy: false,
            sources: [
                VarAtomBase::new(addr(1), 0, 7),
                VarAtomBase::new(addr(2), 0, 7),
            ]
            .into_iter()
            .collect(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr: NodeId(0),
        }
    };

    // Case A: full overlap drops source, counters, guard snapshot load/store, and dead source load.
    let mut full = vec![make_path(0, 7, true)];
    relocation::exclude_written_process_sensitivity(&mut full);
    assert_eq!(
        full[0].sources,
        [VarAtomBase::new(addr(2), 0, 7)].into_iter().collect()
    );
    let LogicPathTarget::Process {
        sensitivity_counters,
        written_excluded_inputs,
        unit,
        ..
    } = &full[0].target
    else {
        unreachable!()
    };
    assert_eq!(
        sensitivity_counters,
        &[(VarAtomBase::new(addr(2), 0, 7), addr(201))]
    );
    assert_eq!(
        written_excluded_inputs,
        &[(VarAtomBase::new(addr(2), 0, 7), addr(301))]
    );
    let insts = &unit.blocks[&celox_sir::BlockId(0)].instructions;
    assert!(!insts.iter().any(|inst| match inst {
        SIRInstruction::Load(_, a, ..) | SIRInstruction::Store(a, ..) => {
            *a == addr(1) || *a == addr(300)
        }
        _ => false,
    }));
    assert!(insts.contains(&SIRInstruction::Imm(
        RegisterId(2),
        celox_sir::SIRValue::new(0u8)
    )));

    // Case B: partial multi-gap overlap ([0..7] minus [2..5] -> [0..1] and [6..7]) slices guard check.
    let mut partial = vec![make_path(2, 5, true)];
    relocation::exclude_written_process_sensitivity(&mut partial);
    assert_eq!(
        partial[0].sources,
        [
            VarAtomBase::new(addr(1), 0, 1),
            VarAtomBase::new(addr(1), 6, 7),
            VarAtomBase::new(addr(2), 0, 7),
        ]
        .into_iter()
        .collect()
    );
    let LogicPathTarget::Process {
        sensitivity_counters,
        written_excluded_inputs,
        unit,
        ..
    } = &partial[0].target
    else {
        unreachable!()
    };
    assert_eq!(
        sensitivity_counters,
        &[
            (VarAtomBase::new(addr(1), 0, 1), addr(200)),
            (VarAtomBase::new(addr(1), 6, 7), addr(200)),
            (VarAtomBase::new(addr(2), 0, 7), addr(201)),
        ]
    );
    assert_eq!(
        written_excluded_inputs,
        &[
            (VarAtomBase::new(addr(1), 0, 1), addr(300)),
            (VarAtomBase::new(addr(1), 6, 7), addr(300)),
            (VarAtomBase::new(addr(2), 0, 7), addr(301)),
        ]
    );
    let insts = &unit.blocks[&celox_sir::BlockId(0)].instructions;
    let slice_ranges: Vec<_> = insts
        .iter()
        .filter_map(|inst| match inst {
            SIRInstruction::Slice(_, src, offset, width) => Some((*src, *offset, *width)),
            _ => None,
        })
        .collect();
    assert_eq!(
        slice_ranges,
        vec![
            (RegisterId(0), 0, 2),
            (RegisterId(1), 0, 2),
            (RegisterId(0), 6, 2),
            (RegisterId(1), 6, 2),
        ]
    );

    // Case C: always @* (`written_excluded_inputs` empty) strips sources but leaves counters and guard intact.
    let mut star = vec![make_path(0, 7, false)];
    relocation::exclude_written_process_sensitivity(&mut star);
    assert_eq!(
        star[0].sources,
        [VarAtomBase::new(addr(2), 0, 7)].into_iter().collect()
    );
    let LogicPathTarget::Process {
        sensitivity_counters,
        unit,
        ..
    } = &star[0].target
    else {
        unreachable!()
    };
    assert_eq!(sensitivity_counters.len(), 2);
    assert_eq!(
        unit.blocks[&celox_sir::BlockId(0)].instructions.len(),
        make_guard_unit().blocks[&celox_sir::BlockId(0)]
            .instructions
            .len()
    );
}

/// An instance's units name its module's own functions by their index from
/// its copies' base on, and the functions its module imports, numbered
/// after its own, by the design's numbers of their home copies.
#[test]
fn imported_functions_resolve_to_their_home_copies() {
    let ids = super::relocation::FunctionIds {
        base: 10,
        local: 2,
        imported: &[3, 7],
    };
    let global: Vec<u32> = (0..4).map(|id| ids.global(id)).collect();
    assert_eq!(global, [10, 11, 3, 7]);
}
