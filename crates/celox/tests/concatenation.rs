use celox::Simulator;

#[test]
fn test_lhs_concatenation_execution() {
    let code = r#"
        module Top (val_in: input logic<16>) {
            var a: logic<8>;
            var b: logic<8>;
            assign {a, b} = val_in;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let val_in = sim.signal("val_in");
    let a = sim.signal("a");
    let b = sim.signal("b");

    sim.modify(|io| io.set(val_in, 0x1234u16)).unwrap();

    assert_eq!(sim.get(a), 0x12u8.into());
    assert_eq!(sim.get(b), 0x34u8.into());
}

#[test]
fn test_rhs_concatenation_execution() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            o: output logic<16>
        ) {
            always_comb {
                o = {a, b};
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0x12u8);
        io.set(b, 0x34u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0x1234u32.into());
}

#[test]
fn test_rhs_mixed_concatenation_execution() {
    let code = r#"
        module Top (
            a: input logic<4>,
            b: input logic<4>,
            o: output logic<12>
        ) {
            always_comb {
                o = {a, 4'hF, b};
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0xAu8);
        io.set(b, 0xCu8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0xAFCu32.into());
}

#[test]
fn test_rhs_concatenation_dependency() {
    let code = r#"
        module Top (a: input logic<8>, b: input logic<8>) {
            var tmp: logic<16>;
            var out: logic<16>;
            always_comb {
                tmp = {a, b};
                out = tmp;
            }
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "RHS concatenation must register all parts as sources"
    );
}

#[test]
fn test_replication_concatenation_execution() {
    let code = r#"
        module Top (
            a: input logic<2>,
            o: output logic<8>
        ) {
            assign o = {a repeat 4};
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 2u8)).unwrap();
    assert_eq!(sim.get(o), 0xAAu8.into());
}



