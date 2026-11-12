use super::*;

#[path = "packed_structs/port_connections.rs"]
mod port_connections;

/// Members of packed structure elements of unpacked arrays, such as
/// `m[i].y`, selected with run-time indices: the index selects take an
/// element and the member selects then select from it (IEEE 1800-2023
/// 7.4.5, 7.2.1). An index outside the array, or with unknown bits, reads
/// the default value and writes nothing (7.4.5).
const ELEMENT_MEMBERS: &str = r#"
    module Top(input logic clk, rst, we,
               input logic [1:0] ri, wi, ci,
               input logic [2:0] wide,
               input logic [7:0] d,
               output logic [7:0] r, ro,
               output logic [3:0] ny,
               output logic [15:0] e0, e1, e2, e3, c0, c1, c2, c3);
        typedef struct packed { logic [7:0] x; logic [7:0] y; } e_t;
        typedef struct packed { logic [3:0] x; logic [3:0] y; } f_t;
        typedef struct packed { logic [1:0] tag; f_t e; } n_t;
        e_t m [4];
        n_t n [2][3];
        e_t c [4];
        always_ff @(posedge clk)
            if (rst) begin
                for (int k = 0; k < 4; k++) m[k] <= '{x: 8'(k), y: 8'(8'h10 + k)};
                for (int a = 0; a < 2; a++)
                    for (int b = 0; b < 3; b++)
                        n[a][b] <= '{tag: 2'(a), e: '{x: 4'(b), y: 4'(3 * a + b + 1)}};
            end else if (we) begin
                m[wi].x <= d;
                m[wi].y[3:0] <= d[7:4];
                n[wi[0]][wi].e.y <= d[3:0];
            end
        always_comb begin
            for (int k = 0; k < 4; k++) c[k] = m[k];
            c[ci].x = ~d;
            c[ci].y[7:4] = 4'h9;
        end
        assign r = m[ri].y;
        assign ro = m[wide].x;
        assign ny = n[ri[0]][ri].e.y;
        assign e0 = m[0];
        assign e1 = m[1];
        assign e2 = m[2];
        assign e3 = m[3];
        assign c0 = c[0];
        assign c1 = c[1];
        assign c2 = c[2];
        assign c3 = c[3];
    endmodule
"#;

fn element_members<B: celox::SimBackend>(sim: &mut celox::Simulator<B>, four_state: bool) {
    let read = |sim: &mut celox::Simulator<B>, name: &str| {
        let signal = sim.signal(name);
        let (value, mask) = sim.get_four_state(signal);
        let digit = |number: BigUint| number.iter_u64_digits().next().unwrap_or(0);
        (digit(value), digit(mask))
    };
    let set = |sim: &mut celox::Simulator<B>, name: &str, value: u64, mask: u64| {
        let signal = sim.signal(name);
        sim.set_four_state(signal, value.into(), mask.into());
    };
    let clk = sim.event("clk");
    // The value of each element of `m`, and `n[a][b].e.y` by `3 * a + b`.
    let mut m: [u64; 4] = std::array::from_fn(|k| (k as u64) << 8 | (0x10 + k as u64));
    let mut ny: [u64; 6] = std::array::from_fn(|k| k as u64 + 1);
    // A read outside the array or at an unknown index is the default value.
    let unknown = if four_state { (0xff, 0xff) } else { (0, 0) };
    let check = |sim: &mut celox::Simulator<B>, m: &[u64; 4], ny: &[u64; 6], ci: u64, d: u64| {
        for (k, value) in m.iter().enumerate() {
            assert_eq!(read(sim, &format!("e{k}")), (*value, 0), "m[{k}]");
            let changed = if k as u64 == ci {
                (!d & 0xff) << 8 | 0x90 | (value & 0x0f)
            } else {
                *value
            };
            assert_eq!(read(sim, &format!("c{k}")), (changed, 0), "c[{k}]");
        }
        for ri in 0..4u64 {
            set(sim, "ri", ri, 0);
            assert_eq!(read(sim, "r"), (m[ri as usize] & 0xff, 0), "m[{ri}].y");
            let expected = if ri < 3 {
                (ny[(3 * (ri & 1) + ri) as usize], 0)
            } else {
                (unknown.0 & 0xf, unknown.1 & 0xf)
            };
            let (value, mask) = read(sim, "ny");
            assert_eq!(
                (value & !mask, mask),
                (expected.0 & !expected.1, expected.1),
                "n[{ri}].e.y"
            );
        }
        for wide in 0..8u64 {
            set(sim, "wide", wide, 0);
            let (value, mask) = read(sim, "ro");
            let expected = if wide < 4 {
                (m[wide as usize] >> 8, 0)
            } else {
                unknown
            };
            assert_eq!(
                (value & !mask, mask),
                (expected.0 & !expected.1, expected.1),
                "m[{wide}].x"
            );
        }
    };
    for (name, value) in [
        ("rst", 1),
        ("we", 0),
        ("ri", 0),
        ("wi", 0),
        ("ci", 0),
        ("wide", 0),
        ("d", 0),
    ] {
        set(sim, name, value, 0);
    }
    sim.tick(clk).unwrap();
    set(sim, "rst", 0, 0);
    check(sim, &m, &ny, 0, 0);

    set(sim, "we", 1, 0);
    for (wi, d) in [(1u64, 0xa5u64), (3, 0x5c), (0, 0x3c)] {
        set(sim, "wi", wi, 0);
        set(sim, "d", d, 0);
        sim.tick(clk).unwrap();
        let element = &mut m[wi as usize];
        *element = d << 8 | (*element & 0xf0) | d >> 4;
        if wi < 3 {
            ny[(3 * (wi & 1) + wi) as usize] = d & 0xf;
        }
        for ci in 0..4 {
            set(sim, "ci", ci, 0);
            check(sim, &m, &ny, ci, d);
        }
    }
    if four_state {
        // An unknown index writes nothing and reads x.
        set(sim, "wi", 0, 3);
        set(sim, "d", 0x77, 0);
        sim.tick(clk).unwrap();
        set(sim, "ci", 0, 3);
        check(sim, &m, &ny, 4, 0x77);
        set(sim, "ri", 0, 3);
        assert_eq!(read(sim, "r").1, 0xff);
        assert_eq!(read(sim, "ny").1, 0xf);
    }
}

