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

        // Sanity: without VCD the same build does adopt element-strided
        // storage for `mem`, so this test actually exercises both layouts.
        let strided = SimulatorBuilder::new(code, "Top").build_tiered().unwrap();
        assert_eq!(
            strided.layout().unpacked_arrays.len(),
            1,
            "expected `mem` to be element-strided when not recording VCD"
        );
        drop(strided);

        drive(&mut reference);
        dump_at(&mut reference, 10);

        drive(&mut tiered);
        dump_at(&mut tiered, 10);

        assert_eq!(without_date(reference_path), without_date(tiered_path));

        fs::remove_file(reference_path).unwrap();
        fs::remove_file(tiered_path).unwrap();
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
