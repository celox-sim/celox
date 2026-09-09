#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

#[cfg(test)]
mod tests {
    use celox::{IOContext, SimBackend, SimulatorBuilder};
    use std::fs;
    use std::path::Path;

    #[test]
    fn test_vcd_generation() {
        let code = r#"
        module Top (
            a: input logic<8>,
            b: output logic<8>,
        ) {
            assign b = a;
        }
        "#;

        let vcd_path = "test_output.vcd";
        let mut sim = SimulatorBuilder::new(code, "Top")
            .vcd(vcd_path)
            .build()
            .unwrap();

        let a = sim.signal("a");
        sim.modify(|ctx: &mut IOContext| {
            ctx.set(a, 8u8);
        })
        .unwrap();

        sim.dump(0);
        sim.dump(10);
        sim.flush_vcd().unwrap();

        assert!(Path::new(vcd_path).exists());
        let content = fs::read_to_string(vcd_path).unwrap();
        assert!(content.contains("$var wire 8"));
        assert!(content.contains("#0"));
        assert!(content.contains("#10"));

        fs::remove_file(vcd_path).unwrap();
    }

    /// VCD descriptors assume contiguous packed storage, so a tiered build
    /// that records traces must lay out unpacked arrays packed instead of the
    /// element-strided form its compiled tier prefers. Regression guard for
    /// the tiered factories that create their VCD writer post-build. The
    /// dynamically indexed array matters: only designs that would otherwise
    /// adopt element-strided storage expose the divergence.
    #[test]
    fn test_tiered_vcd_matches_packed_reference_for_unpacked_arrays() {
        let code = r#"
        module Top (
            clk: input clock,
            rst: input reset,
            d: input logic<3>,
            widx: input logic<2>,
            ridx: input logic<2>,
            q0: output logic<3>,
        ) {
            var mem: logic<3>[4];
            always_ff (clk, rst) {
                if_reset {
                    mem[0] = 3'd0;
                } else {
                    mem[widx] = d;
                }
            }
            assign q0 = mem[ridx];
        }
        "#;

        let reference_path = "test_vcd_packed_reference.vcd";
        let tiered_path = "test_vcd_tiered.vcd";

        let mut reference = SimulatorBuilder::new(code, "Top")
            .vcd(reference_path)
            .build_interpreter()
            .unwrap();
        let mut tiered = SimulatorBuilder::new(code, "Top")
            .vcd(tiered_path)
            .build_tiered()
            .unwrap();

        assert!(
            tiered.layout().unpacked_arrays.is_empty(),
            "VCD recording must force the packed layout for tiered builds"
        );

        // Sanity: without VCD the same build adopts element-strided storage
        // for `mem` on hosts whose default compiled tier is native, so the
        // trace comparison really exercises both layouts. Cross-codegen
        // builds default to Cranelift, whose tiered target layout is packed
        // either way.
        if native_default_compiled_tier() {
            let strided = SimulatorBuilder::new(code, "Top").build_tiered().unwrap();
            assert_eq!(
                strided.layout().unpacked_arrays.len(),
                1,
                "expected `mem` to be element-strided when not recording VCD"
            );
        }

        drive(&mut reference);
        dump_at(&mut reference, 10);

        drive(&mut tiered);
        dump_at(&mut tiered, 10);

        reference.flush_vcd().unwrap();
        tiered.flush_vcd().unwrap();
        assert_eq!(without_date(reference_path), without_date(tiered_path));

        fs::remove_file(reference_path).unwrap();
        fs::remove_file(tiered_path).unwrap();
    }

    /// Whether the host's default compiled tier is the native backend.
    ///
    /// Mirrors the crate's `native_is_default_target` selection, which is
    /// not public; cross-codegen builds fall back to Cranelift.
    fn native_default_compiled_tier() -> bool {
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        {
            true
        }
        #[cfg(not(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        )))]
        {
            false
        }
    }

    /// Identical stimulus for every backend under comparison.
    fn drive<B: SimBackend>(sim: &mut celox::Simulator<B>) {
        let rst = sim.signal("rst");
        let d = sim.signal("d");
        let widx = sim.signal("widx");
        sim.modify(|io| {
            io.set(rst, 1u8);
        })
        .unwrap();
        tick(sim, 1);
        for lane in 0..4u8 {
            sim.modify(|io| {
                io.set(rst, 0u8);
                io.set(d, lane + 1);
                io.set(widx, lane);
            })
            .unwrap();
            tick(sim, 1);
        }
    }

