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
