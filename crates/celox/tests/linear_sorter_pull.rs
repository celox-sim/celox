use celox::Simulator;

fn sorter_code() -> &'static str {
    r#"
proto package SorterItemProto {
    type Item;
}

package SorterItemPlain::<W: u32> for SorterItemProto {
    type Item = logic<W>;
}

module SorterCellPull::<PKG: SorterItemProto> (
    clk     : input  clock    ,
    rst     : input  reset    ,
    push    : input  logic    ,
    pop     : input  logic    ,
    d_in    : input  PKG::Item,
    prev_val: input  PKG::Item,
    next_val: input  PKG::Item,
    lt_prev : input  logic    ,
    lt_next : input  logic    ,
    pp_lt   : input  logic    ,
    o_reg   : output PKG::Item,
    lt      : output logic    ,
) {
    var r_val: PKG::Item;

    assign lt    = d_in <: r_val;
    assign o_reg = r_val;

    always_ff {
        if_reset {
            r_val = -1;
        } else if push && !pop {
            if lt && !lt_prev {
                r_val = d_in;
            } else if lt {
                r_val = prev_val;
            }
        } else if pop && !push {
            r_val = next_val;
        } else if push && pop {
            if lt_next && !pp_lt {
                r_val = d_in;
            } else if !lt_next {
                r_val = next_val;
            }
        }
    }
}

module LinearSorterPull::<PKG: SorterItemProto> #(
    param DEPTH: u32 = 8,
) (
    clk  : input  clock    ,
    rst  : input  reset    ,
    push : input  logic    ,
    pop  : input  logic    ,
    d_in : input  PKG::Item,
    d_out: output PKG::Item,
    empty: output logic    ,
) {
    var cell_lt  : logic         [DEPTH + 2];
    var cell_reg : PKG::Item     [DEPTH + 2];
    var pp_lt_sig: logic         [DEPTH]    ;
    var count    : logic    <32>            ;

    assign cell_lt[0]          = 0;
    assign cell_reg[0]         = 0;
    assign cell_lt[DEPTH + 1]  = 1;
    assign cell_reg[DEPTH + 1] = -1;

    assign pp_lt_sig[0] = 0;
    for i in 1..DEPTH :g_pp {
        assign pp_lt_sig[i] = cell_lt[i + 1];
    }

    for i in 0..DEPTH :g_cel {
        inst s: SorterCellPull::<PKG> (
            clk                      ,
            rst                      ,
            push                     ,
            pop                      ,
            d_in                     ,
            prev_val: cell_reg[i]    ,
            next_val: cell_reg[i + 2],
            lt_prev : cell_lt[i]     ,
            lt_next : cell_lt[i + 2] ,
            pp_lt   : pp_lt_sig[i]   ,
            o_reg   : cell_reg[i + 1],
            lt      : cell_lt[i + 1] ,
        );
    }

    assign d_out = cell_reg[1];

    always_ff {
        if_reset {
            count = 0;
        } else if push && !pop {
            count = count + 1;
        } else if pop && !push {
            if count != 0 {
                count = count - 1;
            }
        }
    }

    assign empty = count == 0;
}

pub module Top #(
    param DEPTH: u32 = 4,
) (
    clk  : input  clock    ,
    rst  : input  reset    ,
    push : input  logic    ,
    pop  : input  logic    ,
    d_in : input  logic<16>,
    d_out: output logic<16>,
    empty: output logic    ,
) {
    inst s: LinearSorterPull::<SorterItemPlain::<16>> #(
        DEPTH: DEPTH,
    ) (
        clk  ,
        rst  ,
        push ,
        pop  ,
        d_in ,
        d_out,
        empty,
    );
}
"#
}

