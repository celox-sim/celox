use super::*;
use celox_design::{InstanceId, StateObjectId};
use celox_sir::SIROffset;

fn address(id: u32) -> AbsoluteAddr {
    AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId(id),
    }
}

fn stable(id: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address(id))
}

/// A call with these effects, of no arguments and no result.
fn call(reads: Vec<u32>, writes: Vec<u32>) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Call {
        function: celox_sir::FunctionId(0),
        frame: RegisterId(0),
        args: Vec::new(),
        outputs: Vec::new(),
        dst: None,
        effects: std::sync::Arc::new(celox_sir::CallEffects {
            reads: reads.into_iter().map(stable).collect(),
            writes: writes.into_iter().map(stable).collect(),
            ..Default::default()
        }),
    }
}

/// A settle whose first unit calls a function that reads 0 and writes 1
/// and 2, and whose second stores 2 again from 3 and 3 into 4 and 5 from
/// 1.
fn calling_settle() -> [Unit; 2] {
    let unit = |instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>| {
        let mut b = Builder::new();
        let frame = b.alloc_bit(32, false);
        b.emit(SIRInstruction::Imm(frame, celox_sir::SIRValue::new(0u8)));
        for instruction in instructions {
            b.emit(instruction);
        }
        b.seal_block(SIRTerminator::Return);
        let (blocks, mut register_map, _) = b.drain();
        for register in 1..3 {
            register_map.insert(
                RegisterId(register),
                celox_sir::RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            );
        }
        Unit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    };
    let load = |register, id| {
        SIRInstruction::Load(RegisterId(register), stable(id), SIROffset::Static(0), 8)
    };
    let store = |id, register| {
        SIRInstruction::Store(
            stable(id),
            SIROffset::Static(0),
            8,
            RegisterId(register),
            Vec::new(),
            Vec::new(),
        )
    };
    [
        unit(vec![call(vec![0], vec![1, 2])]),
        unit(vec![
            load(1, 3),
            store(2, 1),
            store(4, 1),
            load(2, 1),
            store(5, 2),
        ]),
    ]
}

/// The slicer sees what a call reads and writes through its effects:
/// a slice of what it writes keeps it, with what it reads, and with the
/// settle's other writes of what else it writes; a slice of storage it
/// leaves alone does not.
#[test]
fn slices_keep_calls_by_their_effects() {
    let units = calling_settle();
    let slicer = CombSlicer::new(&units, HashMap::default());
    assert!(slicer.stores(address(1)) && slicer.stores(address(2)));
    let calls = |units: &[Unit]| {
        units
            .iter()
            .flat_map(instructions)
            .filter(|inst| inst.call_effects().is_some())
            .count()
    };
    // Storage 5 is computed from 1, which only the call writes.
    let slice = slicer.slice([address(5)]);
    assert_eq!(calls(&slice), 1);
    // The call writes 2 too, which the second unit overwrites: the slice
    // keeps that store, so 2 ends as the settle leaves it.
    assert!(slice.iter().flat_map(instructions).any(|inst| matches!(
        inst,
        SIRInstruction::Store(target, ..) if *target == stable(2)
    )));
    assert_eq!(
        slicer.input_signals([address(5)]),
        HashSet::from_iter([address(0), address(1), address(3)])
    );
    // Storage 4 is computed from 3 alone.
    let slice = slicer.slice([address(4)]);
    assert_eq!(calls(&slice), 0);
    assert_eq!(
        slicer.input_signals([address(4)]),
        HashSet::from_iter([address(3)])
    );
    // A change of what the call reads changes all it writes.
    let affected = slicer.affected_storage([address(0)]);
    assert_eq!(
        affected,
        HashSet::from_iter([address(1), address(2), address(5)])
    );
    let (writes, inputs) = slicer.stateless_cone(address(4), None).unwrap();
    assert_eq!(writes, HashSet::from_iter([address(4)]));
    assert_eq!(inputs, HashSet::from_iter([address(3)]));
    // 5 is computed from 1, which the call need not write on every path:
    // its load may read what the last settle left.
    assert!(slicer.stateless_cone(address(5), None).is_none());
}

