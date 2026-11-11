use super::*;

#[cfg(all(feature = "systemverilog", hotspot_jit))]
#[test]
fn hotspot_block_frames_preserve_values_across_native_and_interpreted_branches() {
    let mut source = String::from(
        "module Top(input logic [129:0] d, a, input logic sel, output logic [129:0] q); always_comb begin logic [129:0] v; v = d;\n",
    );
    for shift in 0..24 {
        source.push_str(&format!("v = (v ^ a) + (d << {shift});\n"));
    }
    source.push_str("if (sel) q = v; else q = (v | a) ^ d; end endmodule");
    let build = || {
        crate::Simulator::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("region.sv"))],
            "Top",
        )
        .four_state(true)
        .opt_level(crate::OptLevel::O0)
        .build_interpreter()
        .unwrap()
    };
    let mut mixed = build();
    let mut reference = build();
    let options = SimulatorOptions {
        four_state: true,
        ..Default::default()
    };
    mixed.backend.enable_hotspots(&options).unwrap();
    // Force the oversized-group path on this small fixture. The same
    // admission path is used when a real group exceeds the compile budget.
    let runtime = mixed.backend.hotspots.as_mut().unwrap();
    runtime.begin(EntryKey::Comb);
    let mut oversized = usize::MAX;
    runtime.collect(
        EntryKey::Comb,
        &mut Some(Vec::new()),
        &mixed.backend.eval_comb_units[0].unit,
        &mut oversized,
    );
    assert!(runtime.has_regions(EntryKey::Comb));
    let (d, a, sel, q) = (
        mixed.signal("d"),
        mixed.signal("a"),
        mixed.signal("sel"),
        mixed.signal("q"),
    );
    let (rd, ra, rs, rq) = (
        reference.signal("d"),
        reference.signal("a"),
        reference.signal("sel"),
        reference.signal("q"),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut iteration = 0u32;
    loop {
        let value = (BigUint::from(1u8) << 129usize) | BigUint::from(iteration + 1);
        let mask = if iteration.is_multiple_of(3) {
            BigUint::from(1u8) << 128usize
        } else {
            BigUint::default()
        };
        mixed
            .modify(|io| {
                io.set_four_state(d, value.clone(), mask.clone());
                io.set_four_state(a, (iteration + 7).into(), (iteration & 1).into());
                io.set(sel, (iteration & 1) as u8);
            })
            .unwrap();
        reference
            .modify(|io| {
                io.set_four_state(rd, value, mask);
                io.set_four_state(ra, (iteration + 7).into(), (iteration & 1).into());
                io.set(rs, (iteration & 1) as u8);
            })
            .unwrap();
        assert_eq!(
            mixed.get_four_state(q),
            reference.get_four_state(rq),
            "iteration={iteration}"
        );
        let stats = mixed.backend.hotspot_stats().unwrap();
        assert_eq!(stats.failed_entries, 0, "{stats:?}");
        if stats.compiled_block_calls >= 64 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no block publication: {stats:?}"
        );
        iteration += 1;
        std::thread::yield_now();
    }
}