sv_backends! {
    fn packed_struct_members_of_unpacked_array_elements(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(ELEMENT_MEMBERS, Path::new("element_members.sv"))], "Top");
        element_members(&mut sim, false);
    }

    fn packed_struct_members_of_unpacked_array_elements_four_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(ELEMENT_MEMBERS, Path::new("element_members.sv"))], "Top").four_state(true);
        element_members(&mut sim, true);
    }
}

sv_backends! {
    /// A parameter table of packed structures whose values name
    /// enumeration members (IEEE 1800-2023 6.19, 6.20.2), read through a
    /// package function that searches it, as Veryl emits for decode tables.
    fn packed_struct_parameter_tables_name_enumeration_members(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(r#"
            package p;
                typedef enum logic [3:0] { NONE, LOAD, STORE, JUMP } kind_t;
                typedef struct packed { logic [7:0] opcode; kind_t kind; } entry_t;
                localparam entry_t TABLE [3] = '{
                    entry_t'{opcode: 8'h10, kind: LOAD},
                    entry_t'{opcode: 8'h20, kind: STORE},
                    entry_t'{opcode: 8'h30, kind: JUMP}
                };
                function automatic entry_t decode(input logic [7:0] opcode);
                    for (int i = 0; i < $size(TABLE); i++)
                        if (TABLE[i].opcode == opcode) return TABLE[i];
                    return entry_t'{opcode: opcode, kind: NONE};
                endfunction
            endpackage
            module Top(input logic clk, input logic [7:0] opcode, input logic [1:0] index,
                       output logic [3:0] kind, selected, local_kind, output logic [11:0] packed_entry);
                typedef enum logic [1:0] { A, B, C } local_t;
                localparam p::entry_t ENTRY = '{opcode: 8'h40, kind: p::JUMP};
                localparam local_t LOCAL [2] = '{C, B};
                always_ff @(posedge clk) begin
                    p::entry_t entry;
                    entry = p::decode(opcode);
                    kind <= entry.kind;
                end
                assign selected = p::TABLE[index].kind;
                assign local_kind = 4'(LOCAL[index[0]]);
                assign packed_entry = ENTRY;
            endmodule
        "#, Path::new("enum_tables.sv"))], "Top");
        let clk = sim.event("clk");
        let opcode = sim.signal("opcode");
        let index = sim.signal("index");
        let kind = sim.signal("kind");
        let selected = sim.signal("selected");
        let local_kind = sim.signal("local_kind");
        let packed_entry = sim.signal("packed_entry");
        for (value, expected) in [(0x10u8, 1u8), (0x20, 2), (0x30, 3), (0x44, 0)] {
            sim.set(opcode, value);
            sim.tick(clk).unwrap();
            assert_eq!(sim.get(kind), expected.into(), "decode({value:#x})");
        }
        for (value, expected, local) in [(0u8, 1u8, 2u8), (1, 2, 1), (2, 3, 2)] {
            sim.set(index, value);
            assert_eq!(sim.get(selected), expected.into(), "TABLE[{value}].kind");
            assert_eq!(sim.get(local_kind), local.into(), "LOCAL[{value}]");
        }
        assert_eq!(sim.get(packed_entry), 0x403u32.into());
    }
}

/// Members of the elements of packed array members of a packed structure,
/// such as `s.arr[0].f` (IEEE 1800-2023 7.2.1, 7.4.1), read and written,
/// of descending, ascending and multidimensional arrays.
const ARRAY_MEMBER_ELEMENTS: &str = r#"
    module Top(input logic clk, input logic [7:0] d,
               output logic [3:0] f0, g1, h, a3, output logic [31:0] tw, uw);
        typedef struct packed { logic [3:0] f; logic [3:0] g; } in_t;
        typedef struct packed { in_t [1:0] arr; in_t [0:1] asc; } s_t;
        typedef struct packed { in_t [1:0][1:0] grid; } m_t;
        s_t s;
        m_t m;
        assign s = 32'h1234_5678;
        assign m = 32'h89ab_cdef;
        assign f0 = s.arr[0].f;
        assign g1 = s.arr[1].g;
        assign h = s.asc[0].g;
        assign a3 = m.grid[1][0].f;
        s_t t, u;
        always_comb begin
            t = '0;
            t.arr[1].f = d[3:0];
            t.asc[1].g = d[7:4];
        end
        always_ff @(posedge clk) begin
            u.arr[0].g <= d[3:0];
            u.asc[0].f <= d[7:4];
        end
        initial u = '0;
        assign tw = t;
        assign uw = u;
    endmodule
