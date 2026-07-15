use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn signed_divrem_i8(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    a: input i8,
    b: input i8,
    ub: input logic<8>,
    a5: input signed logic<5>,
    b5: input signed logic<5>,
    q: output i8,
    r: output i8,
    uq: output logic<8>,
    ur: output logic<8>,
    mixed_q: output logic<8>,
    mixed_r: output logic<8>,
    mixed5_q: output logic<8>,
    mixed5_r: output logic<8>,
    q5: output signed logic<5>,
    r5: output signed logic<5>,
) {
    assign q = a / b;
    assign r = a % b;
    assign uq = (a as u8) / (b as u8);
    assign ur = (a as u8) % (b as u8);
    assign mixed_q = a / ub;
    assign mixed_r = a % ub;
    assign mixed5_q = a5 / ub;
    assign mixed5_r = a5 % ub;
    assign q5 = a5 / b5;
    assign r5 = a5 % b5;
}
"#, "Top");

        let a = sim.signal("a");
        let b = sim.signal("b");
        let ub = sim.signal("ub");
        let a5 = sim.signal("a5");
        let b5 = sim.signal("b5");
        let q = sim.signal("q");
        let r = sim.signal("r");
        let uq = sim.signal("uq");
        let ur = sim.signal("ur");
        let mixed_q = sim.signal("mixed_q");
        let mixed_r = sim.signal("mixed_r");
        let mixed5_q = sim.signal("mixed5_q");
        let mixed5_r = sim.signal("mixed5_r");
        let q5 = sim.signal("q5");
        let r5 = sim.signal("r5");

        // Signed division truncates toward zero, and the remainder has the
        // dividend's sign. The explicitly unsigned expressions must retain
        // the same bits but use unsigned arithmetic.
        sim.modify(|io| {
            io.set(a, 0xf9u8); // -7
            io.set(b, 0x02u8); // 2
            io.set(ub, 0x02u8);
            io.set(a5, 0x19u8); // -7 in 5 bits
            io.set(b5, 0x02u8);
        })
        .unwrap();
        assert_eq!(sim.get(q), 0xfdu8.into()); // -3
        assert_eq!(sim.get(r), 0xffu8.into()); // -1
        assert_eq!(sim.get(uq), 124u8.into());
        assert_eq!(sim.get(ur), 1u8.into());
        // Mixed signed/unsigned operands select unsigned arithmetic.
        assert_eq!(sim.get(mixed_q), 124u8.into());
        assert_eq!(sim.get(mixed_r), 1u8.into());
        assert_eq!(sim.get(mixed5_q), 12u8.into()); // unsigned 5'b11001 / 2
        assert_eq!(sim.get(mixed5_r), 1u8.into());
        assert_eq!(sim.get(q5), 0x1du8.into()); // -3 in 5 bits
        assert_eq!(sim.get(r5), 0x1fu8.into()); // -1 in 5 bits

        sim.modify(|io| {
            io.set(a, 0x07u8); // 7
            io.set(b, 0xfeu8); // -2
        })
        .unwrap();
        assert_eq!(sim.get(q), 0xfdu8.into()); // -3
        assert_eq!(sim.get(r), 1u8.into());

        sim.modify(|io| {
            io.set(a, 0xf9u8); // -7
            io.set(b, 0xfeu8); // -2
        })
        .unwrap();
        assert_eq!(sim.get(q), 3u8.into());
        assert_eq!(sim.get(r), 0xffu8.into()); // -1

        // Fixed-width arithmetic wraps MIN / -1 instead of exposing the host
        // CPU/WebAssembly overflow trap.
        sim.modify(|io| {
            io.set(a, 0x80u8);
            io.set(b, 0xffu8);
        })
        .unwrap();
        assert_eq!(sim.get(q), 0x80u8.into());
        assert_eq!(sim.get(r), 0u8.into());

        sim.modify(|io| {
            io.set(a5, 0x10u8); // MIN for 5 bits
            io.set(b5, 0x1fu8); // -1
        })
        .unwrap();
        assert_eq!(sim.get(q5), 0x10u8.into());
        assert_eq!(sim.get(r5), 0u8.into());

        // Celox's established totalized integer semantics use zero for both
        // quotient and remainder when the divisor is zero.
        sim.modify(|io| {
            io.set(a, 0xf9u8);
            io.set(b, 0u8);
        })
        .unwrap();
        assert_eq!(sim.get(q), 0u8.into());
        assert_eq!(sim.get(r), 0u8.into());
    }

    fn signed_divrem_i64(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    a: input i64,
    b: input i64,
    q: output i64,
    r: output i64,
) {
    assign q = a / b;
    assign r = a % b;
}
"#, "Top");

        let a = sim.signal("a");
        let b = sim.signal("b");
        let q = sim.signal("q");
        let r = sim.signal("r");

        sim.modify(|io| {
            io.set(a, (-9i64) as u64);
            io.set(b, 4u64);
        })
        .unwrap();
        assert_eq!(sim.get(q), ((-2i64) as u64).into());
        assert_eq!(sim.get(r), ((-1i64) as u64).into());

        sim.modify(|io| {
            io.set(a, i64::MIN as u64);
            io.set(b, (-1i64) as u64);
        })
        .unwrap();
        assert_eq!(sim.get(q), (i64::MIN as u64).into());
        assert_eq!(sim.get(r), 0u64.into());
    }

    fn signed_divrem_i128(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    a: input signed logic<128>,
    b: input signed logic<128>,
    a65: input signed logic<65>,
    b65: input signed logic<65>,
    q: output signed logic<128>,
    r: output signed logic<128>,
    q65: output signed logic<65>,
    r65: output signed logic<65>,
) {
    assign q = a / b;
    assign r = a % b;
    assign q65 = a65 / b65;
    assign r65 = a65 % b65;
}
"#, "Top");

        let a = sim.signal("a");
        let b = sim.signal("b");
        let a65 = sim.signal("a65");
        let b65 = sim.signal("b65");
        let q = sim.signal("q");
        let r = sim.signal("r");
        let q65 = sim.signal("q65");
        let r65 = sim.signal("r65");

        let modulus65 = BigUint::from(1u8) << 65;

        sim.modify(|io| {
            io.set(a, u128::MAX - 99); // -100
            io.set(b, 7u128);
            io.set_wide(a65, &modulus65 - BigUint::from(100u8));
            io.set_wide(b65, BigUint::from(7u8));
        })
        .unwrap();
        assert_eq!(sim.get(q), (u128::MAX - 13).into()); // -14
        assert_eq!(sim.get(r), (u128::MAX - 1).into()); // -2
        assert_eq!(sim.get(q65), &modulus65 - BigUint::from(14u8));
        assert_eq!(sim.get(r65), &modulus65 - BigUint::from(2u8));

        let min = 1u128 << 127;
        sim.modify(|io| {
            io.set(a, min);
            io.set(b, u128::MAX); // -1
        })
        .unwrap();
        assert_eq!(sim.get(q), min.into());
        assert_eq!(sim.get(r), 0u128.into());

        sim.modify(|io| {
            io.set(a, u128::MAX - 99);
            io.set(b, 0u128);
        })
        .unwrap();
        assert_eq!(sim.get(q), 0u128.into());
        assert_eq!(sim.get(r), 0u128.into());
    }

    fn signed_divrem_always_ff(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    rst: input reset,
    a: input i8,
    b: input i8,
    q: output i8,
    r: output i8,
) {
    always_ff (clk, rst) {
        if_reset {
            q = 0;
            r = 0;
        } else {
            q = a / b;
            r = a % b;
        }
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let rst = sim.signal("rst");
        let a = sim.signal("a");
        let b = sim.signal("b");
        let q = sim.signal("q");
        let r = sim.signal("r");

        sim.modify(|io| {
            io.set(rst, 1u8);
            io.set(a, 0xf9u8); // -7
            io.set(b, 2u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), 0xfdu8.into());
        assert_eq!(sim.get(r), 0xffu8.into());
    }

    fn signed_divrem_four_state_unknown(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    a: input signed logic<128>,
    b: input signed logic<128>,
    q: output signed logic<128>,
    r: output signed logic<128>,
) {
    assign q = a / b;
    assign r = a % b;
}
"#, "Top").four_state(true);

        let a = sim.signal("a");
        let b = sim.signal("b");
        let q = sim.signal("q");
        let r = sim.signal("r");
        let all: BigUint =
            (BigUint::from(u64::MAX) << 64usize) | BigUint::from(u64::MAX);

        sim.modify(|io| {
            io.set_four_state(a, BigUint::from(100u8), BigUint::from(0u8));
            io.set_four_state(b, BigUint::from(7u8), BigUint::from(1u8));
        })
        .unwrap();

        assert_eq!(sim.get_four_state(q), (all.clone(), all.clone()));
        assert_eq!(sim.get_four_state(r), (all.clone(), all));
    }
}
