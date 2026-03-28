/// Tests that verify unimplemented paths panic instead of producing silent wrong results.
use celox::{BigUint, IOContext, SimulatorBuilder};

/// Wide dynamic shift (a >> idx with 128-bit a) is handled by the runtime
/// shift select chain, NOT by lower_wide_extract. Verify it works correctly.
#[test]
fn wide_dynamic_shift_works() {
    let code = r#"
        module Top (
            a: input logic<128>,
            idx: input logic<8>,
            y: output logic<128>
        ) {
            assign y = a >> idx;
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .build()
        .unwrap();
    let id_a = sim.signal("a");
    let id_idx = sim.signal("idx");
    let id_y = sim.signal("y");
    let val: BigUint = (BigUint::from(0xDEADu64) << 64) | BigUint::from(0xCAFEu64);
    sim.modify(|io: &mut IOContext| {
        io.set_wide(id_a, val);
        io.set(id_idx, 64u8);
    })
    .unwrap();
    assert_eq!(sim.get(id_y), BigUint::from(0xDEADu64));
}

/// Dynamic offset Store with 4-state mask: array write with variable index
/// on a logic (4-state) type must correctly store the mask.
#[test]
fn dynamic_mask_store() {
    let code = r#"
        module Top (
            clk: input '_ clock,
            idx: input logic<4>,
            val: input logic<8>,
            out: output logic<8>,
        ) {
            var mem: logic<8> [16];
            always_ff (clk) {
                mem[idx] = val;
            }
            assign out = mem[idx];
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let clk_event = sim.event("clk");
    let id_idx = sim.signal("idx");
    let id_val = sim.signal("val");
    let id_out = sim.signal("out");

    // Write val with partial X mask at index 3
    sim.modify(|io: &mut IOContext| {
        io.set(id_idx, 3u8);
        io.set_four_state(id_val, BigUint::from(0xABu32), BigUint::from(0x0Fu32));
    })
    .unwrap();
    sim.tick(clk_event).unwrap();

    // Read back: should see the same mask
    sim.modify(|io: &mut IOContext| {
        io.set(id_idx, 3u8);
    })
    .unwrap();
    let (v, m) = sim.get_four_state(id_out);
    // FF path stores raw value (no v|=m normalization on this path)
    assert_eq!(v, BigUint::from(0xABu32), "dynamic mask store: value");
    assert_eq!(m, BigUint::from(0x0Fu32), "dynamic mask store: mask");
}