#[cfg(all(feature = "systemverilog", hotspot_jit))]
#[test]
fn hotspot_split_groups_share_pending_writes_and_trigger_snapshots() {
    const SOURCE: &str = r#"`timescale 1ns/1ns
module Top;
  logic clk = 0, en = 0;
  logic gclk;
  logic [129:0] q = 0, g = 0;
  logic [7:0] seen = 0;
  logic [7:0] mem [0:1023];
  wire [7:0] m = mem[3];
  assign gclk = clk & en;
  always #5 clk = ~clk;
  always #37 en = ~en;
  always_ff @(posedge clk) begin
    q <= {2'bxz, q[127:0] + 128'd1};
    mem[q[5:0]] <= q[7:0];
  end
  always_ff @(posedge gclk) g <= {q[129:128], g[127:0] + q[127:0]};
  always @(posedge gclk) seen = seen + 8'd1;
endmodule
"#;
    let build = || {
        crate::Simulation::from_sv_sources(vec![(SOURCE, std::path::Path::new("mixed.sv"))], "Top")
            .four_state(true)
            .build_interpreter()
            .unwrap()
    };
    for (native_eval, native_apply) in [(true, false), (false, true), (true, true)] {
        let mut reference = build();
        let mut mixed = build();
        let options = SimulatorOptions {
            four_state: true,
            emit_triggers: true,
            ..Default::default()
        };
        let backend = &mut mixed.simulator.backend;
        backend.enable_hotspots(&options).unwrap();
        let mut runtime = backend.hotspots.take().unwrap();
        let mut jobs = Vec::new();
        if native_eval {
            for (addr, group) in &backend.eval_only_units {
                jobs.push((
                    EntryKey::EvalOnly(backend.eval_only_event_map[addr].id()),
                    group
                        .units
                        .iter()
                        .map(|prepared| prepared.unit.as_ref().clone())
                        .collect(),
                ));
            }
        }
        if native_apply {
            for (addr, group) in &backend.apply_units {
                jobs.push((
                    EntryKey::Apply(backend.apply_event_map[addr].id()),
                    group
                        .units
                        .iter()
                        .map(|prepared| prepared.unit.as_ref().clone())
                        .collect(),
                ));
            }
        }
        assert!(!jobs.is_empty());
        // Publish each selected group before driving the design. Poll
        // through an absent entry so setup executes no simulation effects.
        for (key, units) in jobs {
            let count = runtime.stats().compiled_entries;
            runtime.begin(key);
            runtime.finish(key, None, Some(units));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while runtime.stats().compiled_entries == count {
                assert!(
                    runtime
                        .try_run(EntryKey::Process(usize::MAX), backend.memory.as_mut_slice())
                        .is_none()
                );
                assert_eq!(runtime.stats().failed_entries, 0, "{key:?}");
                assert!(std::time::Instant::now() < deadline, "{key:?}");
                std::thread::yield_now();
            }
        }
        // Hold this exact mixed configuration throughout the comparison.
        runtime.cancel();
        backend.hotspots = Some(runtime);
        for time in (0..1000).step_by(3) {
            mixed.run_until(time).unwrap();
            reference.run_until(time).unwrap();
            for name in ["q", "g", "seen", "m"] {
                assert_eq!(
                    mixed.get_four_state(mixed.signal(name)),
                    reference.get_four_state(reference.signal(name)),
                    "time={time}, signal={name}, native_eval={native_eval}, native_apply={native_apply}"
                );
            }
        }
        assert!(
            mixed
                .simulator
                .backend
                .hotspot_stats()
                .unwrap()
                .compiled_calls
                > 0
        );
    }
}

