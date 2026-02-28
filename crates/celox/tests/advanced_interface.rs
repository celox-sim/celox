use celox::Simulator;

/// Interface with multiple modport signals and bidirectional data flow.
#[test]
fn test_interface_bidirectional() {
    let code = r#"
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
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
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

/// Multiple interface instances used in parallel.
#[test]
fn test_multiple_interface_instances() {
    let code = r#"
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
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
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

/// Interface with wide (multi-bit) signals.
#[test]
fn test_interface_wide_signal() {
    let code = r#"
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
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
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



