#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Two independent clock domains: each FF only advances on its own clock.
    fn test_independent_clock_domains(sim) {
        @case "multi_clock::test_independent_clock_domains";
    }

    // A counter in one clock domain feeding into another (CDC pattern).
    // Tests that domains are truly independent.
    fn test_clock_domain_crossing_pattern(sim) {
        @case "multi_clock::test_clock_domain_crossing_pattern";
    }

    // FF with separate clocks and separate resets.
    fn test_separate_resets_per_domain(sim) {
        @case "multi_clock::test_separate_resets_per_domain";
    }
}

mod tick_events {
    use celox::{Simulator, SimulatorBuilder};

    // `sampled` reads `count` on the shared edge. A coincident edge must let every
    // domain read the pre-edge state; serialized ticks would let whichever
    // domain runs second see the other's update.
    const DIVIDED_CLOCKS: &str = r#"
module Top (
    clk_fast: input 'a clock,
    clk_slow: input 'b clock,
    rst: input 'a reset,
    count: output 'a logic<8>,
    sampled: output 'b logic<8>,
) {
    always_ff (clk_fast, rst) {
        if_reset {
            count = 0;
        } else {
            count = count + 1;
        }
    }
    unsafe (cdc) {
        always_ff (clk_slow) {
            sampled = count;
        }
    }
}
"#;

    fn builder() -> SimulatorBuilder<'static, Simulator> {
        Simulator::builder(DIVIDED_CLOCKS, "Top")
    }

    macro_rules! check_coincident_edges {
        ($sim:expr) => {{
            let mut sim = $sim;
            let fast = sim.event("clk_fast");
            let slow = sim.event("clk_slow");
            let rst = sim.signal("rst");
            let count = sim.signal("count");
            let sampled = sim.signal("sampled");
            // Veryl's default reset is asynchronous and active low.
            sim.modify(|io| io.set(rst, 0u8)).unwrap();
            sim.tick(fast).unwrap();
            sim.modify(|io| io.set(rst, 1u8)).unwrap();
            for cycle in 1..=8u64 {
                if cycle % 2 == 0 {
                    // Order inside the slice does not matter.
                    let events = if cycle % 4 == 0 {
                        [fast, slow]
                    } else {
                        [slow, fast]
                    };
                    sim.tick_events(&events).unwrap();
                    assert_eq!(
                        sim.get(sampled),
                        (cycle - 1).into(),
                        "sampled at cycle {cycle}"
                    );
                } else {
                    sim.tick_events(&[fast]).unwrap();
                }
                assert_eq!(sim.get(count), cycle.into(), "count at cycle {cycle}");
            }
            // A repeated domain is applied once.
            sim.tick_events(&[fast, fast]).unwrap();
            assert_eq!(sim.get(count), 9u64.into());
            sim.tick_events(&[]).unwrap();
            assert_eq!(sim.get(count), 9u64.into());
        }};
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn native() {
        check_coincident_edges!(builder().build_native().unwrap());
    }

    #[test]
    fn cranelift() {
        check_coincident_edges!(builder().build_cranelift().unwrap());
    }

    #[test]
    fn interpreter() {
        check_coincident_edges!(builder().build_interpreter().unwrap());
    }

    #[test]
    fn wasm() {
        check_coincident_edges!(builder().build_wasm().unwrap());
    }

    // A design with one active domain has no split kernels; repeated
    // events of that domain are one ordinary tick, and a clock that drives
    // no FF is a no-op alongside it.
    #[test]
    fn single_domain() {
        let mut sim = Simulator::builder(
            r#"
module Top (
    clk: input 'a clock,
    idle_clk: input 'b clock,
    rst: input 'a reset,
    count: output 'a logic<8>,
) {
    always_ff (clk, rst) {
        if_reset {
            count = 0;
        } else {
            count = count + 1;
        }
    }
}
"#,
            "Top",
        )
        .build()
        .unwrap();
        let clk = sim.event("clk");
        let rst = sim.signal("rst");
        let count = sim.signal("count");
        sim.modify(|io| io.set(rst, 0u8)).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(rst, 1u8)).unwrap();
        sim.tick_events(&[clk, clk]).unwrap();
        assert_eq!(sim.get(count), 1u64.into());
        let idle = sim.event("idle_clk");
        sim.tick_events(&[clk, idle]).unwrap();
        assert_eq!(sim.get(count), 2u64.into());
        sim.tick_events(&[idle, clk]).unwrap();
        assert_eq!(sim.get(count), 3u64.into());
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn native_o2() {
        check_coincident_edges!(
            builder()
                .opt_level(celox::OptLevel::O2)
                .build_native()
                .unwrap()
        );
    }
}
