use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Parametric interface array: verify array_dims are populated for parametric-type members.
    fn test_parametric_interface_array(sim) {
        @omit_veryl;
        // The SystemVerilog frontend names the members of an interface port
        // `bus$data`, not `bus.data`.
        @ignore_on(sv);
        @setup { let code = r#"
interface Bus::<T: type> {
var data:  T;
var valid: logic;
modport consumer {
data:  input,
valid: input,
}
}
module Top (
bus: modport Bus::<u8>::consumer [2],
out: output u8,
) {
assign out = bus[0].data + bus[1].data;
}
"#; }
        @build Simulator::builder(code, "Top");
    let signals = sim.named_signals();

    let bus_data = signals
        .iter()
        .find(|s| s.name == "bus.data")
        .expect("bus.data not found");
    let bus_valid = signals
        .iter()
        .find(|s| s.name == "bus.valid")
        .expect("bus.valid not found");

    assert_eq!(
        bus_data.info.array_dims,
        vec![2],
        "bus.data should have array_dims [2]"
    );
    assert_eq!(
        bus_valid.info.array_dims,
        vec![2],
        "bus.valid should have array_dims [2]"
    );

    // For a [2] array of logic<8>, total signal width = 16
    assert_eq!(bus_data.signal.width, 16, "bus.data total signal width");
    assert_eq!(bus_valid.signal.width, 2, "bus.valid total signal width");

    }

}
