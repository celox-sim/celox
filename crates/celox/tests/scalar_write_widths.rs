use celox::{SimBackend, Simulator};

fn check<B: SimBackend>(mut sim: Simulator<B>, four_state: bool) {
    for width in [
        1, 7, 8, 9, 15, 16, 17, 24, 25, 31, 32, 33, 40, 48, 56, 63, 64, 65, 127, 128,
    ] {
        let signal = sim.signal(&format!("s{width}"));
        let sentinel = sim.signal("sentinel");
        let all: celox::BigUint = (celox::BigUint::from(1u8) << width) - 1u8;
        sim.modify(|io| {
            io.set(sentinel, 0xa5u8);
            if four_state {
                io.set_four_state(signal, all.clone(), all.clone());
            }
            io.set(signal, u128::MAX);
        })
        .unwrap();
        assert_eq!(sim.get(signal), all, "width {width}");
        assert_eq!(sim.get(sentinel), 0xa5u8.into());
        assert_eq!(
            sim.get_as::<u128>(signal),
            ((1u128 << (width - 1)) - 1) * 2 + 1
        );
        let (ptr, len) = sim.memory_as_ptr();
        let bytes = (width as usize).div_ceil(8);
        assert!(signal.offset + bytes <= len);
        // The simulator remains alive and is not mutated during this read.
        let last = unsafe { *ptr.add(signal.offset + bytes - 1) };
        assert_eq!(
            last,
            if width % 8 == 0 {
                0xff
            } else {
                (1u8 << (width % 8)) - 1
            }
        );
        assert_eq!(sim.get_four_state(signal).1, 0u8.into());
        sim.modify(|io| io.set(signal, 1)).unwrap(); // default i32 literal
        assert_eq!(sim.get(signal), 1u8.into(), "width {width}");
        sim.modify(|io| io.set(signal, 0u128)).unwrap();
        assert_eq!(sim.get(signal), 0u8.into());
    }
}

#[test]
fn scalar_writes_truncate_without_touching_neighbours_or_masks() {
    let mut code = "module Top (sentinel: input logic<8>".to_string();
    for width in [
        1, 7, 8, 9, 15, 16, 17, 24, 25, 31, 32, 33, 40, 48, 56, 63, 64, 65, 127, 128,
    ] {
        code.push_str(&format!(", s{width}: input logic<{width}>"));
    }
    code.push_str(") {}");
    for four_state in [false, true] {
        let build = || {
            Simulator::builder(&code, "Top")
                .four_state(four_state)
                .dead_store_policy(celox::DeadStorePolicy::PreserveAllPorts)
        };
        check(build().build_interpreter().unwrap(), four_state);
        check(build().build_cranelift().unwrap(), four_state);
        check(build().build_wasm().unwrap(), four_state);
        check(build().build_tiered().unwrap(), four_state);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(build().build_native().unwrap(), four_state);
    }
}