    fn dump_at<B: SimBackend>(sim: &mut celox::Simulator<B>, timestamp: u64) {
        sim.dump(timestamp);
    }

    fn tick<B: SimBackend>(sim: &mut celox::Simulator<B>, ticks: usize) {
        let clk = sim
            .named_events()
            .iter()
            .find(|event| event.name == "clk")
            .expect("clk event")
            .id;
        sim.tick_by_id_n(clk, ticks as u32).unwrap();
    }

    /// File contents with the `$date` block removed so runs are comparable.
    fn without_date(path: &str) -> String {
        let mut out = String::new();
        let mut in_date_block = false;
        for line in fs::read_to_string(path).unwrap().lines() {
            if in_date_block {
                if line.trim() == "$end" {
                    in_date_block = false;
                }
                continue;
            }
            if line.trim() == "$date" {
                in_date_block = true;
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

mod activity {
    use celox::{SimBackend, Simulator, SimulatorBuilder, VcdWriter};
    use num_bigint::BigUint;

    const SOURCE: &str = r#"
        module Top (
            clk: input clock, rst: input reset,
            enable: input logic, index: input logic<3>, data: input logic<65>,
            q: output logic<65>, alias_out: output logic<65>, comb: output logic<65>,
            read: output logic<9>,
        ) {
            var mem: logic<9>[8];
            assign alias_out = data;
            assign comb = data ^ (data << 1);
            assign read = mem[index];
            always_ff (clk, rst) {
                if_reset {
                    q = 0;
                    mem = '0;
                } else if enable {
                    q = data;
                    mem[index] = data[0+:9];
                }
            }
        }
    "#;

    fn commands(bytes: &[u8]) -> Vec<vcd::Command> {
        let mut parser = vcd::Parser::new(bytes);
        parser.parse_header().unwrap();
        parser.map(Result::unwrap).collect()
    }

    fn compare<B: SimBackend>(mut sim: Simulator<B>, path: &std::path::Path, four_state: bool) {
        assert!(sim.layout().trace.is_some());
        let descs = sim.build_vcd_descs(four_state);
        let mut reference = VcdWriter::from_writer(Vec::new(), &descs);
        let clk = sim.event("clk");
        let rst = sim.signal("rst");
        let data = sim.signal("data");
        let index = sim.signal("index");
        let enable = sim.signal("enable");
        let dump = |sim: &mut Simulator<B>, reference: &mut VcdWriter<Vec<u8>>, time| {
            sim.dump(time);
            let (ptr, size) = sim.memory_as_ptr();
            reference
                .dump(time, unsafe { std::slice::from_raw_parts(ptr, size) })
                .unwrap();
        };
        // Initial dump works even without any registered activity.
        dump(&mut sim, &mut reference, 0);
        sim.set(rst, 0u8);
        sim.tick(clk).unwrap();
        dump(&mut sim, &mut reference, 1);
        sim.set(rst, 1u8);
        for step in 2..34u64 {
            let value = (BigUint::from(1u8) << (step as usize % 65)) | BigUint::from(step);
            let mask = if four_state && step % 3 == 0 {
                BigUint::from(0x155u16)
            } else {
                BigUint::default()
            };
            sim.modify(|io| {
                io.set(index, (step % 8) as u8);
                io.set(enable, u8::from(step % 4 != 0));
                io.set_four_state(data, value.clone(), mask.clone());
            })
            .unwrap();
            // Notifications must survive several clock/comb evaluations before dump.
            sim.tick(clk).unwrap();
            sim.tick(clk).unwrap();
            dump(&mut sim, &mut reference, step);
            let before = sim.vcd_statistics().unwrap().comparisons;
            dump(&mut sim, &mut reference, step);
            assert_eq!(
                sim.vcd_statistics().unwrap().comparisons,
                before,
                "a second dump without writes must not rescan memory"
            );
        }
        // Changes that return to the previous dump value are suppressed.
        let old = sim.get(data);
        sim.set_wide(data, &old ^ BigUint::from(1u8));
        sim.set_wide(data, old);
        dump(&mut sim, &mut reference, 35);
        // A retained mutable pointer bypasses setters on more than one dump.
        let (raw, _) = sim.memory_as_mut_ptr();
        for time in 36..39 {
            unsafe {
                *raw.add(data.offset) ^= 1;
            }
            sim.modify(|_| {}).unwrap();
            dump(&mut sim, &mut reference, time);
        }
        sim.flush_vcd().unwrap();
        let actual = std::fs::read(path).unwrap();
        let expected = reference.into_inner().unwrap();
        assert!(!commands(&actual).is_empty());
        assert_eq!(commands(&actual), commands(&expected));
    }

    #[test]
    fn generated_notifications_match_full_scan_across_backends() {
        let dir = tempfile::tempdir().unwrap();
        for optimized in [false, true] {
            for four_state in [false, true] {
                let path = dir.path().join("wave.vcd");
                let builder = || {
                    SimulatorBuilder::new(SOURCE, "Top")
                        .optimize(optimized)
                        .four_state(four_state)
                        .vcd(&path)
                };
                compare(builder().build().unwrap(), &path, four_state);
                compare(builder().build_cranelift().unwrap(), &path, four_state);
                compare(builder().build_interpreter().unwrap(), &path, four_state);
                compare(builder().build_wasm().unwrap(), &path, four_state);
                compare(builder().build_tiered().unwrap(), &path, four_state);
            }
        }
    }

    #[test]
    fn clock_trigger_consumption_does_not_consume_waveform_activity() {
        let code = r#"
            module Top (clk: input clock, rst: input reset, div: output logic, q: output logic<8>) {
                let gclk: '_ clock = clk;
                always_ff (clk, rst) { if_reset { div = 0; } else { div = ~div; } }
                always_ff (gclk, rst) { if_reset { q = 0; } else { q += 1; } }
            }
        "#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cascade.vcd");
        let mut sim = celox::Simulation::builder(code, "Top")
            .vcd(&path)
            .build()
            .unwrap();
        sim.add_clock("clk", 10, 10);
        let rst = sim.signal("rst");
        sim.modify(|io| io.set(rst, 0u8)).unwrap();
        // The reference writer reads the same committed state after the scheduler.
        let descs = ["clk", "rst", "div", "q"].map(|name| {
            let signal = sim.signal(name);
            celox::VcdSignalDesc {
                scope: "reference".into(),
                name: name.into(),
                offset: signal.offset,
                width: signal.width,
                is_4state: false,
            }
        });
        let mut reference = VcdWriter::from_writer(Vec::new(), &descs);
        for step in 0..20 {
            if step == 2 {
                let rst = sim.signal("rst");
                sim.modify(|io| io.set(rst, 1u8)).unwrap();
            }
            let time = sim.step().unwrap().unwrap();
            sim.dump(time);
            let (ptr, size) = sim.memory_as_ptr();
            reference
                .dump(time, unsafe { std::slice::from_raw_parts(ptr, size) })
                .unwrap();
        }
        sim.flush_vcd().unwrap();
        let bytes = std::fs::read(path).unwrap();
        let actual = commands(&bytes);
        let reference_bytes = reference.into_inner().unwrap();
        let expected = commands(&reference_bytes);
        // Match by declaration names, since the full design includes scopes and aliases.
        fn values(bytes: &[u8], names: &[&str]) -> Vec<(u64, String, String)> {
            let mut parser = vcd::Parser::new(bytes);
            let header = parser.parse_header().unwrap();
            fn vars(
                items: &[vcd::ScopeItem],
                out: &mut Vec<(vcd::IdCode, String)>,
                names: &[&str],
            ) {
                for item in items {
                    match item {
                        vcd::ScopeItem::Var(var) if names.contains(&var.reference.as_str()) => {
                            out.push((var.code, var.reference.clone()))
                        }
                        vcd::ScopeItem::Scope(scope) => vars(&scope.items, out, names),
                        _ => {}
                    }
                }
            }
            let mut ids = vec![];
            vars(&header.items, &mut ids, names);
            let mut time = 0;
            let mut out = vec![];
            for command in parser.map(Result::unwrap) {
                let pair = match command {
                    vcd::Command::Timestamp(t) => {
                        time = t;
                        None
                    }
                    vcd::Command::ChangeScalar(id, value) => Some((id, value.to_string())),
                    vcd::Command::ChangeVector(id, value) => Some((id, value.to_string())),
                    _ => None,
                };
                if let Some((id, value)) = pair {
                    for (_, name) in ids.iter().filter(|(code, _)| *code == id) {
                        out.push((time, name.clone(), value.clone()));
                    }
                }
            }
            out.sort();
            out
        }
        assert!(actual.len() > 20 && expected.len() > 20);
        assert_eq!(
            values(&bytes, &["clk", "rst", "div", "q"]),
            values(&reference_bytes, &["clk", "rst", "div", "q"])
        );
        assert!(
            values(&bytes, &["q"])
                .iter()
                .any(|(_, _, value)| value != "0")
        );
    }
}
