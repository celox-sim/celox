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

// An element of an unpacked array whose element width calls `$clog2` is read
// and written at a run-time index (IEEE 1800-2023 20.8.1), as Vortex's MSHR
// table `reg [32 - $clog2(64) - $clog2(4) - 1:0] addr_table [16]` is, with a
// constant argument or a parameter.
#[test]
fn unpacked_elements_whose_width_calls_clog2_take_run_time_indices() {
    let source = r#"
        `timescale 1ns/1ns
        module Top #(parameter int M = 32) ();
            logic [32 - $clog2(64) - $clog2(4) - 1:0] table_a [16];
            logic [$clog2(M)-1:0] table_b [4];
            logic clk = 0;
            logic [3:0] id = 3;
            logic [1:0] k = 2;
            logic [23:0] read_a;
            logic [4:0] read_b;
            always_comb read_a = table_a[id];
            always @(posedge clk) begin
                table_a[id] <= 24'habcdef;
                table_b[k] <= 5'd21;
            end
            initial begin
                foreach (table_a[i]) table_a[i] = '0;
                foreach (table_b[i]) table_b[i] = '0;
                table_b[k + 1] = 5'd9;
                #1 clk = 1;
                #1 read_b = table_b[k];
                $display("%h %h %0d %0d %0d", read_a, table_a[id - 1], read_b, table_b[3],
                    $bits(table_b[k]));
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("clog2_elements.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build_cranelift()
        .unwrap();
        sim.run_until(10).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            displayed,
            ["abcdef 000000 21 9 5"],
            "four_state={four_state}"
        );
    }
}

// A select whose index reads an element of an unpacked array, such as
// OpenTitan's Keccak `state[PiRotate[x][y]][x]` with a localparam array,
// or a variable array a process computes (IEEE 1800-2023 7.4.6, 11.5.1).
// Verilator 5.054 gives the same values.
sv_backends! {
    fn selects_index_by_unpacked_array_elements(sim) {
        @setup {
            let source = r#"
                module Top(input logic [1:0] rot, output logic [2:0][2:0][3:0] r, p);
                    localparam int W = 4;
                    typedef logic [2:0][2:0][W-1:0] box_t;
                    localparam int PiRotate [9] = '{0, 2, 1, 1, 0, 2, 2, 1, 0};
                    function automatic box_t pi(box_t state);
                        box_t result;
                        for (int x = 0; x < 3; x++)
                            for (int y = 0; y < 3; y++)
                                result[x][y][W-1:0] = state[PiRotate[3*x+y]][x][W-1:0];
                        return result;
                    endfunction
                    box_t s;
                    int idx [3][3];
                    assign s = 36'h876543210;
                    always_comb
                        for (int x = 0; x < 3; x++)
                            for (int y = 0; y < 3; y++) idx[x][y] = (x + y + rot) % 3;
                    always_comb
                        for (int x = 0; x < 3; x++)
                            for (int y = 0; y < 3; y++) r[x][y] = s[idx[x][y]][x];
                    assign p = pi(s);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("element_index.sv"))], "Top");
        let (rot, r, p) = (sim.signal("rot"), sim.signal("r"), sim.signal("p"));
        for (value, expected) in [(0u8, 0x5_2817_4630u64), (1, 0x8_5241_7063), (2, 0x2_8574_1306)] {
            sim.modify(|io| io.set(rot, value)).unwrap();
            assert_eq!(sim.get(r), expected.into(), "rot={value}");
            assert_eq!(sim.get(p), 0x2_5871_4360u64.into(), "rot={value}");
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

// Port actuals that select an element of a packed array whose bound is a
// power, as OpenTitan's `prim_generic_otp` connects `wdata_q[cnt_q]` of a
// `logic [2**SizeWidth-1:0][Width-1:0]`, at a run-time or a constant index,
// and port actuals that raise to a run-time power (IEEE 1800-2023 11.4.3,
// 11.5.1, 23.3.3). Verilator 5.054 gives the same values.
#[test]
fn port_actuals_select_elements_of_power_bounded_arrays() {
    let source = r#"
        `timescale 1ns/1ns
        module enc(input [15:0] data_i, output logic [21:0] data_o);
            assign data_o = {6'h2a, data_i};
        endmodule
        module pass #(parameter int W = 8) (input [W-1:0] d, output logic [W-1:0] q);
            assign q = d;
        endmodule
        module otp #(parameter int Width = 16, parameter int SizeWidth = 2) (
            input logic [SizeWidth-1:0] cnt_q, output logic [21:0] o, k, output logic [7:0] s, p
        );
            localparam int Depth = 2**SizeWidth;
            logic [2**SizeWidth-1:0][Width-1:0] wdata_q;
            logic [SizeWidth**2-1:0][3:0] sq;
            logic [Depth-1:0][7:0] dq;
            assign wdata_q = 64'h1111_2222_3333_4444;
            assign sq = 16'hdcba;
            assign dq = 32'h4433_2211;
            enc u_enc(.data_i(wdata_q[cnt_q]), .data_o(o));
            enc u_const(.data_i(wdata_q[2]), .data_o(k));
            pass u_sq(.d({sq[cnt_q], sq[0]}), .q(s));
            pass u_dq(.d(dq[cnt_q]), .q(p));
        endmodule
        module Top;
            logic [1:0] c = 2'd1;
            logic signed [7:0] b = -8'sd3;
            logic [21:0] o, k;
            logic [7:0] s, p, e, f;
            logic [15:0] g;
            otp u(.cnt_q(c), .o, .k, .s, .p);
            pass u_e(.d(c ** 2), .q(e));
            pass u_f(.d(8'd2 ** c), .q(f));
            pass #(16) u_g(.d(b ** c), .q(g));
            initial begin
                #1 $display("%h %h %h %h %h %h %h", o, k, s, p, e, f, g);
                c = 2'd3;
                #1 $display("%h %h %h %h %h %h %h", o, k, s, p, e, f, g);
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("power_bounds.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        sim.run_until(10).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            displayed,
            [
                "2a3333 2a2222 ba 22 01 02 fffd",
                "2a1111 2a2222 da 44 09 08 ffe5",
            ],
            "four_state={four_state}"
        );
    }
}

// A run-time part-select within an array element at a run-time index stays in
// the element (IEEE 1800-2023 7.4.5, 11.5.1): the bits it selects outside the
// element (or the structure member) read X and are not written, by blocking
// and nonblocking assignments alike, and an element outside the array reads X
// and is not written. Here a member of an element of a packed and of an
// unpacked array of structures, as OpenPiton's write buffer writes
// `wbuffer_d[wr_ptr].data[k*8 +: 8]`, and the bits of an element of a plain
// packed array and of an unpacked array.
#[test]
fn run_time_part_selects_stay_in_their_array_element() {
    let source = r#"
        `timescale 1ns/1ns
        module Top;
            typedef struct packed { logic [3:0] tag; logic [7:0] data; logic [3:0] low; } entry_t;
            entry_t [3:0] packed_q, packed_n;
            entry_t unpacked_q [4], unpacked_n [4];
            logic [3:0][7:0] a, a_n;
            logic [7:0] u [4], u_n [4];
            logic clk = 0;
            int i, j;
            task automatic show(input string what);
                $display("%s %h %h %h %h %h %h %h %h %h %h", what, packed_q,
                    unpacked_q[0], unpacked_q[1], unpacked_q[2], unpacked_q[3],
                    a, u[0], u[1], u[2], u[3]);
            endtask
            task automatic clear();
                packed_q = '0;
                foreach (unpacked_q[e]) unpacked_q[e] = '0;
                a = '0;
                foreach (u[e]) u[e] = '0;
            endtask
            initial begin
                // Ones everywhere: what a select reads past its member or
                // element would be a one.
                packed_q = '1;
                foreach (unpacked_q[e]) unpacked_q[e] = '1;
                a = '1;
                foreach (u[e]) u[e] = '1;
                packed_q[2].data = 8'b1010_0110;
                unpacked_q[2].data = 8'b1010_0110;
                a[2] = 8'b1010_0110;
                u[2] = 8'b1010_0110;
                i = 2;
                for (j = -2; j < 8; j += 3)
                    $display("read %0d %b %b %b %b", j, packed_q[i].data[j +: 4],
                        unpacked_q[i].data[j +: 4], a[i][j +: 4], u[i][j +: 4]);
                // An element past the array.
                i = 5;
                j = 0;
                $display("read past %b %b %b %b", packed_q[i].data[j +: 4],
                    unpacked_q[i].data[j +: 4], a[i][j +: 4], u[i][j +: 4]);
                // Blocking writes: inside, hanging over the top and the
                // bottom, and in an element past the array.
                clear();
                i = 1;
                j = 2;
                packed_q[i].data[j +: 4] = 4'hf; unpacked_q[i].data[j +: 4] = 4'hf;
                a[i][j +: 4] = 4'hf; u[i][j +: 4] = 4'hf;
                j = 6;
                packed_q[i].data[j +: 4] = 4'hf; unpacked_q[i].data[j +: 4] = 4'hf;
                a[i][j +: 4] = 4'hf; u[i][j +: 4] = 4'hf;
                i = 2;
                j = -2;
                packed_q[i].data[j +: 4] = 4'hf; unpacked_q[i].data[j +: 4] = 4'hf;
                a[i][j +: 4] = 4'hf; u[i][j +: 4] = 4'hf;
                i = 7;
                j = 0;
                packed_q[i].data[j +: 4] = 4'hf; unpacked_q[i].data[j +: 4] = 4'hf;
                a[i][j +: 4] = 4'hf; u[i][j +: 4] = 4'hf;
                show("blocking");
                // The same writes as nonblocking assignments, one per clock.
                clear();
                for (int step = 0; step < 4; step++) begin
                    case (step)
                        0: begin i = 1; j = 2; end
                        1: begin i = 1; j = 6; end
                        2: begin i = 2; j = -2; end
                        3: begin i = 7; j = 0; end
                    endcase
                    #1 clk = 1;
                    #1 clk = 0;
                end
                show("nonblocking");
            end
            always @(posedge clk) begin
                packed_q[i].data[j +: 4] <= 4'hf;
                unpacked_q[i].data[j +: 4] <= 4'hf;
                a[i][j +: 4] <= 4'hf;
                u[i][j +: 4] <= 4'hf;
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let x = |bits: &str| {
            if four_state {
                bits.to_string()
            } else {
                bits.replace('x', "0")
            }
        };
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("windows.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        sim.run_until(100).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        // data = 1010_0110: bits [1:-2], [4:1], [7:4] and [10:7].
        let read = |j: i32, bits: &str| {
            let bits = x(bits);
            format!("read {j} {bits} {bits} {bits} {bits}")
        };
        let past = x("xxxx");
        // Element 1 gets data[5:2] and then data[7:6] (0xfc), element 2
        // data[1:0] (0x03): an entry is tag:data:low, 16 bits; a byte
        // element is just the data.
        let blocking = "000000300fc00000 0000 0fc0 0030 0000 0003fc00 00 fc 03 00";
        assert_eq!(
            displayed,
            [
                read(-2, "10xx"),
                read(1, "0011"),
                read(4, "1010"),
                read(7, "xxx1"),
                format!("read past {past} {past} {past} {past}"),
                format!("blocking {blocking}"),
                format!("nonblocking {blocking}"),
            ],
            "four_state={four_state}"
        );
    }
}

// The same in combinational processes, whose writes and reads at run-time
// positions within a run-time element stay within the element.
#[test]
fn run_time_part_selects_stay_in_their_array_element_in_combinational_processes() {
    let source = r#"
        `timescale 1ns/1ns
        module Top;
            typedef struct packed { logic [3:0] tag; logic [7:0] data; logic [3:0] low; } entry_t;
            entry_t unpacked_q [4];
            logic [7:0] u [4];
            logic [3:0][7:0] a;
            logic [3:0] r_u, r_q, r_a;
            int i, j;
            always_comb begin
                u = '{default: '1};
                u[i][j +: 4] = 4'h0;
                r_u = u[i][j +: 4];
            end
            always_comb begin
                foreach (unpacked_q[e]) unpacked_q[e] = '1;
                unpacked_q[i].data[j +: 4] = 4'h0;
                r_q = unpacked_q[i].data[j +: 4];
            end
            always_comb begin
                a = '1;
                a[i][j +: 4] = 4'h0;
                r_a = a[i][j +: 4];
            end
            initial begin
                i = 1; j = 6;
                #1 $display("%h %h %h %h %h %b %b %b", u[1], u[2], unpacked_q[1], unpacked_q[2], a,
                    r_u, r_q, r_a);
                i = 9;
                #1 $display("%h %h %h %h %h %b %b %b", u[1], u[2], unpacked_q[1], unpacked_q[2], a,
                    r_u, r_q, r_a);
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let x = |bits: &str| {
            if four_state {
                bits.to_string()
            } else {
                bits.replace('x', "0")
            }
        };
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("comb_windows.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        sim.run_until(10).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            displayed,
            [
                // Bits 7:6 of element 1 are cleared, bits 9:8 are past it.
                format!("3f ff f3ff ffff ffff3fff {}", x("xx00 xx00 xx00")),
                // Element 9 is past the arrays: nothing is written.
                format!("ff ff ffff ffff ffffffff {}", x("xxxx xxxx xxxx")),
            ],
            "four_state={four_state}"
        );
    }
}

