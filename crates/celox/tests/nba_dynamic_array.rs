#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_subbyte_arithmetic_padding_does_not_corrupt_concat(sim) {
        @ignore_on(sv);
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
        @ignore_on(sv);
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
    // of ignoring the write (IEEE 1800-2023 7.4.6). The SV frontend rejects
    // four-state always_ff event signals.
    fn test_out_of_range_dynamic_ff_access_four_state(sim) {
        @ignore_on(veryl, sv);
        @case "nba_dynamic_array::test_out_of_range_dynamic_ff_access_four_state";
    }

}

all_backends! {
// Each index of a multidimensional array is checked against its own dimension
// (IEEE 1800-2023 7.4.6). This stays out of the shared suite: Icarus 13.0,
// Verilator 5.052, and the Veryl reference simulator all apply `grid[0][3]`
// to another element.
fn test_out_of_range_inner_index_of_dynamic_ff_write_is_ignored(sim) {
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
