use celox::Simulator;

#[test]
fn test_struct_constructor_comb_assignment() {
    let code = r#"
        module Top (
            a: input logic<4>,
            b: input logic<4>,
            o: output logic<8>
        ) {
            struct S {
                x: logic<4>,
                y: logic<4>,
            }

            always_comb {
                o = S'{x: a, y: b};
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0xAu8);
        io.set(b, 0x5u8);
    })
    .unwrap();

    assert_eq!(sim.get(o), 0xA5u8.into());
}

#[test]
fn test_struct_constructor_member_width_adjustment() {
    let code = r#"
        module Top (
            narrow: input logic<4>,
            wide  : input logic<8>,
            o_pad : output logic<8>,
            o_cut : output logic<4>
        ) {
            struct Padded {
                x: logic<8>,
            }

            struct Cut {
                y: logic<4>,
            }

            always_comb {
                o_pad = Padded'{x: narrow};
                o_cut = Cut'{y: wide};
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let narrow = sim.signal("narrow");
    let wide = sim.signal("wide");
    let o_pad = sim.signal("o_pad");
    let o_cut = sim.signal("o_cut");

    sim.modify(|io| {
        io.set(narrow, 0xBu8);
        io.set(wide, 0xABu8);
    })
    .unwrap();

    // narrow(4bit) -> member logic<8> should be zero-extended.
    assert_eq!(sim.get(o_pad), 0x0Bu8.into());
    // wide(8bit) -> member logic<4> should keep lower 4 bits.
    assert_eq!(sim.get(o_cut), 0xBu8.into());
}