// Nonblocking assignments of a process with timing controls to run-time
// part-selects within one element of an unpacked array at a run-time index
// each write their bits, and one to an element outside the array writes
// nothing (IEEE 1800-2023 7.4.5, 10.4.2, 11.5.1).
#[test]
fn nonblocking_run_time_part_selects_of_unpacked_elements_all_land() {
    let source = r#"
        `timescale 1ns/1ns
        module Top;
            typedef struct packed { logic [3:0] tag; logic [15:0] data; } entry_t;
            logic [15:0] mem [4];
            entry_t q [4];
            int a, j0, j1;
            initial begin
                foreach (mem[e]) mem[e] = '0;
                foreach (q[e]) q[e] = '0;
                a = $urandom_range(1, 1);
                j0 = $urandom_range(0, 0);
                j1 = $urandom_range(8, 8);
                #1;
                mem[a][j0 +: 8] <= 8'h34;
                mem[a][j1 +: 8] <= 8'h12;
                q[a].data[j0 +: 8] <= 8'h34;
                q[a].data[j1 +: 8] <= 8'h12;
                a = 7;
                mem[a][j0 +: 8] <= 8'hff;
                #1 $display("%h %h %h %h", mem[1], q[1], mem[0], mem[3]);
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("nba_elements.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        sim.run_until(10).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            displayed,
            ["1234 01234 0000 0000"],
            "four_state={four_state}"
        );
    }
}

// The run-time part-selects of an interface's functions stay in the array
// element their run-time index selects, whose position names the
// interface's members, called through a port and read on the instance.
#[test]
fn run_time_part_selects_of_interface_functions_stay_in_their_array_element() {
    let source = r#"
        `timescale 1ns/1ns
        interface Bus;
            bit [7:0] q [4];
            int idx = 1;
            function automatic void put(int j);
                q[idx][j +: 4] = 4'hf;
            endfunction
            function automatic logic [3:0] get(int j);
                return q[idx][j +: 4];
            endfunction
        endinterface
        module User(Bus b);
            initial begin
                #1 b.put(4);
                #1 b.put(2);
                #1 $display("%h %h %h", b.q[0], b.q[1], b.get(2));
            end
        endmodule
        module Top;
            Bus b();
            User u(b);
            initial #4 $display("%h %h %h", b.q[0], b.q[1], b.get(4));
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("interface_windows.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build_cranelift()
        .unwrap();
        sim.run_until(10).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        assert_eq!(displayed, ["00 fc f"; 2], "four_state={four_state}");
    }
}

