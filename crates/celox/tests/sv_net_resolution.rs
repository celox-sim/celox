//! Several continuous assignments to a net are its drivers, and the net
//! type resolves their values bit by bit (IEEE 1800-2023 6.5, 6.6): Table
//! 6-2 for `wire` and `tri`, Table 6-3 for `wand` and `triand`, Table 6-4
//! for `wor` and `trior`. A two-state build stores the X of the resolved
//! value as 0, and resolves only drivers without an X or Z literal.
use celox::Simulation;
use num_bigint::BigUint;
use std::path::Path;

/// `kind`, `two-state value`, `four-state (value, mask)` of a 4-bit net
/// driven by `a = 4'b0011` and `b = 4'b0101`.
const KINDS: [(&str, u8, (u8, u8)); 6] = [
    ("wire", 0b0001, (0b0111, 0b0110)),
    ("tri", 0b0001, (0b0111, 0b0110)),
    ("wand", 0b0001, (0b0001, 0)),
    ("triand", 0b0001, (0b0001, 0)),
    ("wor", 0b0111, (0b0111, 0)),
    ("trior", 0b0111, (0b0111, 0)),
];

macro_rules! resolution_backend {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            for (kind, two_state, four_state) in KINDS {
                let source = format!(
                    "module Top(input logic [3:0] a, b, output {kind} [3:0] y);
                       assign y = a;
                       assign y = b;
                     endmodule"
                );
                for state in [false, true] {
                    let mut sim = Simulation::from_sv_sources(
                        vec![(source.as_str(), Path::new("net_resolution.sv"))],
                        "Top",
                    )
                    .four_state(state)
                    .$backend()
                    .unwrap();
                    let (a, b, y) = (sim.signal("a"), sim.signal("b"), sim.signal("y"));
                    sim.modify(|io| {
                        io.set(a, 0b0011u8);
                        io.set(b, 0b0101u8);
                    })
                    .unwrap();
                    let expected = if state { four_state } else { (two_state, 0) };
                    assert_eq!(
                        sim.get_four_state(y),
                        (BigUint::from(expected.0), BigUint::from(expected.1)),
                        "{kind}, four-state {state}"
                    );
                }
            }
        }
    };
}
resolution_backend!(resolves_two_drivers_interpreter, build_interpreter);
resolution_backend!(resolves_two_drivers_cranelift, build);
#[cfg(target_arch = "x86_64")]
resolution_backend!(resolves_two_drivers_native, build_native);
resolution_backend!(resolves_two_drivers_wasm, build_wasm);

/// Runs `source`, whose `initial` block prints lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("net_drivers.sv"))], "Top")
        .four_state(four_state)
        .build()
        .unwrap();
    sim.run_until(100).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect()
}

/// Part-selects that overlap on some bits only: each range of bits has its
/// own set of drivers, and a bit with one driver takes its value.
#[test]
fn overlapping_part_selects_resolve_their_common_bits() {
    let source = r#"`timescale 1ns/1ns
      module Top;
        reg [39:0] vld;
        wire [143:0] pad;
        assign pad[39:0] = vld[39:0];
        assign pad[143:32] = 'h0;
        initial begin
          vld = 40'h00_1234_5678; #1 $display("%h", pad);
          vld = 40'hff_0000_0001; #1 $display("%h", pad);
        end
      endmodule"#;
    assert_eq!(
        display(source, false),
        [
            "000000000000000000000000000012345678",
            "000000000000000000000000000000000001"
        ]
    );
    assert_eq!(
        display(source, true),
        [
            "000000000000000000000000000012345678",
            "00000000000000000000000000xx00000001"
        ]
    );
}

