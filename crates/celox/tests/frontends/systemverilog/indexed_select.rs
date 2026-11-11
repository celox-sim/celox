use super::*;

// IEEE 1800-2023 11.5.1: the index operator and declaration direction
// independently determine the endpoints of the selected range.
sv_backends! {

    fn veryl_emitted_parameterized_step_generate(sim) {
        @setup {
            let veryl = r#"
                module Child #(param WIDTH: u32 = 4) (
                    data: input logic<WIDTH * 4>, y: output logic<WIDTH * 4>,
                ) {
                    for i in 0..4: lanes {
                        assign y[i step WIDTH] = data[(3-i) step WIDTH];
                    }
                }
                module Top (data: input logic<16>, wide: output logic<16>, narrow: output logic<8>) {
                    inst wide_child: Child #(WIDTH: 4) (data, y: wide);
                    inst narrow_child: Child #(WIDTH: 2) (data: data[7:0], y: narrow);
                }
            "#;
            let emitted = celox_test_suite::veryl::emit::emit_veryl_sources(&[(veryl, Path::new("step_generate.veryl"))]);
            assert!(emitted.as_sv_sources().iter().any(|(source, _)| source.contains("+:")));
        }
        @build Simulator::from_sv_sources(emitted.as_sv_sources(), "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x12e4u16)).unwrap();
        assert_eq!(sim.get(sim.signal("wide")), 0x4e21u16.into());
        assert_eq!(sim.get(sim.signal("narrow")), 0x1bu8.into());
    }

}

#[test]
fn rejects_nonpositive_and_runtime_indexed_widths() {
    for width in ["0", "-1", "width", "{2{1'b0}}", "{0{1'b1}}"] {
        let source = format!(
            "module Top(input logic [7:0] data, input int width, output logic [7:0] y); assign y = data[0 +: {width}]; endmodule"
        );
        let error =
            Simulator::from_sv_sources(vec![(&source, Path::new("invalid_width.sv"))], "Top")
                .build_cranelift()
                .expect_err("invalid indexed width must be rejected")
                .to_string();
        assert!(error.contains("indexed part-select"), "{error}");
    }
}

#[test]
fn rejects_selected_widths_that_are_nonpositive() {
    for width in ["W[0]", "$clog2(W[0])", "W[1:0] - 2", "int'(W[1:0]) - 3"] {
        let source = format!(
            "module Top(input logic [15:0] data, output logic [15:0] y); localparam logic [3:0] W = 4'b1010; assign y = data[0 +: ({width})]; endmodule"
        );
        let error = Simulator::from_sv_sources(
            vec![(&source, Path::new("invalid_selected_width.sv"))],
            "Top",
        )
        .build_cranelift()
        .expect_err("selected width must be positive")
        .to_string();
        assert!(error.contains("indexed part-select"), "{error}");
    }
}

#[test]
fn rejects_unresolved_indexed_parameter_initializers_after_collection() {
    let source = "module Top; localparam logic [7:0] P = 8'hab; localparam logic [3:0] Q = P[MISSING +: 4]; endmodule";
    Simulator::from_sv_sources(
        vec![(source, Path::new("unresolved_indexed_parameter.sv"))],
        "Top",
    )
    .build_cranelift()
    .expect_err("an unused unresolved indexed initializer must be rejected");
}

#[test]
fn rejects_unresolved_logical_parameter_type_queries() {
    for expression in [
        "missing && 0",
        "0 && missing",
        "missing || 1",
        "1 || missing",
    ] {
        let source = format!("module Top; localparam N = $bits({expression}); endmodule");
        Simulator::from_sv_sources(
            vec![(&source, Path::new("unresolved_logical_query.sv"))],
            "Top",
        )
        .build_cranelift()
        .expect_err("unresolved operands must not acquire a logical result type");
    }
}

/// A continuous assignment to a part-select partly outside its vector drives
/// the bits inside, and one wholly outside drives nothing (IEEE 1800-2023
/// 11.5.1). Icarus 13.0 aborts on the partial assignment, so this is no
/// suite case.
#[test]
fn continuous_part_selects_partly_outside_drive_the_bits_inside() {
    let source = r#"
        module Top(input logic [3:0] v, output logic [7:0] q, r);
            assign q[9:6] = v;
            assign q[5:0] = 6'h01;
            assign r[11:8] = v;
            assign r[7:0] = 8'h5a;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("continuous_partly_outside.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let (v, q, r) = (sim.signal("v"), sim.signal("q"), sim.signal("r"));
    sim.modify(|io| io.set(v, 0xfu8)).unwrap();
    assert_eq!(sim.get(q), 0xc1u8.into());
    assert_eq!(sim.get(r), 0x5au8.into());
}

