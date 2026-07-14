use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_child_dynamic_ff_read_reaches_parent_after_same_edge_enable(sim) {
        @setup { let code = r#"
module Cache (
    clk  : input  clock,
    wen  : input  logic,
    index: input  logic<3>,
    din  : input  logic<64>,
    probe: input  logic<3>,
    rdata0: output logic<64>,
    rdata1: output logic<64>,
) {
    var mem: logic<64> [8];
    always_ff (clk) {
        if wen {
            mem[index] = din;
        }
    }
    assign rdata0 = mem[probe];
    assign rdata1 = mem[probe];
}

module Top (
    clk  : input  clock,
    arm  : input  logic,
    wen  : input  logic,
    index: input  logic<3>,
    din  : input  logic<64>,
    probe: input  logic<3>,
    q    : output logic<64>,
) {
    var active: logic;
    var rdata0: logic<64>;
    var rdata1: logic<64>;
    inst cache: Cache (clk, wen, index, din, probe, rdata0, rdata1);
    always_ff (clk) {
        active = arm;
    }
    assign q = if active ? rdata1 : rdata0;
}
"#; }
        @build Simulator::builder(code, "Top");

        let clk = sim.event("clk");
        let arm = sim.signal("arm");
        let wen = sim.signal("wen");
        let index = sim.signal("index");
        let din = sim.signal("din");
        let probe = sim.signal("probe");
        let q = sim.signal("q");
        sim.modify(|io| {
            io.set(arm, 1u8);
            io.set(wen, 1u8);
            io.set(index, 3u8);
            io.set(probe, 3u8);
            io.set(din, 0xdead_beef_1234_5678u64);
        }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), 0xdead_beef_1234_5678u64.into());
    }

    fn test_static_ff_writes_are_applied_after_all_rhs_evaluation(sim) {
        @setup { let code = r#"
module Top (
    clk : input  clock,
    en  : input  logic,
    a   : input  logic<8>,
    b   : input  logic<8>,
    q   : output logic<8>,
) {
    var r: logic<8>;

    always_ff (clk) {
        if en {
            r = a;
        } else {
            r = b;
        }
    }

    always_ff (clk) {
        q = r;
    }
}
"#; }
        @build Simulator::builder(code, "Top");

        let clk = sim.event("clk");
        let en = sim.signal("en");
        let a = sim.signal("a");
        let b = sim.signal("b");
        let q = sim.signal("q");

        sim.modify(|io| {
            io.set(en, 1u8);
            io.set(a, 0x31u8);
            io.set(b, 0x72u8);
        }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), 0u8.into());

        sim.modify(|io| {
            io.set(en, 0u8);
            io.set(a, 0x44u8);
            io.set(b, 0x9au8);
        }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), 0x31u8.into());

        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), 0x9au8.into());
    }

    // Separate always_ff blocks on the same clock sample the same pre-edge
    // state. A dynamic array write in one block must not become visible to a
    // read in another block until all blocks for the edge have evaluated.
    fn test_dynamic_array_write_is_deferred_across_ff_blocks(sim) {
        @setup { let code = r#"
module Top (
    clk  : input  clock,
    we   : input  logic,
    we2  : input  logic,
    addr : input  logic<2>,
    din  : input  logic<8>,
    q    : output logic<8>,
) {
    var mem: logic<8> [4];

    always_ff (clk) {
        if we {
            mem[addr] = din;
        }
        // A second write enable shares the array. It stays disabled below, so
        // it does not change the expected value in this scenario.
        if we2 {
            mem[addr] = din + 1;
        }
    }

    always_ff (clk) {
        q = mem[addr];
    }
}
"#; }
        @build Simulator::builder(code, "Top");

        let clk = sim.event("clk");
        let we = sim.signal("we");
        let we2 = sim.signal("we2");
        let addr = sim.signal("addr");
        let din = sim.signal("din");
        let q = sim.signal("q");

        sim.modify(|io| {
            io.set(we, 1u8);
            io.set(we2, 0u8);
            io.set(addr, 2u8);
            io.set(din, 0x33u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        // The first write must not feed q on the same edge.
        assert_eq!(
            sim.get(q),
            0u8.into(),
            "the first write must not feed a separate always_ff on the same edge"
        );

        // mem[2] changes 0x33 -> 0xA5 at this edge. q must sample old mem[2].
        sim.modify(|io| io.set(din, 0xA5u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get(q),
            0x33u8.into(),
            "a separate always_ff block must see the pre-edge array element"
        );

        // The deferred write becomes visible on the following edge.
        sim.modify(|io| io.set(we, 0u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), 0xA5u8.into());
    }

    fn test_partial_sparse_chunks_do_not_overlap_adjacent_variables(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Top (
    clk  : input  clock,
    addr : input  logic<2>,
    din  : input  logic<3>,
    qa   : output logic<3>,
    qb   : output logic<3>,
) {
    var a: logic<3> [3];
    var b: logic<3> [3];

    always_ff (clk) {
        a[addr] = din;
        b[addr] = din + 1;
    }

    always_ff (clk) {
        qa = a[addr];
        qb = b[addr];
    }
}
"#; }
        @build Simulator::builder(code, "Top").four_state(true);

        let clk = sim.event("clk");
        let addr = sim.signal("addr");
        let din = sim.signal("din");
        let qa = sim.signal("qa");
        let qb = sim.signal("qb");

        sim.modify(|io| {
            io.set(addr, 2u8);
            io.set(din, 3u8);
        }).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(din, 5u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(qa), 3u8.into());
        assert_eq!(sim.get(qb), 4u8.into());

        sim.modify(|io| io.set(addr, 0u8)).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(addr, 2u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(qa), 5u8.into());
        assert_eq!(sim.get(qb), 6u8.into());
    }

    fn test_always_ff_let_bindings_are_visible_immediately(sim) {
        @setup { let code = r#"
module Top (
    clk: input  clock,
    x  : input  logic<8>,
    q  : output logic<8>,
) {
    always_ff (clk) {
        let a: logic<8> = x + 8'd1;
        let b: logic<8> = a + 8'd1;
        q = b;
    }
}
"#; }
        @build Simulator::builder(code, "Top");

        let clk = sim.event("clk");
        let x = sim.signal("x");
        let q = sim.signal("q");

        sim.modify(|io| io.set(x, 0x35u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get(q),
            0x37u8.into(),
            "always_ff let bindings must use blocking procedural-local semantics",
        );

        sim.modify(|io| io.set(x, 0x80u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), 0x82u8.into());
    }

    fn test_wide_dynamic_ff_checkpoint_round_trip(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Top (
    clk    : input  clock,
    capture: input  logic,
    restore: input  logic,
    idx    : input  logic<5>,
    d0     : input  logic<64>,
    d1     : input  logic<64>,
    d2     : input  logic<64>,
    q0     : output logic<64>,
    q1     : output logic<64>,
    q2     : output logic<64>,
) {
    var checkpoint: logic<192> [32];

    always_ff (clk) {
        if capture {
            checkpoint[idx] = {d2, d1, d0};
        }
        if restore {
            q0 = checkpoint[idx][63:0];
            q1 = checkpoint[idx][127:64];
            q2 = checkpoint[idx][191:128];
        }
    }
}
"#; }
        @build Simulator::builder(code, "Top");

        let clk = sim.event("clk");
        let capture = sim.signal("capture");
        let restore = sim.signal("restore");
        let idx = sim.signal("idx");
        let d0 = sim.signal("d0");
        let d1 = sim.signal("d1");
        let d2 = sim.signal("d2");
        let q0 = sim.signal("q0");
        let q1 = sim.signal("q1");
        let q2 = sim.signal("q2");

        for (index, words) in [
            (0u8, [0x0123_4567_89ab_cdefu64, 0xfedc_ba98_7654_3210, 0x55aa_00ff_cc33_9669]),
            (1u8, [0x8000_0000_0000_0001u64, 0x7fff_ffff_ffff_fffe, 0xdead_beef_cafe_babe]),
            (31u8, [0x1111_2222_3333_4444u64, 0x5555_6666_7777_8888, 0x9999_aaaa_bbbb_cccc]),
        ] {
            sim.modify(|io| {
                io.set(idx, index);
                io.set(d0, words[0]);
                io.set(d1, words[1]);
                io.set(d2, words[2]);
                io.set(capture, 1u8);
                io.set(restore, 0u8);
            }).unwrap();
            sim.tick(clk).unwrap();

            sim.modify(|io| {
                io.set(capture, 0u8);
                io.set(restore, 1u8);
            }).unwrap();
            sim.tick(clk).unwrap();

            assert_eq!(sim.get(q0), words[0].into(), "checkpoint[{index}] low word");
            assert_eq!(sim.get(q1), words[1].into(), "checkpoint[{index}] middle word");
            assert_eq!(sim.get(q2), words[2].into(), "checkpoint[{index}] high word");
        }
    }

    fn test_unaligned_309_bit_dynamic_ff_round_trip(sim) {
        @omit_veryl;
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
    clk    : input  clock,
    capture: input  logic,
    capture2: input logic,
    restore: input  logic,
    idx    : input  logic<3>,
    idx2   : input  logic<3>,
    d      : input  logic<309>,
    d2     : input  logic<309>,
    q      : output logic<309>,
) {
    var entries: logic<309> [8];

    always_ff (clk) {
        if capture {
            entries[idx] = d;
        }
        if capture2 {
            entries[idx2] = d2;
        }
        if restore {
            q = entries[idx];
        }
    }
}
"#; }
        @build Simulator::builder(code, "Top");

        let clk = sim.event("clk");
        let capture = sim.signal("capture");
        let capture2 = sim.signal("capture2");
        let restore = sim.signal("restore");
        let idx = sim.signal("idx");
        let idx2 = sim.signal("idx2");
        let d = sim.signal("d");
        let d2 = sim.signal("d2");
        let q = sim.signal("q");

        for (index, value) in [0u8, 1, 2, 7].into_iter().map(|index| {
            let value = (BigUint::from(1u8) << 308usize)
                | (BigUint::from(1u8) << (244usize + index as usize))
                | (BigUint::from(1u8) << 64usize)
                | BigUint::from(0x135u16 + index as u16);
            (index, value)
        }) {
            sim.modify(|io| {
                io.set(idx, index);
                io.set_wide(d, value.clone());
                io.set(capture, 1u8);
                io.set(capture2, 0u8);
                io.set(restore, 0u8);
            })
            .unwrap();
            sim.tick(clk).unwrap();

            sim.modify(|io| {
                io.set(capture, 0u8);
                io.set(restore, 1u8);
            })
            .unwrap();
            sim.tick(clk).unwrap();

            assert_eq!(sim.get(q), value, "entries[{index}] must round-trip exactly");
        }

        let first = (BigUint::from(1u8) << 308usize)
            | (BigUint::from(0x8000_0000u64) << 244usize)
            | BigUint::from(0x55u8);
        let second = (BigUint::from(1u8) << 307usize)
            | (BigUint::from(0x2000_0000u64) << 244usize)
            | BigUint::from(0xaau8);
        sim.modify(|io| {
            io.set(idx, 1u8);
            io.set(idx2, 2u8);
            io.set_wide(d, first.clone());
            io.set_wide(d2, second.clone());
            io.set(capture, 1u8);
            io.set(capture2, 1u8);
            io.set(restore, 0u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();

        for (index, expected) in [(1u8, first), (2u8, second)] {
            sim.modify(|io| {
                io.set(idx, index);
                io.set(capture, 0u8);
                io.set(capture2, 0u8);
                io.set(restore, 1u8);
            })
            .unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(sim.get(q), expected, "adjacent entries[{index}] must not overlap");
        }
    }

    fn test_packed_rat_checkpoint_round_trip(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Top (
    clk    : input  clock,
    capture: input  logic,
    restore: input  logic,
    idx    : input  logic<5>,
    x3_map : input  logic<6>,
    q0     : output logic<6>,
    q3     : output logic<6>,
    q31    : output logic<6>,
) {
    var map   : logic<6> [32];
    var packed_map: logic<192>;
    var checkpoint: logic<192> [32];

    always_comb {
        for r in 0..32 {
            map[r] = r as 6;
        }
        map[3] = x3_map;
        for r in 0..32 {
            packed_map[r * 6 +: 6] = map[r];
        }
    }

    always_ff (clk) {
        if capture {
            checkpoint[idx] = packed_map;
        }
        if restore {
            q0  = checkpoint[idx][0  * 6 +: 6];
            q3  = checkpoint[idx][3  * 6 +: 6];
            q31 = checkpoint[idx][31 * 6 +: 6];
        }
    }
}
"#; }
        @build Simulator::builder(code, "Top");

        let clk = sim.event("clk");
        let capture = sim.signal("capture");
        let restore = sim.signal("restore");
        let idx = sim.signal("idx");
        let x3_map = sim.signal("x3_map");
        let q0 = sim.signal("q0");
        let q3 = sim.signal("q3");
        let q31 = sim.signal("q31");

        sim.modify(|io| {
            io.set(capture, 1u8);
            io.set(restore, 0u8);
            io.set(idx, 17u8);
            io.set(x3_map, 7u8);
        }).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| {
            io.set(capture, 0u8);
            io.set(restore, 1u8);
        }).unwrap();
        sim.tick(clk).unwrap();

        assert_eq!(sim.get(q0), 0u8.into());
        assert_eq!(sim.get(q3), 7u8.into());
        assert_eq!(sim.get(q31), 31u8.into());
    }

    fn test_dynamic_ff_array_partial_squash_preserves_head_and_branch(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Top (
    clk        : input  clock,
    set_en     : input  logic,
    set_idx    : input  logic<5>,
    squash_en  : input  logic,
    head_idx   : input  logic<5>,
    squash_idx : input  logic<5>,
    probe_idx  : input  logic<5>,
    probe_valid: output logic,
) {
    var valid: logic [32];

    always_ff (clk) {
        if set_en {
            valid[set_idx] = 1'b1;
        }
        if squash_en {
            let squash_age: logic<5> = squash_idx - head_idx;
            for i in 0..32 {
                let age: logic<5> = (i as 5) - head_idx;
                if age >: squash_age {
                    valid[i] = 1'b0;
                }
            }
        }
    }

    assign probe_valid = valid[probe_idx];
}
"#; }
        @build Simulator::builder(code, "Top");

        let clk = sim.event("clk");
        let set_en = sim.signal("set_en");
        let set_idx = sim.signal("set_idx");
        let squash_en = sim.signal("squash_en");
        let head_idx = sim.signal("head_idx");
        let squash_idx = sim.signal("squash_idx");
        let probe_idx = sim.signal("probe_idx");
        let probe_valid = sim.signal("probe_valid");

        for index in 0u8..32 {
            sim.modify(|io| {
                io.set(set_en, 1u8);
                io.set(set_idx, index);
                io.set(squash_en, 0u8);
            }).unwrap();
            sim.tick(clk).unwrap();
        }

        sim.modify(|io| {
            io.set(set_en, 0u8);
            io.set(squash_en, 1u8);
            io.set(head_idx, 18u8);
            io.set(squash_idx, 21u8);
        }).unwrap();
        sim.tick(clk).unwrap();

        for index in [18u8, 19, 20, 21] {
            sim.modify(|io| io.set(probe_idx, index)).unwrap();
            assert_eq!(
                sim.get(probe_valid),
                1u8.into(),
                "partial squash cleared preserved ROB slot {index}",
            );
        }
        for index in (22u8..32).chain(0u8..18) {
            sim.modify(|io| io.set(probe_idx, index)).unwrap();
            assert_eq!(
                sim.get(probe_valid),
                0u8.into(),
                "partial squash retained younger ROB slot {index}",
            );
        }
    }

    fn test_line_write_loop_updates_large_sparse_ff_array(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Top (
    clk  : input  clock,
    wen  : input  logic,
    waddr: input  logic<64>,
    data0: input  logic<64>,
    strb0: input  logic<8>,
    probe: input  logic<20>,
    q_lo : output logic<32>,
    q_hi : output logic<32>,
    q_dynamic: output logic<32>,
) {
    var mem  : logic<32> [1048576];
    var wdata: logic<64> [8];
    var wstrb: logic<8>  [8];
    always_comb {
        for i in 0..8 {
            wdata[i] = 0;
            wstrb[i] = 0;
        }
        wdata[0] = data0;
        wstrb[0] = strb0;
    }
    always_ff (clk) {
        if wen {
            for l in 0..8 {
                if wstrb[l] != 0 {
                    let w_lo : logic<20> = {waddr[21:6], l as 3, 1'b0};
                    let w_hi : logic<20> = {waddr[21:6], l as 3, 1'b1};
                    let w_old: logic<64> = {mem[w_hi], mem[w_lo]};
                    let w_new: logic<64> = {
                        if wstrb[l][7] ? wdata[l][63:56] : w_old[63:56],
                        if wstrb[l][6] ? wdata[l][55:48] : w_old[55:48],
                        if wstrb[l][5] ? wdata[l][47:40] : w_old[47:40],
                        if wstrb[l][4] ? wdata[l][39:32] : w_old[39:32],
                        if wstrb[l][3] ? wdata[l][31:24] : w_old[31:24],
                        if wstrb[l][2] ? wdata[l][23:16] : w_old[23:16],
                        if wstrb[l][1] ? wdata[l][15:8]  : w_old[15:8],
                        if wstrb[l][0] ? wdata[l][7:0]   : w_old[7:0]
                    };
                    mem[w_lo] = w_new[31:0];
                    mem[w_hi] = w_new[63:32];
                }
            }
        }
    }
    assign q_lo = mem[20'd1024];
    assign q_hi = mem[20'd1025];
    assign q_dynamic = mem[probe];
}
"#; }
        @build Simulator::builder(code, "Top");

        let clk = sim.event("clk");
        let wen = sim.signal("wen");
        let waddr = sim.signal("waddr");
        let data0 = sim.signal("data0");
        let strb0 = sim.signal("strb0");
        let probe = sim.signal("probe");
        let q_lo = sim.signal("q_lo");
        let q_hi = sim.signal("q_hi");
        let q_dynamic = sim.signal("q_dynamic");
        sim.modify(|io| {
            io.set(wen, 1u8);
            io.set(waddr, 0x0400_0000_8000_1000u64);
            io.set(data0, 1u64);
            io.set(strb0, 0x0fu8);
            io.set_wide(probe, BigUint::from(1024u32));
        }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q_lo), 1u32.into(), "low word was not committed");
        assert_eq!(sim.get(q_dynamic), 1u32.into(), "dynamic low read was not invalidated");

        sim.modify(|io| {
            io.set(data0, 0xaabb_ccdd_0000_0000u64);
            io.set(strb0, 0xf0u8);
            io.set_wide(probe, BigUint::from(1025u32));
        }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q_hi), 0xaabb_ccddu32.into(), "high word was not committed");
        assert_eq!(sim.get(q_dynamic), 0xaabb_ccddu32.into(), "dynamic high read was not invalidated");
    }

}