#[test]
fn partition_caches_do_not_make_a_constant_alias_stateful() {
    let mut b = Builder::new();
    for index in 0..2 {
        let flag = b.alloc_bit(8, false);
        let zero = b.alloc_bit(8, false);
        let cond = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Load(
            flag,
            stable(0),
            SIROffset::Static(index * 8),
            8,
        ));
        b.emit(SIRInstruction::Imm(zero, celox_sir::SIRValue::new(0u8)));
        b.emit(SIRInstruction::Binary(cond, flag, BinaryOp::Ne, zero));
        // The coarse guard of a partition group converts its full flag
        // to a byte before combining it with several dirty bytes.
        let cond = if index == 0 {
            let one = b.alloc_bit(8, false);
            let full = b.alloc_bit(8, false);
            let grouped = b.alloc_bit(1, false);
            b.emit(SIRInstruction::Imm(one, celox_sir::SIRValue::new(1u8)));
            b.emit(SIRInstruction::Mux(full, cond, one, zero));
            b.emit(SIRInstruction::Binary(grouped, full, BinaryOp::Ne, zero));
            grouped
        } else {
            cond
        };
        let execute = b.new_block();
        let done = b.new_block();
        b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (execute, Vec::new()),
            false_block: (done, Vec::new()),
        });
        b.switch_to_block(execute);
        let value = b.alloc_bit(1, false);
        b.emit(if index == 0 {
            SIRInstruction::Imm(value, celox_sir::SIRValue::new(1u8))
        } else {
            SIRInstruction::Load(value, stable(1), SIROffset::Static(0), 1)
        });
        b.emit(SIRInstruction::Store(
            stable(index as u32 + 1),
            SIROffset::Static(0),
            1,
            value,
            Vec::new(),
            Vec::new(),
        ));
        b.emit(SIRInstruction::Store(
            stable(0),
            SIROffset::Static(index * 8),
            8,
            zero,
            Vec::new(),
            Vec::new(),
        ));
        b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        b.switch_to_block(done);
    }
    b.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = b.drain();
    let units = [Unit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }];
    units[0].verify();
    let slicer = CombSlicer::new(&units, HashMap::default());
    assert!(slicer.stateless_cone(address(2), None).is_none());
    let (writes, inputs) = slicer.stateless_cone(address(2), Some(address(0))).unwrap();
    assert_eq!(writes, HashSet::from_iter([address(1), address(2)]));
    assert_eq!(inputs, HashSet::from_iter([address(0)]));
}

#[test]
fn stability_analysis_excludes_unrelated_error_control() {
    let mut b = Builder::new();
    let value = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Load(
        value,
        stable(0),
        SIROffset::Static(0),
        1,
    ));
    let cond = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Load(
        cond,
        stable(2),
        SIROffset::Static(0),
        1,
    ));
    let error = b.new_block();
    let done = b.new_block();
    b.seal_block(SIRTerminator::Branch {
        cond,
        true_block: (error, Vec::new()),
        false_block: (done, Vec::new()),
    });
    b.switch_to_block(error);
    b.seal_block(SIRTerminator::Error(1));
    b.switch_to_block(done);
    b.emit(SIRInstruction::Store(
        stable(1),
        SIROffset::Static(0),
        1,
        value,
        Vec::new(),
        Vec::new(),
    ));
    b.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = b.drain();
    let units = [Unit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }];
    let slicer = CombSlicer::new(&units, HashMap::default());
    let (writes, inputs) = slicer.stateless_cone(address(1), None).unwrap();
    assert_eq!(writes, HashSet::from_iter([address(1)]));
    assert_eq!(inputs, HashSet::from_iter([address(0)]));
    let executable = slicer.slice([address(1)]);
    assert!(executable.iter().any(fails));
    assert!(executable.iter().flat_map(loads).any(|a| a == address(2)));
    for unit in &executable {
        unit.verify();
    }

    // Control that decides a sink's write remains a real dependency.
    let units = [settle()];
    let slicer = CombSlicer::new(&units, HashMap::default());
    assert_eq!(
        slicer.stateless_cone(address(2), None).unwrap().1,
        HashSet::from_iter([address(0), address(1)])
    );
}