// Icarus 13.0 returns X here, contrary to IEEE 1800-2023 7.4.5 Table 7-1.
// Keep this regression local, like the partial continuous-select check above.
sv_backends! {
    fn constant_invalid_two_state_unpacked_read(sim) {
        @setup {
            let source = "module Top(output logic [7:0] q); bit [7:0] b[1]; assign q=b[1]; endmodule";
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("invalid_bit_element.sv"))], "Top").four_state(true);
        let q=sim.signal("q");
        assert_eq!(sim.get_four_state(q), (BigUint::default(), BigUint::default()));
    }
}

// Verilator 5.052 truncates the bit index of a one-bit packed element and
// writes its zero bit for index 2. Keep the out-of-range check local; the
// disjoint-driver behavior at valid indices is shared with both adapters.
sv_backends! {
    fn single_bit_array_drivers_ignore_invalid_inner_indices(sim) {
        @setup {
            let source = "module Top(input logic [1:0] index, input logic [2:0] value,
                output wire [2:0] q);
                logic [0:0] ready[3];
                for (genvar n=0; n<3; n=n+1) begin : lanes
                    always_comb begin ready[n]='0; ready[n][index]=value[n]; end
                    assign q[n]=ready[n][0];
                end
                endmodule";
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("single_bit_bounds.sv"))], "Top");
        let index=sim.signal("index");
        let value=sim.signal("value");
        let q=sim.signal("q");
        for (position, bits, expected) in [(0u8,5u8,5u8),(1,5,0),(2,5,0),(0,2,2)] {
            sim.modify(|io| {io.set(index,position);io.set(value,bits);}).unwrap();
            assert_eq!(sim.get(q),BigUint::from(expected));
        }
    }
}

// A counted loop over a module variable, as Verilog-2001 memory models
// write their masked words: each iteration selects a constant part of the
// addressed word, though other processes can see the loop variable.
#[test]
fn loops_over_module_variables_select_parts_of_memory_words() {
    let source = r#"
        module Top(
            input logic clk, en, input logic [2:0] addr, input logic [1:0] mask,
            input logic [99:0] wdata, output logic [99:0] rdata, output integer last
        );
            reg [99:0] ram [7:0];
            integer i;
            always @(posedge clk)
                if (en) begin
                    for (i = 0; i < 2; i = i + 1) begin
                        if (mask[i]) ram[addr][i*50 +: 50] <= wdata[i*50 +: 50];
                    end
                end
            assign rdata = ram[addr];
            assign last = i;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("memory.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let (en, addr, mask, wdata) = (
        sim.signal("en"),
        sim.signal("addr"),
        sim.signal("mask"),
        sim.signal("wdata"),
    );
    let (rdata, last) = (sim.signal("rdata"), sim.signal("last"));
    let word = |high: u64, low: u64| (BigUint::from(high) << 50u32) | BigUint::from(low);
    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(addr, 3u8);
        io.set(mask, 0b11u8);
        io.set_wide(wdata, word(0x1234567, 0x89abcde));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(rdata), word(0x1234567, 0x89abcde));
    // The variable holds the value that ended the loop.
    assert_eq!(sim.get(last), 2u8.into());
    sim.modify(|io| {
        io.set(mask, 0b10u8);
        io.set_wide(wdata, word(0x3_ffff_ffff_ffff, 0));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(rdata), word(0x3_ffff_ffff_ffff, 0x89abcde));
    // Another word is untouched.
    sim.modify(|io| io.set(addr, 4u8)).unwrap();
    assert_eq!(sim.get(rdata), 0u8.into());
}

// A loop whose body changes its module variable through a system function
// that writes an argument runs as written: `$random(i)` stores the next
// seed in `i` (IEEE 1800-2023 20.14.1), which ends this loop after one
// iteration.
#[test]
fn loops_whose_body_writes_the_variable_through_a_system_function_run_as_written() {
    let source = r#"
        `timescale 1ns/1ns
        module Top;
            integer i, count, x;
            initial begin
                count = 0;
                for (i = 1; i < 4; i = i + 1) begin
                    x = $random(i);
                    count = count + 1;
                end
                #1 $display("%0d %0d", count, i > 4);
            end
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("seed.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    sim.run_until(10).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["1 1"]);
}

// A sampled value of the variable of a loop over a module variable is that
// of the start of the time slot (IEEE 1800-2023 16.9.3), whatever the
// iterations give the variable.
#[test]
fn loops_over_module_variables_keep_sampled_values() {
    let source = r#"
        `timescale 1ns/1ns
        module Top;
            integer i, s0, s1;
            initial begin
                i = 7;
                #1;
                s0 = 0; s1 = 0;
                for (i = 0; i < 3; i = i + 1) begin
                    s0 = s0 + $sampled(i);
                    s1 = s1 + i;
                end
                $display("%0d %0d", s0, s1);
            end
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("sampled.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(10).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["21 3"]);
}