"#;

fn array_member_elements<B: celox::SimBackend>(sim: &mut celox::Simulator<B>) {
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let f0 = sim.signal("f0");
    let g1 = sim.signal("g1");
    let h = sim.signal("h");
    let a3 = sim.signal("a3");
    let tw = sim.signal("tw");
    let uw = sim.signal("uw");
    assert_eq!(sim.get(f0), 0x3u8.into(), "s.arr[0].f");
    assert_eq!(sim.get(g1), 0x2u8.into(), "s.arr[1].g");
    assert_eq!(sim.get(h), 0x6u8.into(), "s.asc[0].g");
    assert_eq!(sim.get(a3), 0xau8.into(), "m.grid[1][0].f");
    sim.set(d, 0xc5u8);
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(tw), 0x5000_000cu32.into(), "t.arr[1].f, t.asc[1].g");
    assert_eq!(sim.get(uw), 0x0005_c000u32.into(), "u.arr[0].g, u.asc[0].f");
}

sv_backends! {
    fn packed_struct_members_of_packed_array_member_elements(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(ARRAY_MEMBER_ELEMENTS, Path::new("array_member_elements.sv"))], "Top");
        array_member_elements(&mut sim);
    }

    fn packed_struct_members_of_packed_array_member_elements_four_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(ARRAY_MEMBER_ELEMENTS, Path::new("array_member_elements.sv"))], "Top").four_state(true);
        array_member_elements(&mut sim);
    }
}

/// The known bits and the mask of signal `name`.
fn known_and_mask<B: celox::SimBackend>(sim: &mut celox::Simulator<B>, name: &str) -> (u64, u64) {
    let signal = sim.signal(name);
    let (value, mask) = sim.get_four_state(signal);
    let digit = |number: BigUint| number.iter_u64_digits().next().unwrap_or(0);
    let (value, mask) = (digit(value), digit(mask));
    (value & !mask, mask)
}

sv_backends! {
    /// A two-state member of a four-state structure is converted to two
    /// states (IEEE 1800-2023 7.2.1): when it is read from the default value
    /// of an element outside the array (7.4.5), and when it is written at a
    /// run-time index.
    fn packed_struct_two_state_members_of_invalid_elements(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(r#"
            module Top(input logic [1:0] index, input logic wi, output logic [3:0] a, b, c, d,
                       output logic [7:0] w0, w1);
                typedef struct packed { logic [3:0] a; bit [3:0] b; } e_t;
                e_t m [2];
                assign m[0] = '{a: 4'h1, b: 4'h2};
                assign m[1] = '{a: 4'h3, b: 4'h4};
                assign a = m[2].a;
                assign b = m[2].b;
                assign c = m[index].a;
                assign d = m[index].b;
                e_t w [2];
                always_comb begin
                    w[0] = '{a: 4'h5, b: 4'h6};
                    w[1] = w[0];
                    w[wi].b = 4'bxz10;
                end
                assign w0 = w[0];
                assign w1 = w[1];
            endmodule
        "#, Path::new("invalid_element_members.sv"))], "Top").four_state(true);
        let index = sim.signal("index");
        assert_eq!(known_and_mask(&mut sim, "a").1, 0xf);
        assert_eq!(known_and_mask(&mut sim, "b"), (0, 0));
        for (value, c, d) in [(1u8, (3, 0), (4, 0)), (3, (0, 0xf), (0, 0))] {
            sim.set(index, value);
            assert_eq!(known_and_mask(&mut sim, "c"), c, "m[{value}].a");
            assert_eq!(known_and_mask(&mut sim, "d"), d, "m[{value}].b");
        }
        // A write to the two-state member stores two-state bits.
        let wi = sim.signal("wi");
        for (value, w0, w1) in [(0u8, 0x52u64, 0x56u64), (1, 0x56, 0x52)] {
            sim.set(wi, value);
            assert_eq!(known_and_mask(&mut sim, "w0"), (w0, 0), "w[0] after w[{value}].b");
            assert_eq!(known_and_mask(&mut sim, "w1"), (w1, 0), "w[1] after w[{value}].b");
        }
    }
}

// The second and third operands of a conditional are in the assignment-like
// context of the conditional itself (IEEE 1800-2023 10.8), so an assignment
// pattern there takes the type of the target, also of a selected element.
sv_backends! {
    fn conditional_patterns_take_the_type_of_a_selected_target(sim) {
        @setup {
            let source = r#"
    package pad_pkg;
      typedef struct packed { logic [1:0] drive; logic schmitt; logic pull; } attr_t;
    endpackage
    module Top(
      input  logic sel,
      input  pad_pkg::attr_t [3:0] core,
      output pad_pkg::attr_t [3:0] pad,
      output pad_pkg::attr_t [1:0] staged,
      output pad_pkg::attr_t nested
    );
      for (genvar k = 0; k < 4; k++) begin : gen
        if (k == 1) begin : schmitt
          assign pad[k] = (sel) ? '{schmitt: 1'b1, default: '0} : core[k];
        end else begin : plain
          assign pad[k] = sel ? '0 : core[k];
        end
      end
      always_comb begin
        staged[0] = sel ? core[0] : '{drive: 2'b10, default: '1};
        staged[1] = '{default: '0};
        if (!sel) staged[1] = (sel ? '{default: '1} : ('{pull: 1'b1, default: '0}));
      end
      assign nested = sel ? (core[3].pull ? '{default: '1} : '{drive: 2'b01, default: '0}) : core[2];
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("conditional_patterns.sv"))], "Top");
        let sel = sim.signal("sel");
        let core = sim.signal("core");
        sim.modify(|io| {
            io.set(core, 0xfa51u16);
            io.set(sel, 1u8);
        })
        .unwrap();
        assert_eq!(u64::try_from(sim.get(sim.signal("pad"))).unwrap(), 0x0020);
        assert_eq!(u64::try_from(sim.get(sim.signal("staged"))).unwrap(), 0x01);
        assert_eq!(u64::try_from(sim.get(sim.signal("nested"))).unwrap(), 0xf);
        sim.modify(|io| io.set(sel, 0u8)).unwrap();
        assert_eq!(u64::try_from(sim.get(sim.signal("pad"))).unwrap(), 0xfa51);
        assert_eq!(u64::try_from(sim.get(sim.signal("staged"))).unwrap(), 0x1b);
        assert_eq!(u64::try_from(sim.get(sim.signal("nested"))).unwrap(), 0xa);
        sim.modify(|io| {
            io.set(core, 0x0a51u16);
            io.set(sel, 1u8);
        })
        .unwrap();
        assert_eq!(u64::try_from(sim.get(sim.signal("nested"))).unwrap(), 0x4);
    }
}