#[test]
fn eager_empty_sink_keeps_nonterminating_control() {
    let mut builder = Builder::new();
    let forever = builder.new_block();
    builder.seal_block(SIRTerminator::Jump(forever, Vec::new()));
    builder.switch_to_block(forever);
    builder.seal_block(SIRTerminator::Jump(forever, Vec::new()));
    let (blocks, register_map, _) = builder.drain();
    let units = [Unit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }];
    let slicer = CombSlicer::new(&units, HashMap::default());
    assert!(slicer.slice([]).is_empty());
    let eager = slicer.eager_slice([]);
    assert_eq!(eager.len(), 1);
    eager[0].verify();
    let cfg = celox_sir::cfg::SirCfg::analyze(&eager[0]).unwrap();
    assert!(
        !cfg.loops.is_empty(),
        "an activation cannot elide an infinite loop"
    );
}

/// `if (a) { x = c; y = c; }  z = c;`
fn settle() -> Unit {
    let mut b = Builder::new();
    let [a, c, x, y, z] = [0, 1, 2, 3, 4];
    let cond = b.alloc_bit(1, false);
    let value = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Load(
        cond,
        stable(a),
        SIROffset::Static(0),
        1,
    ));
    b.emit(SIRInstruction::Load(
        value,
        stable(c),
        SIROffset::Static(0),
        8,
    ));
    let then = b.new_block();
    let otherwise = b.new_block();
    let join = b.new_block();
    b.seal_block(SIRTerminator::Branch {
        cond,
        true_block: (then, Vec::new()),
        false_block: (otherwise, Vec::new()),
    });
    b.switch_to_block(then);
    for target in [x, y] {
        b.emit(SIRInstruction::Store(
            stable(target),
            SIROffset::Static(0),
            8,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }
    b.seal_block(SIRTerminator::Jump(join, Vec::new()));
    b.switch_to_block(otherwise);
    b.seal_block(SIRTerminator::Jump(join, Vec::new()));
    b.switch_to_block(join);
    let again = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Load(
        again,
        stable(c),
        SIROffset::Static(0),
        8,
    ));
    b.emit(SIRInstruction::Store(
        stable(z),
        SIROffset::Static(0),
        8,
        again,
        Vec::new(),
        Vec::new(),
    ));
    b.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = b.drain();
    Unit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

fn stored(units: &[Unit]) -> Vec<AbsoluteAddr> {
    let mut stored: Vec<_> = units
        .iter()
        .flat_map(instructions)
        .filter_map(|inst| inst.memory_write().map(|write| write.addr.absolute_addr()))
        .collect();
    stored.sort_unstable();
    stored
}

#[test]
fn affected_slice_reads_cached_unaffected_intermediates() {
    let mut b = Builder::new();
    let a = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Load(a, stable(0), SIROffset::Static(0), 8));
    b.emit(SIRInstruction::Store(
        stable(1),
        SIROffset::Static(0),
        8,
        a,
        vec![],
        vec![],
    ));
    let cached = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Load(
        cached,
        stable(1),
        SIROffset::Static(0),
        8,
    ));
    let clock = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Load(
        clock,
        stable(2),
        SIROffset::Static(0),
        8,
    ));
    let result = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Binary(result, clock, BinaryOp::And, cached));
    b.emit(SIRInstruction::Store(
        stable(3),
        SIROffset::Static(0),
        8,
        result,
        vec![],
        vec![],
    ));
    b.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = b.drain();
    let units = [Unit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }];
    let slicer = CombSlicer::new(&units, HashMap::default());
    let slice = slicer.affected_slice([address(2)]);
    assert_eq!(stored(&slice), [address(3)]);
    assert!(instructions(&slice[0]).any(|inst| matches!(inst, SIRInstruction::Load(_, target, ..) if target.absolute_addr() == address(1))));
    assert!(!instructions(&slice[0]).any(|inst| matches!(inst, SIRInstruction::Load(_, target, ..) if target.absolute_addr() == address(0))));
    slice[0].verify();
}

