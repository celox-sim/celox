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

/// Dynamic offset Store with 4-state mask.
/// The native backend does not yet support writing masks at dynamic offsets;
/// it must panic rather than silently skip the mask store.
#[test]
#[should_panic(expected = "dynamic offset 4-state mask Store")]
fn unimplemented_dynamic_mask_store() {
    // Array write with variable index on a logic (4-state) type.
    // The SIR generates Store(addr, Dynamic(idx_reg), ...) and the mask
    // store path hits the unimplemented! branch.
    let code = r#"
        module Top (
            clk: input '_ clock,
            idx: input logic<4>,
            val: input logic<8>,
        ) {
            var mem: logic<8> [16];
            always_ff (clk) {
                mem[idx] = val;
            }
        }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let clk_event = sim.event("clk");
    let id_idx = sim.signal("idx");
    let id_val = sim.signal("val");
    sim.modify(|io: &mut IOContext| {
        io.set(id_idx, 3u8);
        io.set_four_state(id_val, BigUint::from(0xABu32), BigUint::from(0x0Fu32));
    })
    .unwrap();
    // Rising edge triggers the FF write
    sim.tick(clk_event).unwrap();
}