/// A driver of high impedance yields to the other drivers.
#[test]
fn tri_state_drivers_share_a_net() {
    let source = r#"`timescale 1ns/1ns
      module Top;
        reg a, b, oa, ob;
        tri t;
        assign t = oa ? a : 1'bz;
        assign t = ob ? b : 1'bz;
        wire [2:0] p;
        assign p[1:0] = {oa ? a : 1'bz, 1'b1};
        assign p[2:1] = {2{ob ? b : 1'bz}};
        initial begin
          a = 1; b = 0;
          oa = 0; ob = 0; #1 $display("t=%b p=%b", t, p);
          oa = 1; ob = 0; #1 $display("t=%b p=%b", t, p);
          oa = 0; ob = 1; #1 $display("t=%b p=%b", t, p);
          oa = 1; ob = 1; #1 $display("t=%b p=%b", t, p);
        end
      endmodule"#;
    assert_eq!(
        display(source, true),
        ["t=z p=zz1", "t=1 p=z11", "t=0 p=001", "t=x p=0x1"]
    );
    // The values of a two-state build have no Z that could yield, so there
    // the bus keeps the single-driver rule.
    let error = Simulation::from_sv_sources(vec![(source, Path::new("tri_state.sv"))], "Top")
        .build()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(error.contains("multiple net drivers"), "{error}");
}

/// Three drivers, one of them a constant.
#[test]
fn more_than_two_drivers_resolve() {
    let design = |z: &str, x: &str| {
        format!(
            r#"`timescale 1ns/1ns
      module Top;
        reg a, b;
        wire w;  assign w = a;  assign w = b;  assign w = 1'b1;
        wand wa; assign wa = a; assign wa = b; assign wa = {z};
        wor wo;  assign wo = a; assign wo = b; assign wo = {x};
        initial begin
          a = 1; b = 1; #1 $display("%b%b%b", w, wa, wo);
          a = 1; b = 0; #1 $display("%b%b%b", w, wa, wo);
          a = 0; b = 0; #1 $display("%b%b%b", w, wa, wo);
        end
      endmodule"#
        )
    };
    assert_eq!(
        display(&design("1'bz", "1'bx"), true),
        ["111", "x01", "x0x"]
    );
    assert_eq!(
        display(&design("1'b1", "1'b0"), true),
        ["111", "x01", "x00"]
    );
    assert_eq!(
        display(&design("1'b1", "1'b0"), false),
        ["111", "001", "000"]
    );
}

/// The drivers of a net with a delay resolve as its net type does, before
/// the delay.
#[test]
fn delayed_nets_resolve_as_their_net_type() {
    let source = r#"`timescale 1ns/1ns
      module Top;
        reg a, b;
        wand #1 wa; assign wa = a; assign wa = b;
        wor #1 wo;  assign wo = a; assign wo = b;
        wire #1 w;  assign w = a;  assign w = b;
        initial begin
          a = 1; b = 0; #3 $display("%b%b%b", wa, wo, w);
          a = 1; b = 1; #3 $display("%b%b%b", wa, wo, w);
        end
      endmodule"#;
    assert_eq!(display(source, true), ["01x", "111"]);
    assert_eq!(display(source, false), ["010", "111"]);
}

/// What the resolution does not cover keeps the single-driver rule.
#[test]
fn other_drivers_stay_rejected() {
    for (source, diagnostic) in [
        // An unresolved net has one driver (6.6.2).
        (
            "module Top(input logic a, b, output uwire y); assign y = a; assign y = b; endmodule",
            "multiple net drivers",
        ),
        // The implicit driver of a pull or supply net is not modelled as one.
        (
            "module Top(input logic a, b, output logic y); tri1 t; assign t = a; assign t = b; assign y = t; endmodule",
            "multiple net drivers",
        ),
        // A drive strength decides between conflicting drivers (28.12).
        (
            "module Top(input logic a, b, output wire y); assign (pull1, pull0) y = a; assign y = b; endmodule",
            "multiple net drivers",
        ),
        // Also when the assignment with the strength repeats another driver.
        (
            "module Top(input logic a, b, output wire y); assign y = a; assign (supply1, supply0) y = a; assign y = b; endmodule",
            "multiple net drivers",
        ),
        // One assignment that drives a bit twice.
        (
            "module Top(output wire [7:0] q); assign {q[7:0], q[3:0]} = 12'habc; endmodule",
            "multiple net drivers",
        ),
        // A variable has one continuous driver (6.5).
        (
            "module Top(input logic a, b, output logic y); assign y = a; assign y = b; endmodule",
            "multiple variable drivers",
        ),
        // A driver below an instance and one beside it.
        (
            "module Source(output logic y); assign y = 1'b1; endmodule
             module Top(input logic a, output wire y); Source source(.y(y)); assign y = a; endmodule",
            "multiple net drivers",
        ),
    ] {
        let error = Simulation::from_sv_sources(vec![(source, Path::new("drivers.sv"))], "Top")
            .build()
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains(diagnostic), "{source}: {error}");
    }
}
