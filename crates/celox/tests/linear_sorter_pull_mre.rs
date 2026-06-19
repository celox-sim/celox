use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

const DEPTH: usize = 100;
const SOURCE: &str = include_str!("fixtures/linear_sorter_pull_mre.veryl");

fn push_values<B: celox::SimBackend>(sim: &mut celox::Simulator<B>, clk: B::Event, values: &[u16]) {
    let push = sim.signal("push");
    let pop = sim.signal("pop");
    let d_in = sim.signal("d_in");

    sim.modify(|io| {
        io.set(pop, 0u8);
        io.set(push, 1u8);
    })
    .unwrap();
    for &value in values {
        sim.modify(|io| io.set(d_in, value)).unwrap();
        sim.tick(clk).unwrap();
    }
    sim.modify(|io| io.set(push, 0u8)).unwrap();
    sim.tick(clk).unwrap();
}

fn pull_until_empty<B: celox::SimBackend>(
    sim: &mut celox::Simulator<B>,
    clk: B::Event,
    max_cycles: usize,
) -> Vec<u16> {
    let pop = sim.signal("pop");
    let empty = sim.signal("empty");
    let d_out = sim.signal("d_out");
    let mut out = Vec::new();

    for _ in 0..max_cycles {
        if sim.get_as::<u8>(empty) != 0 {
            break;
        }
        out.push(sim.get_as::<u16>(d_out));
        sim.modify(|io| io.set(pop, 1u8)).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(pop, 0u8)).unwrap();
        sim.tick(clk).unwrap();
    }

    out
}

all_backends! {
fn bit_array_index_64_ff_roundtrips(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module BitArrayIndex64 (
    clk: input clock,
    rst: input reset,
    d: input logic,
    q: output logic,
) {
    var bits: logic [65];

    always_ff {
        if_reset {
            bits[0] = 0;
            bits[64] = 0;
        } else {
            bits[0] = d;
            bits[64] = bits[0];
        }
    }

    assign q = bits[64];
}
"#, "BitArrayIndex64").reset_type(celox::ResetType::AsyncLow);

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 0u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 0);

    sim.modify(|io| io.set(d, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 0);
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 1);
}

fn word_array_index_64_ff_roundtrips(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module WordArrayIndex64 (
    clk: input clock,
    rst: input reset,
    d: input logic<16>,
    q: output logic<16>,
) {
    var words: logic<16> [65];

    always_ff {
        if_reset {
            words[0] = 0;
            words[64] = 0;
        } else {
            words[0] = d;
            words[64] = words[0];
        }
    }

    assign q = words[64];
}
"#, "WordArrayIndex64").reset_type(celox::ResetType::AsyncLow);

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 0u16);
    }).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u16>(q), 0);

    sim.modify(|io| io.set(d, 0x1234u16)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u16>(q), 0);
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u16>(q), 0x1234);
}

fn linear_sorter_pull_late_minima_drain_once_in_sorted_order(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(SOURCE, "LinearSorterPullMreU16")
        .param("DEPTH", DEPTH as u64)
        .reset_type(celox::ResetType::AsyncLow);

    let rst = sim.signal("rst");
    let clk = sim.event("clk");
    let clear = sim.signal("clear");
    let push = sim.signal("push");
    let pop = sim.signal("pop");
    let d_in = sim.signal("d_in");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(clear, 0u8);
        io.set(push, 0u8);
        io.set(pop, 0u8);
        io.set(d_in, 0u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();

    let mut input: Vec<u16> = (0..DEPTH - 4).map(|i| 1000 + i as u16).collect();
    input.extend([1, 2, 3, 4]);
    let mut expected = input.clone();
    expected.sort();

    push_values(&mut sim, clk, &input);
    let got = pull_until_empty(&mut sim, clk, DEPTH + 8);

    assert_eq!(got, expected);
}
}