// A cast in an item of a pattern-valued parameter is evaluated by the
// pattern, in the type of its member, as any other item is. The parameter
// is initialized at time zero, so the design runs as a timed simulation.
macro_rules! pattern_parameter_casts_backend {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let source = r#"
    package tl_pkg;
      typedef enum logic [2:0] { PutFull = 3'h0, Get = 3'h4 } op_e;
      typedef struct packed { logic [1:0] rsvd; logic [3:0] instr; } user_t;
      localparam user_t USER_DEFAULT = '{ instr: 4'h9, default: '0 };
      localparam logic [7:0] Blanked = {8{1'b1}};
      typedef struct packed {
        logic       d_ready;
        op_e        a_opcode;
        user_t      a_user;
        logic [7:0] a_data;
        logic       a_valid;
      } h2d_t;
      localparam h2d_t H2D_DEFAULT = '{
        d_ready:  1'b1,
        a_opcode: op_e'('0),
        a_user:   USER_DEFAULT,
        a_data:   Blanked,
        default:  '0
      };
    endpackage
    module Sink(input tl_pkg::h2d_t tl_i, output logic [18:0] seen);
      assign seen = tl_i;
    endmodule
    module Top(output logic [18:0] through_port, output logic [18:0] assigned);
      Sink sink(.tl_i(tl_pkg::H2D_DEFAULT), .seen(through_port));
      assign assigned = tl_pkg::H2D_DEFAULT;
    endmodule
  "#;
            let mut sim = celox::Simulation::from_sv_sources(
                vec![(source, Path::new("pattern_parameter_casts.sv"))],
                "Top",
            )
            .$backend()
            .unwrap();
            sim.run_until(0).unwrap();
            for name in ["through_port", "assigned"] {
                let signal = sim.signal(name);
                assert_eq!(
                    u64::try_from(sim.get(signal)).unwrap(),
                    0b1_000_001001_11111111_0,
                    "{name}"
                );
            }
        }
    };
}
pattern_parameter_casts_backend!(pattern_parameter_casts_interpreter, build_interpreter);
pattern_parameter_casts_backend!(pattern_parameter_casts_cranelift, build);
#[cfg(target_arch = "x86_64")]
pattern_parameter_casts_backend!(pattern_parameter_casts_native, build_native);
pattern_parameter_casts_backend!(pattern_parameter_casts_wasm, build_wasm);

// An enumeration member wider than the numeric constant environment is known
// by its literal, also for the signedness a pattern item needs.
sv_backends! {
    fn wide_enumeration_members_are_pattern_items(sim) {
        @setup {
            let source = r#"
    package st_pkg;
      parameter logic [79:0] A = 80'h1234_5678_9abc_def0_1357;
      parameter logic [79:0] B = 80'hbeef_0000_0000_0000_cafe;
      typedef logic [159:0] state_t;
      typedef enum state_t {
        StRaw      = {160{1'b0}},
        StUnlocked = {A, B}
      } state_e;
    endpackage
    package data_pkg;
      typedef struct packed {
        logic valid;
        st_pkg::state_t state;
      } data_t;
      parameter data_t DATA_DEFAULT = '{valid: 1'b1, state: st_pkg::StUnlocked};
    endpackage
    module Top(input logic sel, output logic valid, output logic [15:0] low, high,
               output logic same);
      data_pkg::data_t d;
      assign d = sel ? data_pkg::DATA_DEFAULT : '{valid: 1'b0, state: st_pkg::StRaw};
      assign valid = d.valid;
      assign low = d.state[15:0];
      assign high = d.state[159:144];
      assign same = d.state == st_pkg::StUnlocked;
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("wide_enum_pattern_items.sv"))], "Top");
        let sel = sim.signal("sel");
        sim.modify(|io| io.set(sel, 1u8)).unwrap();
        assert_eq!(u64::try_from(sim.get(sim.signal("valid"))).unwrap(), 1);
        assert_eq!(u64::try_from(sim.get(sim.signal("low"))).unwrap(), 0xcafe);
        assert_eq!(u64::try_from(sim.get(sim.signal("high"))).unwrap(), 0x1234);
        assert_eq!(u64::try_from(sim.get(sim.signal("same"))).unwrap(), 1);
        sim.modify(|io| io.set(sel, 0u8)).unwrap();
        assert_eq!(u64::try_from(sim.get(sim.signal("valid"))).unwrap(), 0);
        assert_eq!(u64::try_from(sim.get(sim.signal("same"))).unwrap(), 0);
    }
}