// The run-time part-selects of an element at a run-time index that loops
// write, whose index and position name the loop variables, stay in the
// element, as a byte-enabled SRAM writes `sram[addr_i[i]][j*8 +: 8]`.
#[test]
fn looped_run_time_part_selects_stay_in_their_array_element() {
    let source = r#"
        `timescale 1ns/1ns
        module Top #(parameter int NumPorts = 2, parameter int BeWidth = 4) ();
            logic clk = 0;
            logic [NumPorts-1:0] req = '1;
            logic [NumPorts-1:0][BeWidth-1:0] be = '{4'b0101, 4'b1010};
            logic [NumPorts-1:0][3:0] addr = '{4'd5, 4'd9};
            logic [NumPorts-1:0][31:0] wdata = '{32'h1122_3344, 32'haabb_ccdd};
            logic [31:0] sram [16];
            initial foreach (sram[k]) sram[k] = '0;
            always_ff @(posedge clk) begin
                for (int unsigned i = 0; i < NumPorts; i++) begin
                    if (req[i]) begin
                        for (int unsigned j = 0; j < BeWidth; j++) begin
                            if (be[i][j]) begin
                                sram[addr[i]][j*8 +: 8] <= wdata[i][j*8 +: 8];
                            end
                        end
                    end
                end
            end
            initial begin
                #1 clk = 1;
                #1 $display("%h %h %h", sram[5], sram[9], sram[0]);
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("byte_enables.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        sim.run_until(10).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        // Port 1 (`addr[1] = 5`, `be[1] = 0101`) writes bytes 0 and 2 of
        // 1122_3344; port 0 (`addr[0] = 9`, `be[0] = 1010`) bytes 1 and 3 of
        // aabb_ccdd.
        assert_eq!(
            displayed,
            ["00220044 aa00cc00 00000000"],
            "four_state={four_state}"
        );
    }
}

// A part-select of an element at a run-time index writes only that
// element's bits, also when the select is wider than the element, when the
// element is a packed array selected at a run-time index, when the element
// is reached through a `ref` subarray, when the index calls a function (which
// runs once, for a read as for a write), and when an earlier part of a concatenation target assigns the index (the
// element and its bits are then selected by the same index value, the one
// the target's indices are evaluated with); the bits a read of a two-state
// array selects outside its element read 0 (IEEE 1800-2023 7.4.6, 11.5.1).
#[test]
fn run_time_part_selects_of_array_elements_write_only_their_element_bits() {
    let cases = [
        (
            "wide",
            r#"
            `timescale 1ns/1ns
            module Top;
                logic [7:0] mem [16];
                int i = 3, lo = -75;
                initial begin
                    foreach (mem[k]) mem[k] = '0;
                    mem[i][lo +: 80] = {5'b10110, 75'd0};
                    #1 $display("%h %h %h", mem[3], mem[2], mem[4]);
                end
            endmodule
            "#,
            "16 00 00",
        ),
        (
            "packed_element",
            r#"
            `timescale 1ns/1ns
            module Top;
                logic [3:0][7:0] mem [4];
                int i = 2, p = 1, j = 2;
                initial begin
                    foreach (mem[k]) mem[k] = '0;
                    mem[i][p][j +: 4] = 4'hf;
                    #1 $display("%h %h", mem[2], mem[1]);
                end
            endmodule
            "#,
            "00003c00 00000000",
        ),
        (
            "ref_subarray",
            r#"
            `timescale 1ns/1ns
            module Top;
                logic [7:0] mem [3][2];
                int j = 1, k = 1, m = 2;
                task automatic t(ref logic [7:0] r [2]);
                    r[k][m +: 4] = 4'hf;
                endtask
                initial begin
                    foreach (mem[a, b]) mem[a][b] = '0;
                    t(mem[j]);
                    #1 $display("%h %h %h %h", mem[1][1], mem[1][0], mem[0][1], mem[2][1]);
                end
            endmodule
            "#,
            "3c 00 00 00",
        ),
        (
            "called_index",
            r#"
            `timescale 1ns/1ns
            module Top;
                logic [7:0] mem [4];
                int n = 0, j = 4;
                function automatic int next();
                    n = n + 1;
                    return n;
                endfunction
                initial begin
                    foreach (mem[k]) mem[k] = '0;
                    mem[next()][j +: 4] = 4'ha;
                    #1 $display("%h %h %0d", mem[1], mem[2], n);
                end
            endmodule
            "#,
            "a0 00 1",
        ),
        (
            "called_index_read",
            r#"
            `timescale 1ns/1ns
            module Top;
                logic [7:0] mem [4];
                logic [3:0] x;
                int n = 0, j = 4;
                function automatic int next();
                    n = n + 1;
                    return n;
                endfunction
                initial begin
                    foreach (mem[k]) mem[k] = '0;
                    mem[1] = 8'ha5;
                    mem[2] = 8'h3c;
                    x = mem[next()][j +: 4];
                    #1 $display("%h %0d", x, n);
                end
            endmodule
            "#,
            "a 1",
        ),
        (
            "called_index_comb",
            r#"
            `timescale 1ns/1ns
            module Top;
                logic [7:0] q [4];
                logic [1:0] sel = 2;
                int j = 4;
                function automatic int pick(input logic [1:0] s);
                    return s;
                endfunction
                always_comb begin
                    foreach (q[k]) q[k] = '0;
                    q[pick(sel)][j +: 4] = 4'h5;
                end
                initial #1 $display("%h %h", q[2], q[1]);
            endmodule
            "#,
            "50 00",
        ),
        (
            "concatenation",
            r#"
            `timescale 1ns/1ns
            module Top;
                logic [7:0] mem [4];
                int i = 2, j = 4;
                initial begin
                    foreach (mem[k]) mem[k] = '0;
                    {i, mem[i][j +: 4]} = {32'd1, 4'hf};
                    #1 $display("%h %h %0d", mem[2], mem[1], i);
                end
            endmodule
            "#,
            "f0 00 1",
        ),
        (
            "concatenation_comb",
            r#"
            `timescale 1ns/1ns
            module Top;
                logic [7:0] q [4];
                int idx, j = 4;
                always_comb begin
                    foreach (q[k]) q[k] = '0;
                    idx = 2;
                    {idx, q[idx][j +: 4]} = {32'd1, 4'h5};
                end
                initial #1 $display("%h %h", q[2], q[1]);
            endmodule
            "#,
            "50 00",
        ),
        (
            "two_state_overhang_read",
            r#"
            `timescale 1ns/1ns
            module Top;
                bit [7:0] mem [4];
                int i = 1, j = 6;
                logic [3:0] x;
                initial begin
                    foreach (mem[k]) mem[k] = '1;
                    x = mem[i][j +: 4];
                    #1 $display("%b", x);
                end
            endmodule
            "#,
            "0011",
        ),
    ];
    for (name, source, expected) in cases {
        for four_state in [false, true] {
            let mut sim =
                celox::Simulation::from_sv_sources(vec![(source, Path::new("select.sv"))], "Top")
                    .four_state(four_state)
                    .build_cranelift()
                    .unwrap_or_else(|error| panic!("{name}: {error}"));
            sim.run_until(10).unwrap();
            let displayed: Vec<_> = sim
                .drain_runtime_events()
                .into_iter()
                .filter_map(|event| match event {
                    celox::RuntimeEvent::Display { message } => {
                        Some(String::from_utf8(message).unwrap())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(displayed, [expected], "{name} four_state={four_state}");
        }
    }
}

// A run-time part-select of a member of an element of a packed array that is
// itself a member of a packed structure, `s.regs[i].q[j +: 4]`, is rejected:
// the select would not stay within `q` (IEEE 1800-2023 11.5.1).
#[test]
fn run_time_part_selects_of_members_of_elements_of_packed_array_members_are_rejected() {
    let source = r#"
        module Top;
            typedef struct packed { logic [3:0] q; logic [3:0] r; } reg_t;
            typedef struct packed { logic [7:0] other; reg_t [1:0] regs; } s_t;
            s_t s;
            int i = 1, j = 2;
            initial s.regs[i].q[j +: 4] = 4'hf;
        endmodule
    "#;
    let error =
        match celox::Simulation::from_sv_sources(vec![(source, Path::new("nested.sv"))], "Top")
            .build_cranelift()
        {
            Ok(_) => panic!("the select is rejected"),
            Err(error) => error.to_string(),
        };
    assert!(
        error.contains(
            "run-time part-select of member `q` of an element of packed array member `s.regs`"
        ),
        "{error}"
    );
}

/// Selects of packed arrays of structures at run-time indices in a
/// combinational process, as Ariane's scoreboard makes them: element members
/// written and read at the index of a small selector, which may lie past the
/// array (6 elements, 3-bit indices), wrap (`p + 3'd1`), be signed, count
/// in an ascending array, or be too wide to take each value of (`w`). The
/// arrays are wide enough (768 bits) for a choice of the constant selects at
/// each index to cost less than shifting them.
const SCOREBOARD_SELECTS: &str = r#"
    module Top(input logic [2:0] p, input bit [2:0] q, input logic signed [2:0] s,
               input logic [7:0] w, input logic [7:0] v,
               output logic [767:0] ym, output logic [767:0] yn,
               output logic [7:0] r0, output logic [7:0] r1, output logic [3:0] r2,
               output logic [7:0] r3, output logic [3:0] r4);
        typedef struct packed { logic [3:0] a; logic [7:0] b; logic [115:0] pad; } e_t;
        e_t [5:0] m;
        e_t [0:5] n;
        always_comb begin
            m = '0;
            n = '0;
            m[p].b = v;
            m[p + 3'd1].a = v[3:0];
            n[q].b = ~v;
            m[s + 4].a = v[7:4];
            m[w].b = m[w].b ^ 8'h0f;
            r0 = m[q].b;
            r1 = n[p].b;
            r2 = m[s + 3].a;
            r3 = m[w].b;
            r4 = m[s + 4].a;
        end
        assign ym = m;
        assign yn = n;
    endmodule
"#;

/// What [`SCOREBOARD_SELECTS`] computes from its inputs: the arrays as
/// element `(a, b)` pairs by index, and each read, `None` for one past the
/// array, which reads X in a four-state simulation and 0 in a two-state one.
#[allow(clippy::type_complexity)]
fn scoreboard_model(
    p: u8,
    q: u8,
    s: i8,
    w: u8,
    v: u8,
) -> ([(u8, u8); 6], [(u8, u8); 6], [Option<u8>; 5]) {
    let mut m = [(0u8, 0u8); 6];
    let mut n = [(0u8, 0u8); 6];
    let element = |index: i32| usize::try_from(index).ok().filter(|index| *index < 6);
    if let Some(i) = element(p.into()) {
        m[i].1 = v;
    }
    if let Some(i) = element(((p + 1) % 8).into()) {
        m[i].0 = v & 0xf;
    }
    if let Some(i) = element(q.into()) {
        n[i].1 = !v;
    }
    if let Some(i) = element(i32::from(s) + 4) {
        m[i].0 = v >> 4;
    }
    if let Some(i) = element(w.into()) {
        m[i].1 ^= 0x0f;
    }
    let reads = [
        element(q.into()).map(|i| m[i].1),
        element(p.into()).map(|i| n[i].1),
        element(i32::from(s) + 3).map(|i| m[i].0),
        element(w.into()).map(|i| m[i].1),
        element(i32::from(s) + 4).map(|i| m[i].0),
    ];
    (m, n, reads)
}

fn check_scoreboard_selects<B: celox::SimBackend>(sim: &mut celox::Simulator<B>, four_state: bool) {
    let names = ["p", "q", "s", "w", "v"].map(|name| sim.signal(name));
    let reads = [("r0", 8), ("r1", 8), ("r2", 4), ("r3", 8), ("r4", 4)];
    let packed = |elements: &[(u8, u8); 6], ascending: bool| {
        let mut bits = BigUint::from(0u8);
        for (index, (a, b)) in elements.iter().enumerate() {
            let at = if ascending { 5 - index } else { index };
            let element = (u64::from(*a & 0xf) << 8) | u64::from(*b);
            bits |= BigUint::from(element) << (128 * at + 116);
        }
        bits
    };
    for i in 0u8..96 {
        let (p, q) = (i % 8, (i / 8) % 8);
        let s = i8::try_from(i.wrapping_mul(3) % 8).unwrap() - 4;
        let w = [0u8, 3, 5, 6, 200][usize::from(i % 5)];
        let v = 0xa5u8 ^ i.wrapping_mul(7);
        let inputs = [p, q, (s as u8) & 7, w, v];
        sim.modify(|io| {
            for (signal, value) in names.iter().zip(inputs) {
                io.set(*signal, value);
            }
        })
        .unwrap();
        let (m, n, expected) = scoreboard_model(p, q, s, w, v);
        let case = format!("p={p} q={q} s={s} w={w} v={v:#x}");
        let (ym, ym_mask) = sim.get_four_state(sim.signal("ym"));
        assert_eq!(
            (ym, ym_mask),
            (packed(&m, false), BigUint::from(0u8)),
            "ym {case}"
        );
        let (yn, yn_mask) = sim.get_four_state(sim.signal("yn"));
        assert_eq!(
            (yn, yn_mask),
            (packed(&n, true), BigUint::from(0u8)),
            "yn {case}"
        );
        for ((name, width), expected) in reads.into_iter().zip(expected) {
            let (value, mask) = sim.get_four_state(sim.signal(name));
            let all = BigUint::from((1u16 << width) - 1);
            // The known bits and the unknown ones; an X bit's value bit is
            // the backend's.
            let known = value & (&all ^ &mask);
            let expected = match expected {
                Some(value) => (BigUint::from(value), BigUint::from(0u8)),
                None if four_state => (BigUint::from(0u8), all),
                None => (BigUint::from(0u8), BigUint::from(0u8)),
            };
            assert_eq!((known, mask), expected, "{name} {case}");
        }
    }
}

sv_backends! {
    fn scoreboard_selects_two_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(
            vec![(SCOREBOARD_SELECTS, Path::new("scoreboard.sv"))],
            "Top",
        ).four_state(false);
        check_scoreboard_selects(&mut sim, false);
    }

    fn scoreboard_selects_four_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(
            vec![(SCOREBOARD_SELECTS, Path::new("scoreboard.sv"))],
            "Top",
        ).four_state(true);
        check_scoreboard_selects(&mut sim, true);
    }
}

/// A part-select of a structure member at a small run-time index writes only
/// the bits of the member: the bits past it are out of range (IEEE 1800-2023
/// 11.5.1), and the neighbouring member keeps its value. The structure is
/// wide enough for the select to be taken at each index.
const MEMBER_PART_SELECT: &str = r#"
    module Top(input bit [2:0] i, input logic [3:0] v,
               output logic [3:0] hi, output logic [7:0] f, output logic [3:0] lo);
        typedef struct packed {
            logic [3:0] hi; logic [7:0] f; logic [3:0] lo; logic [255:0] pad;
        } s_t;
        s_t s;
        always_comb begin
            s = '0;
            s.hi = 4'h5;
            s.lo = 4'ha;
            s.f[i +: 4] = v;
        end
        assign hi = s.hi;
        assign f = s.f;
        assign lo = s.lo;
    endmodule
"#;

fn check_member_part_select<B: celox::SimBackend>(sim: &mut celox::Simulator<B>) {
    let (i, v) = (sim.signal("i"), sim.signal("v"));
    for index in 0u8..8 {
        sim.modify(|io| {
            io.set(i, index);
            io.set(v, 0xfu8);
        })
        .unwrap();
        let f = (0xfu16 << index) as u8;
        assert_eq!(
            sim.get(sim.signal("hi")),
            BigUint::from(5u8),
            "hi at i={index}"
        );
        assert_eq!(sim.get(sim.signal("f")), BigUint::from(f), "f at i={index}");
        assert_eq!(
            sim.get(sim.signal("lo")),
            BigUint::from(0xau8),
            "lo at i={index}"
        );
    }
}

sv_backends! {
    fn member_part_selects_stay_in_the_member_two_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(
            vec![(MEMBER_PART_SELECT, Path::new("member.sv"))],
            "Top",
        ).four_state(false);
        check_member_part_select(&mut sim);
    }

    fn member_part_selects_stay_in_the_member_four_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(
            vec![(MEMBER_PART_SELECT, Path::new("member.sv"))],
            "Top",
        ).four_state(true);
        check_member_part_select(&mut sim);
    }
}