#[test]
fn affected_storage_tracks_a_branch_choosing_constant_block_arguments() {
    let mut b = Builder::new();
    let condition = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Load(
        condition,
        stable(0),
        SIROffset::Static(0),
        1,
    ));
    let zero = b.alloc_bit(8, false);
    let one = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
    b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
    let parameter = b.alloc_bit(8, false);
    let join = b.new_block_with(vec![parameter]);
    b.seal_block(SIRTerminator::Branch {
        cond: condition,
        true_block: (join, vec![one]),
        false_block: (join, vec![zero]),
    });
    b.switch_to_block(join);
    b.emit(SIRInstruction::Store(
        stable(1),
        SIROffset::Static(0),
        8,
        parameter,
        vec![],
        vec![],
    ));
    b.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = b.drain();
    let units = [Unit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }];
    let slicer = CombSlicer::new(&units, HashMap::default());
    assert_eq!(
        slicer.affected_storage([address(0)]),
        [address(1)].into_iter().collect()
    );
    let slice = slicer.affected_slice([address(0)]);
    slice[0].verify();
    assert_eq!(stored(&slice), [address(1)]);
}

#[test]
fn affected_storage_follows_nested_branch_and_switch_execution() {
    // All selectors and constants are defined in the entry block, so
    // only block execution (not a new register definition) carries the
    // outer branch's influence into the nested stores.
    for switches in 0..8 {
        let mut b = Builder::new();
        let conditions: Vec<_> = (0..3)
            .map(|input| {
                let register = b.alloc_bit(1, false);
                b.emit(SIRInstruction::Load(
                    register,
                    stable(input),
                    SIROffset::Static(0),
                    1,
                ));
                register
            })
            .collect();
        let zero = b.alloc_bit(1, false);
        let one = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        b.emit(SIRInstruction::Store(
            stable(3),
            SIROffset::Static(0),
            1,
            zero,
            vec![],
            vec![],
        ));
        let entry = BlockId(0);
        let nested = [b.new_block(), b.new_block(), b.new_block()];
        let join = b.new_block();
        for (level, &condition) in conditions.iter().enumerate() {
            if level > 0 {
                b.switch_to_block(nested[level - 1]);
            }
            let then = nested[level];
            b.seal_block(if switches & (1 << level) == 0 {
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (then, vec![]),
                    false_block: (join, vec![]),
                }
            } else {
                SIRTerminator::Switch {
                    selector: condition,
                    cases: vec![SIRSwitchCase {
                        value: 1u8.into(),
                        target: then,
                    }],
                    default: join,
                }
            });
        }
        b.switch_to_block(nested[2]);
        b.emit(SIRInstruction::Store(
            stable(3),
            SIROffset::Static(0),
            1,
            one,
            vec![],
            vec![],
        ));
        b.seal_block(SIRTerminator::Jump(join, vec![]));
        b.switch_to_block(join);
        // No input can affect this unconditional constant store.
        b.emit(SIRInstruction::Store(
            stable(4),
            SIROffset::Static(0),
            1,
            one,
            vec![],
            vec![],
        ));
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        let units = [Unit {
            entry_block_id: entry,
            blocks,
            register_map,
        }];
        units[0].verify();
        let slicer = CombSlicer::new(&units, HashMap::default());
        for input in 0..3 {
            assert_eq!(
                slicer.affected_storage([address(input)]),
                [address(3)].into_iter().collect(),
                "switches={switches} input={input}"
            );
            for slice in [
                slicer.affected_slice([address(input)]),
                slicer.slice([address(3)]),
            ] {
                slice[0].verify();
                assert!(stored(&slice).iter().all(|&target| target == address(3)));
                assert!(stored(&slice).contains(&address(3)));
                for &condition in &conditions {
                    assert!(
                        slice[0]
                            .blocks
                            .values()
                            .any(|block| selector(&block.terminator) == Some(condition)),
                        "lost nested selector: switches={switches} input={input} condition={condition:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn affected_storage_follows_control_without_tainting_the_join() {
    let units = [settle()];
    let slicer = CombSlicer::new(&units, HashMap::default());
    assert_eq!(
        slicer.affected_storage([address(0)]),
        [address(2), address(3)].into_iter().collect()
    );
    assert_eq!(
        slicer.affected_storage([address(1)]),
        [address(2), address(3), address(4)].into_iter().collect()
    );
    assert!(slicer.affected_storage([address(9)]).is_empty());
}

#[test]
fn slice_keeps_what_decides_a_store() {
    let units = [settle()];
    let slicer = CombSlicer::new(&units, HashMap::default());
    let slice = slicer.slice([address(2)]);
    assert_eq!(stored(&slice), [address(2)]);
    // The branch on `a` decides whether `x` is stored.
    assert!(matches!(
        slice[0].blocks[&BlockId(0)].terminator,
        SIRTerminator::Branch { .. }
    ));
}

#[test]
fn slice_folds_branches_it_does_not_need() {
    let units = [settle()];
    let slicer = CombSlicer::new(&units, HashMap::default());
    let slice = slicer.slice([address(4)]);
    assert_eq!(stored(&slice), [address(4)]);
    assert_eq!(slice[0].blocks.len(), 2, "the branch on `a` folds away");
    assert!(
        instructions(&slice[0]).all(|inst| inst
            .memory_read()
            .is_none_or(|read| read.addr.absolute_addr() == address(1))),
        "`a` is no longer read"
    );
}

/// `for i in 0..a { z = c; }`
fn looping_settle() -> Unit {
    let mut b = Builder::new();
    let [a, c, z] = [0, 1, 4];
    let bound = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Load(
        bound,
        stable(a),
        SIROffset::Static(0),
        8,
    ));
    let zero = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
    let index = b.alloc_bit(8, false);
    let header = b.new_block_with(vec![index]);
    let body = b.new_block();
    let exit = b.new_block();
    b.seal_block(SIRTerminator::Jump(header, vec![zero]));
    b.switch_to_block(header);
    let more = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Binary(more, index, BinaryOp::LtU, bound));
    b.seal_block(SIRTerminator::Branch {
        cond: more,
        true_block: (body, Vec::new()),
        false_block: (exit, Vec::new()),
    });
    b.switch_to_block(body);
    let value = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Load(
        value,
        stable(c),
        SIROffset::Static(0),
        8,
    ));
    b.emit(SIRInstruction::Store(
        stable(z),
        SIROffset::Static(0),
        8,
        value,
        Vec::new(),
        Vec::new(),
    ));
    let one = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
    let next = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Binary(next, index, BinaryOp::Add, one));
    b.seal_block(SIRTerminator::Jump(header, vec![next]));
    b.switch_to_block(exit);
    b.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = b.drain();
    Unit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

#[test]
fn input_signals_preserve_control_and_storage_aliases() {
    let units = [settle()];
    let slicer = CombSlicer::new(&units, [(address(5), address(2))].into_iter().collect());
    assert_eq!(
        slicer.input_signals([address(5)]),
        [address(0), address(1)].into_iter().collect(),
        "a conditional store reads its selector as well as its value"
    );
    assert_eq!(
        slicer.input_signals([address(4)]),
        [address(1)].into_iter().collect(),
        "a folded branch must not introduce a false scheduler dependency"
    );
    assert!(slicer.input_signals([address(9)]).is_empty());

    let units = [looping_settle()];
    let slicer = CombSlicer::new(&units, HashMap::default());
    assert_eq!(
        slicer.input_signals([address(4)]),
        [address(0), address(1)].into_iter().collect(),
        "a loop retains its bound and the carried block arguments"
    );
}

#[test]
fn input_signals_match_materialized_slices_for_sink_combinations() {
    for units in [
        vec![settle()],
        vec![looping_settle()],
        vec![settle(), looping_settle()],
    ] {
        let slicer = CombSlicer::new(&units, HashMap::default());
        for mask in 0..32 {
            let sinks: Vec<_> = (0..5)
                .filter(|bit| mask & (1 << bit) != 0)
                .map(address)
                .collect();
            let slice = slicer.slice(sinks.iter().copied());
            for unit in &slice {
                unit.verify();
            }
            assert_eq!(
                slicer.input_signals(sinks),
                slice.iter().flat_map(loads).collect(),
                "sink mask {mask:05b}, {} units",
                units.len()
            );
        }
    }
}

#[test]
fn slice_of_storage_no_unit_stores_is_empty() {
    let units = [looping_settle()];
    let slicer = CombSlicer::new(&units, HashMap::default());
    // `a` is an input of the settle: nothing computes it, not even the
    // loop it bounds.
    assert!(slicer.slice([address(0)]).is_empty());
    let slice = slicer.slice([address(0), address(4)]);
    assert_eq!(stored(&slice), [address(4)]);
}
