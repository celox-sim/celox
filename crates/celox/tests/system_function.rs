use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    #[ignore = "direct $onehot in always_comb currently evaluates incorrectly before Celox system-function lowering"]
    fn test_direct_comb_onehot_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    d: input logic<8>,
    q: output logic,
) {
    always_comb {
        q = $onehot(d);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            assert_eq!(
                sim.get_as::<u8>(q),
                u8::from(value.count_ones() == 1),
                "value={value:#010b}",
            );
        }
    }

    fn test_comb_function_body_onehot_system_function(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    d: input logic<8>,
    q: output logic,
) {
    function is_onehot (
        x: input logic<8>,
    ) -> logic {
        return $onehot(x);
    }

    always_comb {
        q = is_onehot(d);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            assert_eq!(
                sim.get_as::<u8>(q),
                u8::from(value.count_ones() == 1),
                "value={value:#010b}",
            );
        }
    }

    #[ignore = "direct $onehot in always_ff is folded to 1'h0 by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_onehot_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic,
) {
    always_ff (clk) {
        q = $onehot(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(
                sim.get_as::<u8>(q),
                u8::from(value.count_ones() == 1),
                "value={value:#010b}",
            );
        }
    }

    fn test_ff_function_body_onehot_system_function(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic,
) {
    function is_onehot (
        x: input logic<8>,
    ) -> logic {
        return $onehot(x);
    }

    always_ff (clk) {
        q = is_onehot(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(
                sim.get_as::<u8>(q),
                u8::from(value.count_ones() == 1),
                "value={value:#010b}",
            );
        }
    }
}
