//! IEEE 1800-2023 11.4.10: an X/Z anywhere in the count makes the result X.
use celox::{BigUint, SimBackend, Simulator};

fn check<B: SimBackend>(mut sim: Simulator<B>, width: usize) {
    let a = sim.signal("a");
    let count = sim.signal("count");
    let all = (BigUint::from(1u8) << width) - 1u8;
    sim.modify(|io| io.set_wide(a, 1u8.into())).unwrap();
    for bit in [0usize, 63, 64, 127] {
        let mask = BigUint::from(1u8) << bit;
        for payload in [BigUint::default(), mask.clone()] {
            sim.modify(|io| io.set_four_state(count, payload.clone(), mask.clone()))
                .unwrap();
            for name in ["shl", "shr", "sar"] {
                assert_eq!(
                    sim.get_four_state(sim.signal(name)),
                    (all.clone(), all.clone()),
                    "{name} width={width} count bit={bit} payload={payload:x}"
                );
            }
        }
        // A known count must also clear the previous unknown result.
        sim.modify(|io| io.set_four_state(count, 0u8.into(), 0u8.into()))
            .unwrap();
        for name in ["shl", "shr", "sar"] {
            assert_eq!(
                sim.get_four_state(sim.signal(name)),
                (1u8.into(), 0u8.into())
            );
        }
    }
}

macro_rules! backend_test {
    ($name:ident, $build:ident) => {
        #[test]
        fn $name() {
            for width in [8, 65, 129, 257] {
                let code = format!(
                    "module Top (a: input signed logic<{width}>, count: input logic<128>,
                        shl: output logic<{width}>, shr: output logic<{width}>, sar: output signed logic<{width}>) {{
                        assign shl = a << count;
                        assign shr = a >> count;
                        assign sar = a >>> count;
                    }}"
                );
                let sim = Simulator::builder(&code, "Top")
                    .four_state(true)
                    .$build()
                    .unwrap();
                check(sim, width);
            }
        }
    };
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
backend_test!(native, build_native);
backend_test!(cranelift, build_cranelift);
backend_test!(wasm, build_wasm);
backend_test!(interpreter, build_interpreter);