/// The bounds of a select of a member of a packed structure are expressions
/// of the scope: they may query the variable that holds the member, also
/// through the same path, as BaseJump's
/// `bsg_manycore_ruche_x_link_fwd_inject_src_y` does with
/// `link[0][E].fwd[$bits(link[0][E].fwd)-1:...]` (IEEE 1800-2023 7.2.1,
/// 20.6.2). Verilator 5.054 prints the same line.
#[test]
fn bounds_of_member_selects_query_the_holding_variable() {
    let source = r#"`timescale 1ns/1ns
package noc_pkg;
  typedef enum logic [2:0] {P = 3'd0, W, E, N, S} Dirs;
endpackage
module top;
  import noc_pkg::*;
  typedef struct packed { logic [5:0] fwd; logic [2:0] rev; } link_s;
  localparam int R = 3;
  link_s [R-1:0][E:W] l;
  logic [2:0] hi;
  assign hi = l[0][E].fwd[$bits(l[0][E].fwd)-1:3];
  initial begin
    l = '0;
    l[0][E].fwd = 6'h2a;
    l[1][W].fwd[$bits(l[1][W].fwd)-1:$bits(l[1][W].rev)] = 3'h6;
    #1 $display("%0d %0d %h %h %h", $bits(l[0][E].fwd), $bits(l[0]), hi, l[1][W].fwd, l[0][E].fwd[$bits(l[0])-13:3]);
  end
endmodule"#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("member_select_bounds.sv"))],
            "top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(lines, ["6 18 5 30 5"], "four_state={four_state}");
    }
}

/// A `default` of a structure pattern reaches the members of nested
/// structures only (IEEE 1800-2023 10.9.2): a packed union member takes the
/// value as a whole, as BlackParrot's command structures with an operand
/// union do (Verilator: 000, 1ff).
const UNION_MEMBER_DEFAULTS: &str = r#"
    module Top(output logic [11:0] zero, ones, named);
        typedef struct packed { logic [3:0] a; logic [3:0] b; } ab_t;
        typedef struct packed { logic [7:0] c; } c_t;
        typedef union packed { ab_t ab; c_t c; } u_t;
        typedef struct packed { logic [3:0] npc; u_t operands; } s_t;
        s_t z, o, n;
        assign z = '{default: '0};
        assign o = '{npc: 4'h1, default: '1};
        assign n = '{npc: 4'h2, operands: 8'h5a};
        assign zero = z;
        assign ones = o;
        assign named = n;
    endmodule
"#;

sv_backends! {
    fn pattern_defaults_fill_packed_union_members(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(UNION_MEMBER_DEFAULTS, Path::new("union_defaults.sv"))], "Top");
        for (name, value) in [("zero", 0x000u16), ("ones", 0x1ff), ("named", 0x25a)] {
            assert_eq!(sim.get(sim.signal(name)), value.into(), "{name}");
        }
    }

    fn pattern_defaults_fill_packed_union_members_four_state(sim) {
        @setup {}
        @build Simulator::from_sv_sources(vec![(UNION_MEMBER_DEFAULTS, Path::new("union_defaults.sv"))], "Top").four_state(true);
        for (name, value) in [("zero", 0x000u16), ("ones", 0x1ff), ("named", 0x25a)] {
            assert_eq!(sim.get_four_state(sim.signal(name)), (value.into(), 0u8.into()), "{name}");
        }
    }
}

