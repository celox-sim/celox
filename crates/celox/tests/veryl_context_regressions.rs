#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn ff_loop_bound_keeps_its_type(sim) {
        @setup { let code = r#"
            module Top #(
                param UNSIGNED_END: u32 = 2,
                param SIGNED_END: i32 = 2,
            ) (
                clk: input clock,
                lo: input i32,
                hi: input u32,
                unsigned_count: output u32,
                signed_count: output u32,
                dynamic_count: output u32,
                inclusive_count: output u32,
            ) {
                always_ff (clk) {
                    var cu: u32;
                    var cs: u32;
                    var cd: u32;
                    var ci: u32;
                    cu = 0;
                    for j in lo..UNSIGNED_END { cu += 1 + (j - j); }
                    cs = 0;
                    for j in lo..SIGNED_END { cs += 1 + (j - j); }
                    cd = 0;
                    for j in lo..hi { cd += 1 + (j - j); }
                    ci = 0;
                    for j in lo..=UNSIGNED_END { ci += 1 + (j - j); }
                    unsigned_count = cu;
                    signed_count = cs;
                    dynamic_count = cd;
                    inclusive_count = ci;
                }
            }
        "#; }
        @build celox::Simulator::builder(code, "Top");
        let clk = sim.event("clk");
        let lo = sim.signal("lo");
        let hi = sim.signal("hi");
        sim.modify(|io| {
            io.set(lo, -2i32);
            io.set(hi, 2u32);
        }).unwrap();
        sim.tick(clk).unwrap();
        for (name, expected) in [
            ("unsigned_count", 0u32),
            ("signed_count", 4u32),
            ("dynamic_count", 0u32),
            ("inclusive_count", 0u32),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), expected.into(), "{name}");
        }
    }
}