#[test]
fn snapshots_the_triggers_of_called_functions() {
    let addr = |var| RegionedAbsoluteAddrBase {
        region: STABLE_REGION,
        instance_id: crate::ir::InstanceId(0),
        var_id: celox_design::StateObjectId(var),
    };
    let call = celox_sir::SIRInstruction::Call {
        function: FunctionId(0),
        frame: celox_sir::RegisterId(0),
        args: Vec::new(),
        outputs: Vec::new(),
        dst: None,
        effects: Arc::new(celox_sir::CallEffects {
            writes: vec![addr(3)],
            triggers: vec![addr(3)],
            ..Default::default()
        }),
    };
    let unit = ExecutionUnit {
        entry_block_id: celox_sir::BlockId(0),
        blocks: [(
            celox_sir::BlockId(0),
            celox_sir::BasicBlock {
                id: celox_sir::BlockId(0),
                params: Vec::new(),
                instructions: vec![call],
                terminator: celox_sir::SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: Default::default(),
    };
    assert_eq!(
        collect_trigger_addrs(&[unit]),
        [(addr(3).absolute_addr(), STABLE_REGION)]
    );
}

#[test]
fn frames_keep_slot_planes_apart_in_each_frame() {
    let addr = |var| RegionedAbsoluteAddrBase {
        region: FRAME_REGION,
        instance_id: crate::ir::InstanceId(0),
        var_id: celox_design::StateObjectId(var),
    };
    // A 4-state 12-bit slot at byte 1 of each 8-byte frame.
    let slot = FrameSlot {
        addr: addr(0),
        offset: 1,
        width: 12,
        four_state: true,
        handle: false,
        element_width: None,
    };
    let slots = [(slot.addr, slot.clone())].into_iter().collect();
    let mut stack = vec![0xffu8; 16];
    let mut frames = Frames {
        functions: &[],
        slots: &slots,
        stack: &mut stack,
        bases: vec![0],
    };
    let whole = SIROffset::Static(0);
    fn at(offset: &SIROffset) -> ResolvedAccess<'_> {
        ResolvedAccess {
            offset,
            dynamics: [None, None],
        }
    }
    let value = SIRValue::new_four_state(BigUint::from(0xabcu32), BigUint::from(0x00fu32));
    frames.store(&slot.addr, &at(&whole), 12, &value).unwrap();
    frames.bases.push(8);
    frames
        .store(
            &slot.addr,
            &at(&whole),
            12,
            &SIRValue::new(BigUint::from(1u8)),
        )
        .unwrap();
    // Bits 4..8 of the inner frame's slot.
    let upper = SIROffset::Static(4);
    assert_eq!(
        frames.load(&slot.addr, &at(&upper), 4).unwrap(),
        SIRValue::new_four_state(BigUint::zero(), BigUint::zero())
    );
    frames.bases.pop();
    assert_eq!(frames.load(&slot.addr, &at(&whole), 12).unwrap(), value);
    // An access past the slot fails instead of reaching its neighbours.
    let past = SIROffset::Static(8);
    assert!(frames.load(&slot.addr, &at(&past), 8).is_err());
    assert!(frames.store(&slot.addr, &at(&past), 8, &value).is_err());
    // The bytes around the slot's planes keep their contents.
    assert_eq!(stack[0], 0xff);
    assert_eq!(stack[5], 0xff);
    assert_eq!(stack[2] >> 4, 0xf);
}

#[test]
fn narrow_packed_access_matches_biguint_reference() {
    const WIDTHS: &[usize] = &[1, 7, 8, 31, 32, 63, 64];
    const VALUES: &[u64] = &[0, 1, 0x55, 0xdead_beef, u64::MAX];

    for bit_offset in 0..16 {
        for &bits in WIDTHS {
            let shift = bit_offset % 8;
            let byte_len = (shift + bits).div_ceil(8);
            let start = bit_offset / 8;
            for &value in VALUES {
                let original: Vec<u8> = (0..16)
                    .map(|index| (index as u8).wrapping_mul(37).wrapping_add(11))
                    .collect();
                let mut actual = original.clone();
                pack_u64(&mut actual[start..start + byte_len], shift, bits, value);

                let mut expected = BigUint::from_bytes_le(&original);
                let field_mask = low_mask(bits) << bit_offset;
                expected &= low_mask(original.len() * 8) ^ &field_mask;
                expected |= (BigUint::from(value) & low_mask(bits)) << bit_offset;
                let mut expected_bytes = expected.to_bytes_le();
                expected_bytes.resize(original.len(), 0);
                assert_eq!(actual, expected_bytes);

                let loaded = unpack_u64(&actual[start..start + byte_len], shift, bits);
                assert_eq!(loaded, value & narrow_mask(bits));
            }
        }
    }
}

#[test]
fn trigger_addrs_iterate_the_union_in_order() {
    let addr = |id: u32| AbsoluteAddr {
        instance_id: celox_design::InstanceId(0),
        var_id: celox_design::StateObjectId(id),
    };
    let lists: [&[u32]; 5] = [&[], &[1], &[1, 4, 9], &[2, 4, 7, 9, 11], &[0, 3, 11]];
    for first in lists {
        for second in lists {
            for region in [0, 1] {
                let entries = |ids: &[u32]| -> Vec<(AbsoluteAddr, u32)> {
                    ids.iter().map(|&id| (addr(id), region)).collect()
                };
                let (first, second) = (entries(first), entries(second));
                let mut expected: Vec<_> = first.iter().chain(&second).copied().collect();
                expected.sort_unstable();
                expected.dedup();
                let addrs = TriggerAddrs {
                    first: &first,
                    second: &second,
                };
                assert_eq!(addrs.iter().collect::<Vec<_>>(), expected);
            }
        }
    }
}

/// Clocks sharing one combinational program each run it before their
/// flip-flops in a combined tick, as the default compiled backend does.
#[test]
fn combined_ticks_of_several_clocks_match_the_compiled_backend() {
    const CODE: &str = r#"
module Top (
    clk_a: input 'a clock,
    clk_b: input 'b clock,
    clk_c: input 'c clock,
    din: input 'a logic<8>,
    qa: output 'a logic<8>,
    qb: output 'b logic<8>,
    qc: output 'c logic<8>,
) {
    var x: 'a logic<8>;
    assign x = din ^ qa;
    always_ff (clk_a) {
        qa = x + 1;
    }
    unsafe (cdc) {
        always_ff (clk_b) {
            qb = qa + x;
        }
        always_ff (clk_c) {
            qc = qb ^ x;
        }
    }
}
"#;
    fn run<B: crate::backend::SimBackend>(mut sim: crate::Simulator<B>) -> Vec<BigUint> {
        let clocks = [sim.event("clk_a"), sim.event("clk_b"), sim.event("clk_c")];
        let din = sim.signal("din");
        let outputs = [sim.signal("qa"), sim.signal("qb"), sim.signal("qc")];
        let mut seen = Vec::new();
        for step in 0..60u64 {
            let value = (step * 37 + 11) as u8;
            sim.modify(|io| io.set(din, value)).unwrap();
            sim.tick_deferred_comb(clocks[(step * 7 % 3) as usize])
                .unwrap();
            if step % 4 == 0 {
                sim.tick_deferred_comb(clocks[((step + 1) % 3) as usize])
                    .unwrap();
            }
            for output in outputs {
                seen.push(sim.get(output));
            }
        }
        seen
    }
    let builder = || crate::Simulator::builder(CODE, "Top");
    let interpreted = builder().build_interpreter().unwrap();
    assert_eq!(interpreted.backend.eval_apply_units.len(), 3);
    assert_eq!(run(interpreted), run(builder().build().unwrap()));
}

/// A testbench schedules its flip-flops in separate eval and apply
/// groups. Those groups snapshot only the trigger addresses their own
/// units store, not the combinational ones, and trigger the same
/// edges as the default compiled backend.
#[cfg(feature = "systemverilog")]
#[test]
fn split_groups_snapshot_only_their_own_trigger_addresses() {
    const SOURCE: &str = "`timescale 1ns/1ns
module Top;
  logic clk = 0, en = 0;
  logic gclk;
  logic [7:0] q = 0, g = 0, seen = 0;
  assign gclk = clk & en;
  always #5 clk = ~clk;
  always_ff @(posedge clk) q <= q + 8'd1;
  always_ff @(posedge gclk) g <= g + q;
  always @(posedge gclk) seen = seen + 8'd1;
  initial begin
    #23 en = 1;
    #40 en = 0;
    #20 en = 1;
  end
endmodule
";
    let path = std::path::Path::new("top.sv");
    let build = || crate::Simulation::from_sv_sources(vec![(SOURCE, path)], "Top");
    let mut interpreted = build().build_interpreter().unwrap();
    let mut compiled = build().build().unwrap();
    let backend = &interpreted.simulator.backend;
    assert!(backend.emit_triggers);
    assert!(!backend.comb_trigger_addrs.is_empty());
    assert!(!backend.apply_units.is_empty());
    for group in backend
        .eval_only_units
        .values()
        .chain(backend.apply_units.values())
    {
        for addr in &group.trigger_addrs {
            assert!(backend.comb_trigger_addrs.binary_search(addr).is_err());
        }
    }
    let mut values = Vec::new();
    for time in (0..200).step_by(3) {
        interpreted.run_until(time).unwrap();
        compiled.run_until(time).unwrap();
        for name in ["q", "g", "seen"] {
            let (a, b) = (interpreted.signal(name), compiled.signal(name));
            values.push((time, name, interpreted.get(a), compiled.get(b)));
        }
    }
    let seen = interpreted.signal("seen");
    assert!(interpreted.get(seen) > BigUint::from(3u8));
    for (time, name, interpreted, compiled) in values {
        assert_eq!(interpreted, compiled, "{name} at {time}");
    }
}
