use celox::Simulator;

#[test]
fn test_array_literal_comb_assignment() {
    let code = r#"
        module Top (o0: output logic<8>, o1: output logic<8>) {
            var a: logic<8> [2];
            always_comb {
                a = '{8'h12, 8'h34};
            }
            assign o0 = a[0];
            assign o1 = a[1];
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");

    // Trigger combinational evaluation once.
    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(o0), 0x12u8.into());
    assert_eq!(sim.get(o1), 0x34u8.into());
}

#[test]
fn test_array_literal_default_comb_assignment() {
    let code = r#"
        module Top (
            o0: output logic<8>,
            o1: output logic<8>,
            o2: output logic<8>,
            o3: output logic<8>
        ) {
            var a: logic<8> [4];
            always_comb {
                a = '{8'h12, default: 8'hAA};
            }
            assign o0 = a[0];
            assign o1 = a[1];
            assign o2 = a[2];
            assign o3 = a[3];
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");
    let o3 = sim.signal("o3");

    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(o0), 0x12u8.into());
    assert_eq!(sim.get(o1), 0xAAu8.into());
    assert_eq!(sim.get(o2), 0xAAu8.into());
    assert_eq!(sim.get(o3), 0xAAu8.into());
}

#[test]
fn test_array_literal_nested_default_multidim_assignment() {
    let code = r#"
        module Top (
            o00: output logic<8>,
            o01: output logic<8>,
            o10: output logic<8>,
            o11: output logic<8>
        ) {
            var a: logic<8> [2, 2];
            always_comb {
                a = '{
                    '{8'h11, default: 8'h22},
                    default: '{default: 8'hAA}
                };
            }
            assign o00 = a[0][0];
            assign o01 = a[0][1];
            assign o10 = a[1][0];
            assign o11 = a[1][1];
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o00 = sim.signal("o00");
    let o01 = sim.signal("o01");
    let o10 = sim.signal("o10");
    let o11 = sim.signal("o11");

    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(o00), 0x11u8.into());
    assert_eq!(sim.get(o01), 0x22u8.into());
    assert_eq!(sim.get(o10), 0xAAu8.into());
    assert_eq!(sim.get(o11), 0xAAu8.into());
}