/// A member of an element of a packed array member is driven by a
/// continuous assignment and by an output port, each of which drives only
/// the bits it selects (IEEE 1800-2023 6.5, 23.3.2.2): OpenTitan's register
/// slices connect `.q(reg2hw.regs[0].q)` of many instances to one
/// structure.
#[test]
fn ports_and_assignments_drive_members_of_array_member_elements() {
    let source = r#"
        `timescale 1ns/1ns
        package Regs;
            typedef struct packed { logic [7:0] q; } reg_t;
            typedef struct packed { logic [3:0] a; reg_t [2:0] regs; logic b; } file_t;
        endpackage
        module Nibble(input logic [3:0] d, output logic [3:0] q);
            assign q = d;
        endmodule
        module Byte(input logic [7:0] d, output logic [7:0] q);
            assign q = d;
        endmodule
        module Top;
            Regs::file_t ports;
            Nibble u_low(.d(4'h5), .q(ports.regs[1].q[3:0]));
            Nibble u_high(.d(4'ha), .q(ports.regs[1].q[7:4]));
            Nibble u_a(.d(4'h3), .q(ports.a));
            Byte u_two(.d(8'h77), .q(ports.regs[2].q));
            assign ports.regs[0].q = 8'h11;
            assign ports.b = 1'b1;
            initial #1 $display("ports=%h element=%h q=%h nibble=%h", ports, ports.regs[1],
                                ports.regs[1].q, ports.regs[1].q[7:4]);
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("element_ports.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<String> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            ["ports=06ef4a23 element=a5 q=a5 nibble=a"],
            "four-state {four_state}"
        );
    }
}

/// Two output ports that drive the same member of an element are multiple
/// drivers of those bits (6.5).
#[test]
fn rejects_overlapping_port_drivers_of_array_member_elements() {
    let source = r#"
        package Regs;
            typedef struct packed { logic [7:0] q; } reg_t;
            typedef struct packed { reg_t [1:0] regs; } file_t;
        endpackage
        module Byte(input logic [7:0] d, output logic [7:0] q);
            assign q = d;
        endmodule
        module Top;
            Regs::file_t ports;
            Byte u_a(.d(8'h01), .q(ports.regs[0].q));
            Byte u_b(.d(8'h02), .q(ports.regs[0].q[3:0]));
        endmodule
    "#;
    let error = celox::Simulation::from_sv_sources(vec![(source, Path::new("overlap.sv"))], "Top")
        .build()
        .map(|_| ())
        .expect_err("overlapping drivers are rejected")
        .to_string();
    assert!(error.contains("multiple variable drivers"), "{error}");
}

/// A port that drives a select of a member of an array member element
/// selects by the member's declared indices, ascending or not starting at 0
/// (IEEE 1800-2023 7.4.6). Verilator 5.054 prints the same.
#[test]
fn ports_drive_members_of_array_member_elements_by_their_declared_indices() {
    let source = r#"
        `timescale 1ns/1ns
        package Regs;
            typedef struct packed { logic [0:7] q; logic [15:8] r; } reg_t;
            typedef struct packed { logic [3:0] a; reg_t [1:0] regs; } file_t;
        endpackage
        module Nibble(input logic [3:0] d, output logic [3:0] q);
            assign q = d;
        endmodule
        module Top;
            Regs::file_t ports;
            Nibble u_q0(.d(4'h5), .q(ports.regs[1].q[4:7]));
            Nibble u_q1(.d(4'ha), .q(ports.regs[1].q[0:3]));
            Nibble u_r0(.d(4'h3), .q(ports.regs[1].r[11:8]));
            Nibble u_r1(.d(4'hc), .q(ports.regs[1].r[15:12]));
            Nibble u_a(.d(4'h7), .q(ports.a));
            assign ports.regs[0] = 16'h1234;
            initial #1 $display("%h %h %h", ports, ports.regs[1].q, ports.regs[1].r);
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("element_port_indices.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<String> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(lines, ["7a5c31234 a5 c3"], "four-state {four_state}");
    }
}

// A run-time bit-select or indexed part-select of a member of a packed
// structure selects within the member (IEEE 1800-2023 7.2.1, 11.5.1): the
// bits it selects outside the member's range read X and are not written, and
// are never those of a neighboring member. The members here have a
// descending range that does not end at 0, an ascending range, and a
// structure type.
#[test]
fn run_time_selects_of_structure_members_stay_in_the_member() {
    let source = r#"
        module Top(
            input logic clk, clear, input logic [1:0] mode, input logic [3:0] idx,
            input logic [15:0] value, input logic [1:0] pair_in, input logic bit_in,
            output logic read, any, output logic [3:0] copied,
            output logic [1:0] pair, lower_pair, rising_pair, inner_pair,
            output logic [15:0] written
        );
            typedef struct packed { logic [1:0] hi; logic [1:0] lo; } inner_t;
            typedef struct packed {
                logic valid; logic [7:4] mask; logic [0:3] rising; inner_t inner; logic [2:0] low;
            } word_t;
            word_t word, target;
            assign word = value;
            assign read = word.mask[idx];
            assign pair = word.mask[idx +: 2];
            assign lower_pair = word.mask[idx -: 2];
            assign rising_pair = word.rising[idx +: 2];
            assign inner_pair = word.inner[idx +: 2];
            always_comb begin
                copied = '0;
                for (int i = 0; i < 4; ++i) if (word.mask[i + 4]) copied[i] = 1'b1;
            end
            always_comb begin
                any = 1'b0;
                for (int i = 4; i < 8; ++i) any |= word.mask[i];
            end
            always_ff @(posedge clk)
                if (clear) target <= '0;
                else case (mode)
                    2'd0: target.mask[idx] <= bit_in;
                    2'd1: target.mask[idx +: 2] <= pair_in;
                    2'd2: target.rising[idx +: 2] <= pair_in;
                    2'd3: target.inner[idx -: 2] <= pair_in;
                endcase
            assign written = target;
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            Simulator::from_sv_sources(vec![(source, Path::new("member_bits.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        let clk = sim.event("clk");
        let (clear, mode, idx) = (sim.signal("clear"), sim.signal("mode"), sim.signal("idx"));
        let (value, pair_in, bit_in) = (
            sim.signal("value"),
            sim.signal("pair_in"),
            sim.signal("bit_in"),
        );
        let written = sim.signal("written");
        // valid = 1, mask = 4'b1010, rising = 4'b1100, inner = 4'b1110,
        // low = 3'b101: every member has a 1 next to its neighbors, which an
        // overhanging select must not read.
        sim.modify(|io| {
            io.set(value, 0xd675u16);
            io.set(idx, 5u8);
            io.set(bit_in, 1u8);
            io.set(pair_in, 3u8);
        })
        .unwrap();
        // The value and the unknown bits of an output, the latter only in
        // four-state simulation, where two-state reads them as zero.
        macro_rules! read {
            ($name:literal) => {{
                let (value, unknown) = sim.get_four_state(sim.signal($name));
                let (value, unknown) =
                    (u8::try_from(value).unwrap(), u8::try_from(unknown).unwrap());
                (value & !unknown & 3, unknown)
            }};
        }
        let x = |bits: u8| if four_state { bits } else { 0 };
        assert_eq!(read!("read"), (1, 0));
        assert_eq!(read!("any"), (1, 0));
        assert_eq!(sim.get(sim.signal("copied")), 0b1010u8.into());
        // mask[6:5] and mask[5:4].
        assert_eq!(read!("pair"), (0b01, 0));
        assert_eq!(read!("lower_pair"), (0b10, 0));
        // mask[8:7]: bit 8 is above the member, next to `valid`.
        sim.modify(|io| io.set(idx, 7u8)).unwrap();
        assert_eq!(read!("pair"), (0b01, x(0b10)));
        // mask[4:3] and mask[3:2]: below the member, next to `rising[0]`.
        sim.modify(|io| io.set(idx, 3u8)).unwrap();
        assert_eq!(read!("pair"), (0b00, x(0b01)));
        assert_eq!(read!("lower_pair"), (0b00, x(0b11)));
        assert_eq!(read!("read"), (0, x(1)));
        // rising[3:4]: index 4 is past the ascending member, next to `inner`.
        assert_eq!(read!("rising_pair"), (0b00, x(0b01)));
        // inner[4:3]: bit 4 is above the nested structure.
        assert_eq!(read!("inner_pair"), (0b01, x(0b10)));
        // rising[1:2] and inner[2:1], inside their members.
        sim.modify(|io| io.set(idx, 1u8)).unwrap();
        assert_eq!(read!("rising_pair"), (0b10, 0));
        assert_eq!(read!("inner_pair"), (0b11, 0));

        // Writes: only the bits inside the member change.
        sim.modify(|io| io.set(clear, 1u8)).unwrap();
        sim.tick(clk).unwrap();
        let mut write = |m: u8, i: u8, pair: u8| {
            sim.modify(|io| {
                io.set(clear, 0u8);
                io.set(mode, m);
                io.set(idx, i);
                io.set(pair_in, pair);
            })
            .unwrap();
            sim.tick(clk).unwrap();
            sim.get_four_state(written)
        };
        let known = |value: u16| (BigUint::from(value), BigUint::from(0u8));
        // mask[8:7]: mask[7] only, not `valid`.
        assert_eq!(write(1, 7, 3), known(0x4000));
        // mask[4:3]: mask[4] only, not `rising[0]`.
        assert_eq!(write(1, 3, 3), known(0x4800));
        // rising[3:4]: rising[3] only, not `inner`.
        assert_eq!(write(2, 3, 3), known(0x4880));
        // inner[0:-1]: inner[0] only, not `low`.
        assert_eq!(write(3, 0, 3), known(0x4888));
        // A bit-select outside the member writes nothing.
        assert_eq!(write(0, 3, 3), known(0x4888));
        // mask[6:5], inside the member.
        assert_eq!(write(1, 5, 2), known(0x6888));
    }
}

/// A select of a member of a packed structure whose index is known only at
/// run time reads and writes the member's bits, as HammerBlade's
/// `bsg_cache_miss` does, here also by blocking assignments of a procedure;
/// an index outside the member reads X (0 in two-state simulation) and
/// writes nothing, never a neighboring member (IEEE 1800-2023 7.2.1,
/// 11.5.1).
#[test]
fn dynamic_selects_of_packed_struct_members_stay_within_the_member() {
    let source = r#"`timescale 1ns/1ns
module top;
  typedef struct packed { logic [3:0] dirty; logic [7:5] high; logic [2:0] lru_bits; } stat_s;
  stat_s s;
  logic [2:0] w;
  logic [2:0] k;
  logic d, h, l;
  logic [1:0] part;
  always_comb begin
    d = s.dirty[w];
    h = s.high[k];
    l = s.lru_bits[w];
    part = s.dirty[w +: 2];
  end
  initial begin
    s = 10'b1010_011_001;
    for (int i = 0; i < 8; i++) begin
      w = i[2:0]; k = 3'(i + 3);
      #1 $display("w=%0d d=%b l=%b k=%0d h=%b part=%b", w, d, l, k, h, part);
    end
    w = 3'd6; s.lru_bits[w] = 1'b1;
    w = 3'd1; s.dirty[w] = 1'b0;
    k = 3'd6; s.high[k] = 1'b0;
    k = 3'd2; s.high[k] = 1'b0;
    #1 $display("s=%b", s);
    $finish;
  end
endmodule"#;
    for four_state in [false, true] {
        let x = if four_state { "x" } else { "0" };
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("dynamic_member.sv"))],
            "top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(20).unwrap();
        let lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        let expected = [
            format!("w=0 d=0 l=1 k=3 h={x} part=10"),
            format!("w=1 d=1 l=0 k=4 h={x} part=01"),
            "w=2 d=0 l=0 k=5 h=1 part=10".to_string(),
            // dirty[4] is past the member, next to `high`: only it is X.
            format!("w=3 d=1 l={x} k=6 h=1 part={x}1"),
            format!("w=4 d={x} l={x} k=7 h=0 part={x}{x}"),
            format!("w=5 d={x} l={x} k=0 h={x} part={x}{x}"),
            format!("w=6 d={x} l={x} k=1 h={x} part={x}{x}"),
            format!("w=7 d={x} l={x} k=2 h={x} part={x}{x}"),
            "s=1000001001".to_string(),
        ];
        assert_eq!(lines, expected, "four_state={four_state}");
    }
}

/// A run-time indexed part-select of a member of a range that is not
/// `[n-1:0]`, descending or ascending, selects the bits its declared
/// indices name (IEEE 1800-2023 7.4.6, 11.5.1). Verilator 5.054 prints
/// the same lines.
#[test]
fn dynamic_indexed_part_selects_of_members_use_the_declared_range() {
    let source = r#"`timescale 1ns/1ns
module top;
  typedef struct packed { logic [7:4] hi; logic [0:3] asc; logic [3:0] lo; } s_t;
  s_t s;
  int i;
  initial begin
    s = 12'b1010_0110_1100;
    for (i = 4; i <= 6; i++)
      $display("i=%0d up=%b asc=%b", i, s.hi[i +: 2], s.asc[i - 4 +: 2]);
    for (i = 5; i <= 7; i++)
      $display("i=%0d down=%b asc=%b", i, s.hi[i -: 2], s.asc[i - 4 -: 2]);
    i = 5; s.hi[i +: 2] = 2'b01;
    i = 1; s.asc[i +: 2] = 2'b01;
    $display("s=%b", s);
  end
endmodule"#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("dynamic_member_ranges.sv"))],
            "top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            [
                "i=4 up=10 asc=01",
                "i=5 up=01 asc=11",
                "i=6 up=10 asc=10",
                "i=5 down=10 asc=01",
                "i=6 down=01 asc=11",
                "i=7 down=10 asc=10",
                "s=101000101100",
            ],
            "four_state={four_state}"
        );
    }
}

// A run-time index of a member of several packed dimensions selects one of
// its elements (IEEE 1800-2023 7.4.5, 11.5.1), as Ariane's configuration
// structure does with `Cfg.NonIdempotentAddrBase[k]`: an index outside the
// member's range reads X and writes nothing, and never reaches a
// neighboring member.
#[test]
fn run_time_element_selects_of_structure_members_stay_in_the_member() {
    let source = r#"
        module Top(
            input logic clk, clear, input logic [1:0] idx, input logic [14:0] value,
            input logic [3:0] nibble, output logic [3:0] element, output logic [1:0] low,
            output logic [14:0] written
        );
            typedef struct packed { logic valid; logic [2:0][3:0] base; logic [1:0] low; } cfg_t;
            cfg_t cfg, target;
            assign cfg = value;
            assign element = cfg.base[idx];
            assign low = cfg.base[idx][1:0];
            always_ff @(posedge clk)
                if (clear) target <= '0;
                else target.base[idx] <= nibble;
            assign written = target;
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            Simulator::from_sv_sources(vec![(source, Path::new("member_elements.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        let clk = sim.event("clk");
        let (clear, idx, value, nibble) = (
            sim.signal("clear"),
            sim.signal("idx"),
            sim.signal("value"),
            sim.signal("nibble"),
        );
        // valid = 1, base = {4'ha, 4'h5, 4'hc}, low = 2'b11: the members
        // next to `base` hold ones that an index past it must not read.
        sim.modify(|io| io.set(value, 0x6973u16)).unwrap();
        let mut read = |name: &str, i: u8| {
            sim.modify(|io| io.set(idx, i)).unwrap();
            let (value, unknown) = sim.get_four_state(sim.signal(name));
            let (value, unknown) = (u8::try_from(value).unwrap(), u8::try_from(unknown).unwrap());
            (value & !unknown, unknown)
        };
        let x = |bits: u8| if four_state { bits } else { 0 };
        assert_eq!(read("element", 0), (0xc, 0));
        assert_eq!(read("element", 1), (0x5, 0));
        assert_eq!(read("element", 2), (0xa, 0));
        assert_eq!(read("low", 2), (0b10, 0));
        // Index 3 is past the member, next to `valid`.
        assert_eq!(read("element", 3), (0, x(0xf)));
        assert_eq!(read("low", 3), (0, x(0b11)));

        let written = sim.signal("written");
        sim.modify(|io| io.set(clear, 1u8)).unwrap();
        sim.tick(clk).unwrap();
        let mut write = |i: u8, value: u8| {
            sim.modify(|io| {
                io.set(clear, 0u8);
                io.set(idx, i);
                io.set(nibble, value);
            })
            .unwrap();
            sim.tick(clk).unwrap();
            sim.get_four_state(written)
        };
        let known = |value: u16| (BigUint::from(value), BigUint::from(0u8));
        // Index 3 writes nothing, not `valid`.
        assert_eq!(write(3, 0xf), known(0));
        assert_eq!(write(0, 0xf), known(0x3c));
        assert_eq!(write(2, 0x6), known(0x183c));
    }
}
