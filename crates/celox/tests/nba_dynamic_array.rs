#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_subbyte_arithmetic_padding_does_not_corrupt_concat(sim) {
        @case "nba_dynamic_array::test_subbyte_arithmetic_padding_does_not_corrupt_concat";
    }

    fn test_child_dynamic_ff_read_reaches_parent_after_same_edge_enable(sim) {
        @case "nba_dynamic_array::test_child_dynamic_ff_read_reaches_parent_after_same_edge_enable";
    }

    fn test_static_ff_writes_are_applied_after_all_rhs_evaluation(sim) {
        @case "nba_dynamic_array::test_static_ff_writes_are_applied_after_all_rhs_evaluation";
    }

    // Separate always_ff blocks on the same clock sample the same pre-edge
    // state. A dynamic array write in one block must not become visible to a
    // read in another block until all blocks for the edge have evaluated.
    fn test_dynamic_array_write_is_deferred_across_ff_blocks(sim) {
        @case "nba_dynamic_array::test_dynamic_array_write_is_deferred_across_ff_blocks";
    }

    fn test_partial_sparse_chunks_do_not_overlap_adjacent_variables(sim) {
        @case "nba_dynamic_array::test_partial_sparse_chunks_do_not_overlap_adjacent_variables";
    }

    fn test_always_ff_let_bindings_are_visible_immediately(sim) {
        @case "nba_dynamic_array::test_always_ff_let_bindings_are_visible_immediately";
    }

    fn test_wide_dynamic_ff_checkpoint_round_trip(sim) {
        @case "nba_dynamic_array::test_wide_dynamic_ff_checkpoint_round_trip";
    }

    fn test_unaligned_309_bit_dynamic_ff_round_trip(sim) {
        @ignore_on(wasm);
        @case "nba_dynamic_array::test_unaligned_309_bit_dynamic_ff_round_trip";
    }

    fn test_packed_rat_checkpoint_round_trip(sim) {
        @case "nba_dynamic_array::test_packed_rat_checkpoint_round_trip";
    }

    fn test_dynamic_ff_array_partial_squash_preserves_head_and_branch(sim) {
        @case "nba_dynamic_array::test_dynamic_ff_array_partial_squash_preserves_head_and_branch";
    }

    fn test_line_write_loop_updates_large_sparse_ff_array(sim) {
        @case "nba_dynamic_array::test_line_write_loop_updates_large_sparse_ff_array";
    }

    fn test_out_of_range_dynamic_ff_stores_are_ignored(sim) {
        @case "nba_dynamic_array::test_out_of_range_dynamic_ff_stores_are_ignored";
    }

    fn test_partly_out_of_range_dynamic_ff_part_select(sim) {
        @case "nba_dynamic_array::test_partly_out_of_range_dynamic_ff_part_select";
    }

    // The Veryl reference simulator writes through an unknown index instead
    // of ignoring the write (IEEE 1800-2023 7.4.6).
    fn test_out_of_range_dynamic_ff_access_four_state(sim) {
        @ignore_on(veryl);
        @case "nba_dynamic_array::test_out_of_range_dynamic_ff_access_four_state";
    }

    // The Veryl reference simulator reads an existing element through an
    // invalid index.
    fn test_out_of_range_dynamic_comb_access(sim) {
        @ignore_on(veryl);
        @case "nba_dynamic_array::test_out_of_range_dynamic_comb_access";
    }

}

