use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// A 100-bit value's final 36-bit chunk is read with a rounded 64-bit load,
// whose upper bytes belong to the byte-sized inputs packed after it (and, in
// four-state mode, to the mask). Those bytes must not reach the comparison.
const CODE: &str = r#"
module Top (
    a: input logic<100>,
    b0: input logic<8>,
    b1: input logic<8>,
    b2: input logic<8>,
    c: input bit<100>,
    d0: input bit<8>,
    d1: input bit<8>,
    d2: input bit<8>,
    o: output logic,
    p: output logic,
) {
    assign o = a == 100'd0;
    assign p = c == 100'd0;
}
"#;

macro_rules! partial_chunk_case {
    ($name:ident, $four_state:literal) => {
        all_backends! {
            fn $name(sim) {
                @build Simulator::builder(CODE, "Top").four_state($four_state);
                let a = sim.signal("a");
                let c = sim.signal("c");
                let o = sim.signal("o");
                let p = sim.signal("p");
                let neighbours = ["b0", "b1", "b2", "d0", "d1", "d2"].map(|n| sim.signal(n));
                let high = BigUint::from(1u8) << 99;
                for (value, expected) in [(BigUint::from(0u8), 1u8), (high, 0)] {
                    sim.modify(|io| {
                        io.set_wide(a, value.clone());
                        io.set_wide(c, value.clone());
                        for n in neighbours {
                            io.set(n, 0xffu8);
                        }
                    })
                    .unwrap();
                    for signal in [o, p] {
                        assert_eq!(
                            sim.get_four_state(signal),
                            (BigUint::from(expected), BigUint::from(0u8)),
                            "value={value:#x}"
                        );
                    }
                }
            }
        }
    };
}

partial_chunk_case!(two_state_wide_compare_ignores_adjacent_bytes, false);
partial_chunk_case!(four_state_wide_compare_ignores_adjacent_bytes, true);
