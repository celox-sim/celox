use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn test_interface_connection(sim) {
        @setup {
            let code = r#"
    interface Bus {
        var data: logic<8>;
        var valid: logic;
        modport producer {
            ..output
        }
        modport consumer {
            ..input
        }
    }

    module Producer (
        bus: modport Bus::producer
    ) {
        assign bus.data = 8'hAA;
        assign bus.valid = 1'b1;
    }

    module Consumer (
        bus: modport Bus::consumer,
        out_data: output logic<8>
    ) {
        assign out_data = bus.data;
    }

    module Top (
        out: output logic<8>
    ) {
        inst bus: Bus;
        inst p: Producer (
            bus: bus
        );
        inst c: Consumer (
            bus: bus,
            out_data: out
        );
    }
    "#;
            let top = "Top";
        }
        @build Simulator::builder(code, top);

        // Run simulation

        // Verify output
        let out_id = sim.signal("out");
        let out_val = sim.get(out_id);

        // 0xAA = 170
        assert_eq!(out_val, 170u32.into());
    }
}