all_backends! {
// Each index of a multidimensional array is checked against its own dimension
// (IEEE 1800-2023 7.4.6). This stays out of the shared suite: Icarus 13.0,
// Verilator 5.052, and the Veryl reference simulator all apply `grid[0][3]`
// to another element.
fn test_out_of_range_inner_index_of_dynamic_ff_write_is_ignored(sim) {
    // Veryl 0.22.0's simulator applies an out-of-range inner index to another element; Veryl
    // master no longer does.
    @ignore_on(veryl);
    @setup {
        let source = r#"
            module Top (
                clk   : input  clock,
                load  : input  logic,
                row   : input  logic<2>,
                col   : input  logic<2>,
                din   : input  logic<8>,
                grid_q: output logic<48>,
                rd_q  : output logic<8>,
            ) {
                var grid: logic<8> [2, 3];
                var rd  : logic<8>;
                always_ff (clk) {
                    if load {
                        for i in 0..2 {
                            for j in 0..3 {
                                grid[i][j] = 8'h0;
                            }
                        }
                        rd = 8'h5a;
                    } else {
                        grid[row][col] = din;
                        rd             = grid[row][col];
                    }
                }
                assign grid_q = {grid[1][2], grid[1][1], grid[1][0], grid[0][2], grid[0][1], grid[0][0]};
                assign rd_q   = rd;
            }
        "#;
    }
    @build celox::SimulatorBuilder::new(source, "Top");
    let clk = sim.event("clk");
    let load = sim.signal("load");
    let row = sim.signal("row");
    let col = sim.signal("col");
    let din = sim.signal("din");
    let grid_q = sim.signal("grid_q");
    let rd_q = sim.signal("rd_q");
    for (r, c) in [(0u8, 3u8), (1, 3), (2, 0), (3, 1), (2, 3)] {
        sim.modify(|io| io.set(load, 1u8)).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| {
            io.set(load, 0u8);
            io.set(row, r);
            io.set(col, c);
            io.set(din, 0xabu8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(grid_q), 0u8.into(), "grid[{r}][{c}] write");
        assert_eq!(sim.get(rd_q), 0u8.into(), "grid[{r}][{c}] read");
    }
    sim.modify(|io| {
        io.set(row, 1u8);
        io.set(col, 2u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(grid_q), (0xabu64 << 40).into());
    assert_eq!(sim.get(rd_q), 0xabu8.into());
}
}

all_backends! {
// A `-:` part select whose low bits fall below bit 0 still writes and reads
// its in-range bits (IEEE 1800-2023 11.5.1). Icarus 13.0 agrees; Verilator
// 5.052 and the Veryl reference simulator ignore the whole write, so this
// stays out of the shared suite.
fn test_descending_part_select_below_bit_zero_keeps_in_range_bits(sim) {
    // Veryl 0.22.0's simulator drops the whole write of a `-:` select that runs below bit 0.
    @ignore_on(veryl);
    @setup {
        let source = r#"
            module Top (
                clk : input  clock,
                load: input  logic,
                base: input  logic<3>,
                q   : output logic<6>,
                r   : output logic<2>,
            ) {
                var v : logic<6>;
                var rr: logic<2>;
                always_ff (clk) {
                    if load {
                        v  = 6'h0;
                        rr = 2'b0;
                    } else {
                        v[base-:2] = 2'b11;
                        rr         = v[base-:2];
                    }
                }
                assign q = v;
                assign r = rr;
            }
        "#;
    }
    @build celox::SimulatorBuilder::new(source, "Top");
    let clk = sim.event("clk");
    let load = sim.signal("load");
    let base = sim.signal("base");
    let q = sim.signal("q");
    let r = sim.signal("r");
    // (base, v after the write, v[base-:2] read after the write)
    for (b, written, read) in [
        (0u8, 0x01u8, 0b10u8),
        (1, 0x03, 0b11),
        (5, 0x30, 0b11),
        (6, 0x20, 0b01),
        (7, 0x00, 0b00),
    ] {
        sim.modify(|io| io.set(load, 1u8)).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| {
            io.set(load, 0u8);
            io.set(base, b);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), written.into(), "v[{b}-:2] write");
        assert_eq!(sim.get(r), read.into(), "v[{b}-:2] read");
    }
}
}

all_backends! {
// Combinational dynamic accesses check each index against its own dimension
// and keep the in-range bits of a `-:` part select that starts below bit 0
// (IEEE 1800-2023 7.4.6, 11.5.1). In a two-state simulation an invalid read,
// like any X, reads 0. The Veryl reference simulator applies `grid[0][3]` to
// another element.
fn test_out_of_range_dynamic_comb_access_two_state(sim) {
    // Veryl 0.22.0's simulator reads an existing element through an out-of-range inner index,
    // and drops a `-:` write below bit 0.
    @ignore_on(veryl);
    @setup {
        let source = r#"
            module Top (
                row   : input  logic<2>,
                col   : input  logic<2>,
                base  : input  logic<3>,
                v     : input  logic<8>,
                rd_q  : output logic<8>,
                grid_q: output logic<48>,
                down_q: output logic<6>,
                dr_q  : output logic<2>,
                x_q   : output logic<8>,
            ) {
                var src : logic<8> [2, 3];
                var dst : logic<8> [2, 3];
                var down: logic<6>;
                var ones: logic<6>;
                always_comb {
                    for i in 0..2 {
                        for j in 0..3 {
                            src[i][j] = 8'h11;
                        }
                    }
                    ones = 6'h3f;
                }
                always_comb {
                    for i in 0..2 {
                        for j in 0..3 {
                            dst[i][j] = 8'h00;
                        }
                    }
                    dst[row][col] = v;
                    down = 6'h00;
                    down[base-:2] = 2'b11;
                }
                assign rd_q   = src[row][col];
                assign grid_q = {dst[1][2], dst[1][1], dst[1][0], dst[0][2], dst[0][1], dst[0][0]};
                assign down_q = down;
                assign dr_q   = ones[base-:2];
                assign x_q    = 8'hxx;
            }
        "#;
    }
    @build celox::SimulatorBuilder::new(source, "Top");
    let row = sim.signal("row");
    let col = sim.signal("col");
    let base = sim.signal("base");
    let v = sim.signal("v");
    let rd_q = sim.signal("rd_q");
    let grid_q = sim.signal("grid_q");
    let down_q = sim.signal("down_q");
    let dr_q = sim.signal("dr_q");
    let x_q = sim.signal("x_q");
    assert_eq!(sim.get(x_q), 0u8.into(), "X literal in a two-state simulation");
    for (r, c) in [(0u8, 3u8), (1, 3), (2, 0), (3, 1), (2, 3)] {
        sim.modify(|io| {
            io.set(row, r);
            io.set(col, c);
            io.set(v, 0xabu8);
        })
        .unwrap();
        assert_eq!(sim.get(rd_q), 0u8.into(), "src[{r}][{c}] read");
        assert_eq!(sim.get(grid_q), 0u8.into(), "dst[{r}][{c}] write");
    }
    sim.modify(|io| {
        io.set(row, 1u8);
        io.set(col, 2u8);
    })
    .unwrap();
    assert_eq!(sim.get(rd_q), 0x11u8.into());
    assert_eq!(sim.get(grid_q), (0xabu64 << 40).into());
    // (base, down after `down[base-:2] = 2'b11`, ones[base-:2])
    for (b, written, read) in [
        (0u8, 0x01u8, 0b10u8),
        (1, 0x03, 0b11),
        (5, 0x30, 0b11),
        (6, 0x20, 0b01),
        (7, 0x00, 0b00),
    ] {
        sim.modify(|io| io.set(base, b)).unwrap();
        assert_eq!(sim.get(down_q), written.into(), "down[{b}-:2] write");
        assert_eq!(sim.get(dr_q), read.into(), "ones[{b}-:2] read");
    }
}
}

all_backends! {
// A loop variable's known range removes only the checks it proves
// unnecessary: `src[i + off]` can still leave the array when `off` is large,
// while `dst[i * 8 +: 8]` in `0..4` cannot.
fn test_loop_variable_range_keeps_needed_checks(sim) {
    @setup {
        let source = r#"
            module Top (
                off  : input  logic<3>,
                v    : input  logic<32>,
                sum_q: output logic<32>,
                dst_q: output logic<32>,
            ) {
                var src: logic<8> [6];
                var dst: logic<32>;
                var sum: logic<32>;
                always_comb {
                    for i in 0..6 {
                        src[i] = (i + 1) as 8;
                    }
                }
                always_comb {
                    sum = 0;
                    for i in 0..4 {
                        sum = sum + {24'h0, src[i + off]};
                    }
                    dst = 0;
                    for i in 0..4 {
                        dst[i * 8 +: 8] = v[i * 8 +: 8] ^ (i as 8);
                    }
                }
                assign sum_q = sum;
                assign dst_q = dst;
            }
        "#;
    }
    @build celox::SimulatorBuilder::new(source, "Top");
    let off = sim.signal("off");
    let v = sim.signal("v");
    let sum_q = sim.signal("sum_q");
    let dst_q = sim.signal("dst_q");
    for o in 0u8..8 {
        sim.modify(|io| {
            io.set(off, o);
            io.set(v, 0x1234_5678u32);
        })
        .unwrap();
        // Elements past the array read 0 in a two-state simulation.
        let expected: u32 = (0..4u32)
            .map(|i| i + u32::from(o))
            .filter(|&index| index < 6)
            .map(|index| index + 1)
            .sum();
        assert_eq!(sim.get(sum_q), expected.into(), "off={o}");
        assert_eq!(sim.get(dst_q), (0x1234_5678u32 ^ 0x0302_0100u32).into(), "off={o}");
    }
}
}

all_backends! {
// Functions read their array arguments through the same checks, in
// combinational and sequential code: an invalid index reads X. Icarus 13.0
// does not support unpacked-array function formals.
fn test_out_of_range_dynamic_read_in_function_is_unknown(sim) {
    @setup {
        let source = r#"
            module Top (
                clk   : input  clock,
                idx   : input  logic<2>,
                comb_q: output logic<8>,
                ff_q  : output logic<8>,
            ) {
                var src: logic<8> [3];
                var ffv: logic<8>;
                function pick (m: input logic<8> [3], p: input logic<2>) -> logic<8> {
                    return m[p];
                }
                always_comb {
                    for i in 0..3 {
                        src[i] = 8'h11;
                    }
                }
                always_ff (clk) {
                    ffv = pick(src, idx);
                }
                assign comb_q = pick(src, idx);
                assign ff_q   = ffv;
            }
        "#;
    }
    @build celox::SimulatorBuilder::new(source, "Top").four_state(true);
    let clk = sim.event("clk");
    let idx = sim.signal("idx");
    let comb_q = sim.signal("comb_q");
    let ff_q = sim.signal("ff_q");
    let x = (celox::BigUint::from(0xffu8), celox::BigUint::from(0xffu8));
    sim.modify(|io| io.set(idx, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_four_state(comb_q), x);
    assert_eq!(sim.get_four_state(ff_q), x);
    sim.modify(|io| io.set(idx, 2u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(comb_q), 0x11u8.into());
    assert_eq!(sim.get(ff_q), 0x11u8.into());
}
}

all_backends! {
// A negative signed index is invalid (IEEE 1800-2023 7.4.6). Its raw bits fit
// the outer dimension, but scaling it by the inner dimension sign-extends it,
// so a read must still redirect its address instead of loading far outside
// the array.
fn test_negative_signed_index_reads_and_writes_nothing(sim) {
    // Veryl 0.22.0's simulator reads an element through a negative signed index; Veryl master
    // no longer does.
    @ignore_on(veryl);
    @setup {
        let source = r#"
            module Top (
                clk   : input  clock,
                idx   : input  signed bit<2>,
                v     : input  logic<8>,
                comb_q: output logic<8>,
                ff_q  : output logic<8>,
                mem_q : output logic<32>,
            ) {
                var src: logic<8> [4, 2];
                var dst: logic<8> [4, 2];
                var rd : logic<8>;
                always_comb {
                    for i in 0..4 {
                        src[i][0] = (i + 1) as 8;
                        src[i][1] = 8'h00;
                    }
                }
                always_ff (clk) {
                    dst[idx][0] = v;
                    rd          = src[idx][0];
                }
                assign comb_q = src[idx][0];
                assign ff_q   = rd;
                assign mem_q  = {dst[3][0], dst[2][0], dst[1][0], dst[0][0]};
            }
        "#;
    }
    @build celox::SimulatorBuilder::new(source, "Top");
    let clk = sim.event("clk");
    let idx = sim.signal("idx");
    let v = sim.signal("v");
    let comb_q = sim.signal("comb_q");
    let ff_q = sim.signal("ff_q");
    let mem_q = sim.signal("mem_q");
    // Clear `dst` through the in-range indices first.
    for i in 0u8..2 {
        sim.modify(|io| {
            io.set(idx, i);
            io.set(v, 0u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
    }
    // 2'b10 and 2'b11 are -2 and -1.
    for raw in [2u8, 3] {
        sim.modify(|io| {
            io.set(idx, raw);
            io.set(v, 0xabu8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(comb_q), 0u8.into(), "comb read of idx={raw:#b}");
        assert_eq!(sim.get(ff_q), 0u8.into(), "always_ff read of idx={raw:#b}");
        assert_eq!(sim.get(mem_q), 0u8.into(), "always_ff write of idx={raw:#b}");
    }
    sim.modify(|io| io.set(idx, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(comb_q), 2u8.into());
    assert_eq!(sim.get(ff_q), 2u8.into());
    assert_eq!(sim.get(mem_q), 0xab00u32.into());
}
}

all_backends! {
// An out-of-range read is X, which a two-state simulation reads as 0 also
// when it is the data side of a wildcard comparison.
fn test_out_of_range_read_compares_as_zero_in_two_state_wildcard(sim) {
    @setup {
        let source = r#"
            module Top (
                idx: input  logic<2>,
                hit: output logic,
            ) {
                var arr: logic<4> [3];
                always_comb {
                    for i in 0..3 {
                        arr[i] = 4'b0000;
                    }
                }
                assign hit = arr[idx] ==? 4'b1x1x;
            }
        "#;
    }
    @build celox::SimulatorBuilder::new(source, "Top");
    let idx = sim.signal("idx");
    let hit = sim.signal("hit");
    for i in 0u8..4 {
        sim.modify(|io| io.set(idx, i)).unwrap();
        assert_eq!(sim.get(hit), 0u8.into(), "idx={i}");
    }
}
}

all_backends! {
// Every read path agrees in a four-state simulation: an invalid index into a
// two-state variable reads 0, into a four-state value X, also when the value
// is an always_ff local or a function argument held in a register
// (IEEE 1800-2023 7.4.6).
fn test_invalid_reads_agree_across_paths_in_four_state(sim) {
    // Veryl 0.22.0's simulator returns X instead of 0 for an invalid read of a 2-state array.
    @ignore_on(veryl);
    @setup {
        let source = r#"
            module Top (
                clk   : input  clock,
                i     : input  logic<2>,
                j     : input  logic<4>,
                v     : input  logic<32>,
                bit_q : output logic<8>,
                fn_q  : output logic,
                let_q : output logic,
            ) {
                var arr: bit<8> [3];
                var fnv: logic;
                var lev: logic;
                always_comb {
                    for k in 0..3 {
                        arr[k] = 8'h11;
                    }
                }
                function pick (x: input logic<8>, b: input logic<4>) -> logic {
                    return x[b];
                }
                always_ff (clk) {
                    let t: logic<8> = v[7:0];
                    fnv = pick(v[7:0], j);
                    lev = t[j];
                }
                assign bit_q = arr[i];
                assign fn_q  = fnv;
                assign let_q = lev;
            }
        "#;
    }
    @build celox::SimulatorBuilder::new(source, "Top").four_state(true);
    let clk = sim.event("clk");
    let i = sim.signal("i");
    let j = sim.signal("j");
    let v = sim.signal("v");
    let bit_q = sim.signal("bit_q");
    let fn_q = sim.signal("fn_q");
    let let_q = sim.signal("let_q");
    let zero = (celox::BigUint::from(0u8), celox::BigUint::from(0u8));
    let x = (celox::BigUint::from(1u8), celox::BigUint::from(1u8));
    sim.modify(|io| {
        io.set(i, 3u8);
        io.set(j, 0u8);
        io.set(v, 0x0000_ff7fu32);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_four_state(bit_q), zero, "two-state array read");
    sim.modify(|io| {
        io.set(i, 0u8);
        io.set(j, 8u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_four_state(fn_q), x, "function argument out of range");
    assert_eq!(sim.get_four_state(let_q), x, "always_ff let out of range");
    sim.modify(|io| io.set(j, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_four_state(fn_q), zero, "function argument in range");
    assert_eq!(sim.get_four_state(let_q), zero, "always_ff let in range");
}
}
