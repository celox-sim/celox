use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

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
}