#[test]
fn test_sorter_push_pop_empty() {
    let mut sim = Simulator::builder(sorter_code(), "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let push = sim.signal("push");
    let pop = sim.signal("pop");
    let d_in = sim.signal("d_in");
    let d_out = sim.signal("d_out");
    let empty = sim.signal("empty");

    // Reset (AsyncLow: active when rst=0)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(push, 0u8);
        io.set(pop, 0u8);
        io.set(d_in, 0u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 1u32.into(), "Should be empty after reset");

    // Deactivate reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 1u32.into(), "Still empty, no push");

    // Push value 100
    sim.modify(|io| {
        io.set(push, 1u8);
        io.set(d_in, 100u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(push, 0u8)).unwrap();
    sim.tick(clk).unwrap(); // settle
    assert_eq!(sim.get(empty), 0u32.into(), "Not empty after push");
    assert_eq!(
        sim.get(d_out),
        100u32.into(),
        "d_out should be 100 (minimum)"
    );

    // Push value 50 (smaller, becomes new min)
    sim.modify(|io| {
        io.set(push, 1u8);
        io.set(d_in, 50u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(push, 0u8)).unwrap();
    sim.tick(clk).unwrap(); // settle
    assert_eq!(
        sim.get(d_out),
        50u32.into(),
        "d_out should be 50 (new minimum)"
    );

    // Pop (removes minimum = 50)
    sim.modify(|io| io.set(pop, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(pop, 0u8)).unwrap();
    sim.tick(clk).unwrap(); // settle
    assert_eq!(
        sim.get(d_out),
        100u32.into(),
        "d_out should be 100 after popping 50"
    );
    assert_eq!(sim.get(empty), 0u32.into(), "Not empty, still has 100");

    // Pop again (removes 100, now empty)
    sim.modify(|io| io.set(pop, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(pop, 0u8)).unwrap();
    sim.tick(clk).unwrap(); // settle
    assert_eq!(
        sim.get(empty),
        1u32.into(),
        "Should be empty after popping all elements"
    );
}

/// Focused test: push one element, then pop it.
/// The empty flag should go 1 → 0 → 1 across the three phases.
#[test]
fn test_sorter_empty_flag_single_push_pop() {
    let mut sim = Simulator::builder(sorter_code(), "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let push = sim.signal("push");
    let pop = sim.signal("pop");
    let d_in = sim.signal("d_in");
    let empty = sim.signal("empty");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(push, 0u8);
        io.set(pop, 0u8);
        io.set(d_in, 0u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 1u32.into(), "empty=1 after reset");

    // Deactivate reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Push one value
    sim.modify(|io| {
        io.set(push, 1u8);
        io.set(d_in, 42u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    // count: 0 → 1, empty should become 0
    assert_eq!(
        sim.get(empty),
        0u32.into(),
        "empty should be 0 after pushing (count=1)"
    );

    // Deassert push
    sim.modify(|io| io.set(push, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 0u32.into(), "still not empty");

    // Pop
    sim.modify(|io| io.set(pop, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    // count: 1 → 0, empty should become 1
    assert_eq!(
        sim.get(empty),
        1u32.into(),
        "empty should be 1 after popping last element (count=0)"
    );

    // Deassert pop
    sim.modify(|io| io.set(pop, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 1u32.into(), "still empty");
}

/// Test: push+pop simultaneously — count should not change.
#[test]
fn test_sorter_simultaneous_push_pop() {
    let mut sim = Simulator::builder(sorter_code(), "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let push = sim.signal("push");
    let pop = sim.signal("pop");
    let d_in = sim.signal("d_in");
    let empty = sim.signal("empty");
    let d_out = sim.signal("d_out");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(push, 0u8);
        io.set(pop, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Deactivate reset, push first element
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(push, 1u8);
        io.set(d_in, 200u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(push, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 0u32.into());

    // Push second element
    sim.modify(|io| {
        io.set(push, 1u8);
        io.set(d_in, 300u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(push, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(d_out), 200u32.into(), "min should be 200");

    // Simultaneous push+pop: pop 200, push 150
    sim.modify(|io| {
        io.set(push, 1u8);
        io.set(pop, 1u8);
        io.set(d_in, 150u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(push, 0u8);
        io.set(pop, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    // count should still be 2 (push+pop doesn't change count)
    assert_eq!(sim.get(empty), 0u32.into(), "count unchanged by push+pop");
    assert_eq!(
        sim.get(d_out),
        150u32.into(),
        "min should be 150 after replacing 200 with 150"
    );
}
