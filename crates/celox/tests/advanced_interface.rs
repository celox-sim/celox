use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Interface with multiple modport signals and bidirectional data flow.
    fn test_interface_bidirectional(sim) {
        @setup { let code = r#"
interface Handshake {
var req:  logic;
var ack:  logic;
var data: logic<8>;
modport master {
req:  output,
ack:  input,
data: output,
}
modport slave {
req:  input,
ack:  output,
data: input,
}
}
module Master (
bus:  modport Handshake::master,
send: input logic,
din:  input logic<8>
) {
assign bus.req  = send;
assign bus.data = din;
}
module Slave (
bus:  modport Handshake::slave,
dout: output logic<8>,
got:  output logic
) {
assign got  = bus.req;
assign dout = bus.data;
assign bus.ack = bus.req;
}
module Top (
send: input  logic,
din:  input  logic<8>,
dout: output logic<8>,
got:  output logic
) {
inst hs: Handshake;
inst m: Master (
bus:  hs,
send: send,
din:  din,
);
inst s: Slave (
bus:  hs,
dout: dout,
got:  got,
);
}
"#; }
        @build Simulator::builder(code, "Top");
    let send = sim.signal("send");
    let din = sim.signal("din");
    let dout = sim.signal("dout");
    let got = sim.signal("got");

    // No request yet
    sim.modify(|io| {
        io.set(send, 0u8);
        io.set(din, 0x42u8);
    })
    .unwrap();
    assert_eq!(sim.get(got), 0u8.into());

    // Send a request with data
    sim.modify(|io| {
        io.set(send, 1u8);
        io.set(din, 0xBBu8);
    })
    .unwrap();
    assert_eq!(sim.get(got), 1u8.into());
    assert_eq!(sim.get(dout), 0xBBu8.into());

    }

    // Multiple interface instances used in parallel.
    fn test_multiple_interface_instances(sim) {
        @setup { let code = r#"
interface DataBus {
var data: logic<8>;
modport writer {
data: output,
}
modport reader {
data: input,
}
}
module Writer (
bus: modport DataBus::writer,
val: input logic<8>
) {
assign bus.data = val;
}
module Reader (
bus: modport DataBus::reader,
out: output logic<8>
) {
assign out = bus.data;
}
module Top (
v0:  input  logic<8>,
v1:  input  logic<8>,
o0:  output logic<8>,
o1:  output logic<8>
) {
inst bus0: DataBus;
inst bus1: DataBus;
inst w0: Writer (bus: bus0, val: v0);
inst w1: Writer (bus: bus1, val: v1);
inst r0: Reader (bus: bus0, out: o0);
inst r1: Reader (bus: bus1, out: o1);
}
"#; }
        @build Simulator::builder(code, "Top");
    let v0 = sim.signal("v0");
    let v1 = sim.signal("v1");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");

    sim.modify(|io| {
        io.set(v0, 0x11u8);
        io.set(v1, 0x22u8);
    })
    .unwrap();
    assert_eq!(sim.get(o0), 0x11u8.into());
    assert_eq!(sim.get(o1), 0x22u8.into());

    sim.modify(|io| {
        io.set(v0, 0xFFu8);
        io.set(v1, 0x00u8);
    })
    .unwrap();
    assert_eq!(sim.get(o0), 0xFFu8.into());
    assert_eq!(sim.get(o1), 0x00u8.into());

    }

    // Interface with wide (multi-bit) signals.
    fn test_interface_wide_signal(sim) {
        @setup { let code = r#"
interface WideBus {
var data: logic<32>;
modport src {
data: output,
}
modport dst {
data: input,
}
}
module Source (
bus: modport WideBus::src,
a:   input logic<16>,
b:   input logic<16>
) {
assign bus.data = {a, b};
}
module Sink (
bus: modport WideBus::dst,
out: output logic<32>
) {
assign out = bus.data;
}
module Top (
a:   input  logic<16>,
b:   input  logic<16>,
out: output logic<32>
) {
inst wb: WideBus;
inst src_inst: Source (bus: wb, a: a, b: b);
inst dst_inst: Sink   (bus: wb, out: out);
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let out = sim.signal("out");

    sim.modify(|io| {
        io.set(a, 0x1234u16);
        io.set(b, 0x5678u16);
    })
    .unwrap();
    // Concatenation {a, b}: a is MSB, b is LSB → 0x12345678
    assert_eq!(sim.get(out), 0x1234_5678u32.into());

    }

    // Parametric interface array: verify array_dims are populated for parametric-type members.
    fn test_parametric_interface_array(sim) {
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
assign out = bus.data[0] + bus.data[1];
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

    // Transitive generics: type parameter flows through interface → child module → top.
    //
    // Tests that generic type parameters are correctly propagated across multiple
    // levels of the module hierarchy (a pattern that has been buggy in the past).
    fn test_transitive_generics(sim) {
        @setup { let code = r#"
interface DataBus::<T: type> {
var data: T;
modport source {
data: output,
}
modport sink {
data: input,
}
}
// Inner module: receives generic interface, adds 1
module Adder::<T: type> (
inp: modport DataBus::<T>::sink,
out: modport DataBus::<T>::source,
) {
assign out.data = inp.data + 1 as T;
}
// Top: instantiates Adder with concrete u16, chains two stages
module Top (
i_val:  input  logic<16>,
o_val:  output logic<16>,
) {
inst a_bus: DataBus::<u16>;
inst b_bus: DataBus::<u16>;
always_comb {
a_bus.data = i_val;
}
inst u_add1: Adder::<u16> (
inp: a_bus,
out: b_bus,
);
assign o_val = b_bus.data;
}
"#; }
        @build Simulator::builder(code, "Top");
    let i_val = sim.signal("i_val");
    let o_val = sim.signal("o_val");

    // 0 + 1 = 1
    sim.modify(|io| io.set(i_val, 0u16)).unwrap();
    assert_eq!(sim.get_as::<u16>(o_val), 1);

    // 100 + 1 = 101
    sim.modify(|io| io.set(i_val, 100u16)).unwrap();
    assert_eq!(sim.get_as::<u16>(o_val), 101);

    // 0xFFFF + 1 = 0 (wrap)
    sim.modify(|io| io.set(i_val, 0xFFFFu16)).unwrap();
    assert_eq!(sim.get_as::<u16>(o_val), 0, "u16 should wrap around");

    }

    // Two-level transitive generics: Top → Mid::<T> → Leaf::<T>, all with concrete type at Top.
    fn test_transitive_generics_two_level(sim) {
        @setup { let code = r#"
module Leaf::<T: type> (
i_a: input T,
i_b: input T,
o_sum: output T,
) {
assign o_sum = i_a + i_b;
}
module Mid::<T: type> (
i_x: input T,
i_y: input T,
o_z: output T,
) {
inst u_leaf: Leaf::<T> (
i_a: i_x,
i_b: i_y,
o_sum: o_z,
);
}
module Top (
i_a: input  logic<8>,
i_b: input  logic<8>,
o_c: output logic<8>,
) {
inst u_mid: Mid::<u8> (
i_x: i_a,
i_y: i_b,
o_z: o_c,
);
}
"#; }
        @build Simulator::builder(code, "Top");
    let i_a = sim.signal("i_a");
    let i_b = sim.signal("i_b");
    let o_c = sim.signal("o_c");

    sim.modify(|io| {
        io.set(i_a, 10u8);
        io.set(i_b, 20u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(o_c), 30);

    sim.modify(|io| {
        io.set(i_a, 200u8);
        io.set(i_b, 100u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(o_c), 44, "u8 wrap: 200+100=300→44");

    }
}
