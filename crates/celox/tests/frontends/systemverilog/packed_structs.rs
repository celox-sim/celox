use super::*;

#[path = "packed_structs/port_connections.rs"]
mod port_connections;

sv_backends! {
fn packed_struct_array_unknown_indices(sim) { @case "packed_structs::packed_struct_array_unknown_indices"; }
fn packed_struct_array_dynamic_members(sim) { @case "packed_structs::packed_struct_array_dynamic_members"; }
fn packed_struct_array_ascending_members(sim) { @case "packed_structs::packed_struct_array_ascending_members"; }
    fn packed_struct_layout_and_nested_fields(sim) {
        @case "packed_structs::packed_struct_layout_and_nested_fields";
    }

    fn packed_struct_field_assignments_and_registers(sim) {
        @case "packed_structs::packed_struct_field_assignments_and_registers";
    }

    fn packed_struct_mixed_state_members(sim) {
        @case "packed_structs::packed_struct_mixed_state_members";
    }

    fn packed_struct_parameterized_ports(sim) {
        @case "packed_structs::packed_struct_parameterized_ports";
    }

    fn packed_struct_bounds_and_type_queries(sim) {
        @case "packed_structs::packed_struct_bounds_and_type_queries";
    }

    fn packed_struct_wide_four_state_layout(sim) {
        @case "packed_structs::packed_struct_wide_four_state_layout";
    }

    fn packed_struct_whole_value_signedness(sim) {
        @case "packed_structs::packed_struct_whole_value_signedness";
    }

    fn packed_struct_typedef_bounds_survive_generate_shadowing(sim) {
        @case "packed_structs::packed_struct_typedef_bounds_survive_generate_shadowing";
    }
}

sv_backends! {
    fn packed_struct_alias_packed_dimensions(sim) {
        @case "packed_structs::packed_struct_alias_packed_dimensions";
    }
}

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
