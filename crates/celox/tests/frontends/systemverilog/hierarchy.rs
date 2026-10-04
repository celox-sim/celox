use super::*;

sv_backends! {
    fn drives_omitted_and_open_child_inputs_with_z(sim) {
        @case "hierarchy::drives_omitted_and_open_child_inputs_with_z";
    }

    fn treats_child_outputs_as_explicit_net_drivers(sim) {
        @case "hierarchy::treats_child_outputs_as_explicit_net_drivers";
    }

    fn simulates_systemverilog_named_port_hierarchy(sim) {
        @case "hierarchy::simulates_systemverilog_named_port_hierarchy";
    }

    fn simulates_systemverilog_hierarchy_through_internal_signal(sim) {
        @case "hierarchy::simulates_systemverilog_hierarchy_through_internal_signal";
    }

    fn simulates_veryl_generated_style_gray_encoder_hierarchy(sim) {
        @case "hierarchy::simulates_veryl_generated_style_gray_encoder_hierarchy";
    }

    fn simulates_parameter_specialized_systemverilog_hierarchy(sim) {
        @case "hierarchy::simulates_parameter_specialized_systemverilog_hierarchy";
    }

    fn simulates_systemverilog_hierarchical_always_ff(sim) {
        @case "hierarchy::simulates_systemverilog_hierarchical_always_ff";
    }

    fn simulates_systemverilog_hierarchical_always_ff_with_constant_clear(sim) {
        @case "hierarchy::simulates_systemverilog_hierarchical_always_ff_with_constant_clear";
    }

    fn simulates_veryl_generated_countones_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/Countones.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("Countones.sv"))], "Top");

    let i_data = sim.signal("i_data");
    let o_ones = sim.signal("o_ones");

    for value in [0u64, 1, 0xffff, 0xdead_beef, u64::MAX] {
        sim.modify(|io| io.set(i_data, value)).unwrap();
        assert_eq!(sim.get(o_ones), value.count_ones().into());
    }
    }

    fn simulates_veryl_generated_onehot_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/Onehot.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("Onehot.sv"))], "Top");

    let i_data = sim.signal("i_data");
    let o_onehot = sim.signal("o_onehot");
    let o_zero = sim.signal("o_zero");

    for value in [0u64, 1, 2, 3, 1u64 << 63, (1u64 << 63) | 1] {
        sim.modify(|io| io.set(i_data, value)).unwrap();
        assert_eq!(sim.get(o_onehot), u8::from(value.count_ones() == 1).into());
        assert_eq!(sim.get(o_zero), u8::from(value == 0).into());
    }
    }

    fn simulates_veryl_generated_edge_detector_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/EdgeDetector.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("EdgeDetector.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_data = sim.signal("i_data");
    let o_edge = sim.signal("o_edge");
    let o_posedge = sim.signal("o_posedge");
    let o_negedge = sim.signal("o_negedge");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_data, 0u32);
    }).unwrap();
    sim.tick(clk).unwrap();

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();

    sim.modify(|io| io.set(i_data, 1u32)).unwrap();
    assert_eq!(sim.get(o_edge), 1u8.into());
    assert_eq!(sim.get(o_posedge), 1u8.into());
    assert_eq!(sim.get(o_negedge), 0u8.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_edge), 0u8.into());

    sim.modify(|io| io.set(i_data, 0u32)).unwrap();
    assert_eq!(sim.get(o_edge), 1u8.into());
    assert_eq!(sim.get(o_posedge), 0u8.into());
    assert_eq!(sim.get(o_negedge), 1u8.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_edge), 0u8.into());
    }

    fn simulates_veryl_generated_std_counter_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/StdCounter.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("StdCounter.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_up = sim.signal("i_up");
    let o_count = sim.signal("o_count");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_up, 0u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 0u32.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_up, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 1u32.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 2u32.into());
    }

    fn simulates_veryl_generated_gray_counter_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/GrayCounter.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("GrayCounter.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_up = sim.signal("i_up");
    let o_count = sim.signal("o_count");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_up, 0u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 0u32.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_up, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 1u32.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 3u32.into());
    }

    fn simulates_veryl_generated_lfsr_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/Lfsr.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("Lfsr.sv"))], "Top");

    let clk = sim.event("clk");
    let i_en = sim.signal("i_en");
    let i_set = sim.signal("i_set");
    let i_setval = sim.signal("i_setval");
    let o_val = sim.signal("o_val");

    sim.modify(|io| {
        io.set(i_en, 1u8);
        io.set(i_set, 1u8);
        io.set(i_setval, 1u32);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_val), 1u32.into());

    sim.modify(|io| io.set(i_set, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_val), 0x8000_0057u32.into());
    }

}

#[test]
fn simulates_veryl_generated_gray_codec_sv_smoke() {
    let sv = include_str!("../../../testdata/verilator/GrayCodec.sv");
    let mut sim = Simulator::from_sv_sources(vec![(sv, Path::new("GrayCodec.sv"))], "Top")
        .build_native()
        .unwrap();
    let i_bin = sim.signal("i_bin");
    let o_gray = sim.signal("o_gray");
    let o_bin = sim.signal("o_bin");

    sim.modify(|io| io.set(i_bin, 0xdead_beefu32)).unwrap();

    assert_eq!(sim.get(o_gray), 0xb1fb_6198u32.into());
    assert_eq!(sim.get(o_bin), 0xdead_beefu32.into());
}

#[test]
fn builds_veryl_generated_verilator_sv_smoke() {
    for (name, sv) in [
        (
            "Countones.sv",
            include_str!("../../../testdata/verilator/Countones.sv"),
        ),
        (
            "Fifo.sv",
            include_str!("../../../testdata/verilator/Fifo.sv"),
        ),
        (
            "EdgeDetector.sv",
            include_str!("../../../testdata/verilator/EdgeDetector.sv"),
        ),
        (
            "GrayCodec.sv",
            include_str!("../../../testdata/verilator/GrayCodec.sv"),
        ),
        (
            "GrayCounter.sv",
            include_str!("../../../testdata/verilator/GrayCounter.sv"),
        ),
        (
            "Lfsr.sv",
            include_str!("../../../testdata/verilator/Lfsr.sv"),
        ),
        (
            "Onehot.sv",
            include_str!("../../../testdata/verilator/Onehot.sv"),
        ),
        (
            "StdCounter.sv",
            include_str!("../../../testdata/verilator/StdCounter.sv"),
        ),
    ] {
        Simulator::from_sv_sources(vec![(sv, Path::new(name))], "Top")
            .build_native()
            .unwrap_or_else(|err| panic!("failed to build {name}: {err:?}"));
    }
}
