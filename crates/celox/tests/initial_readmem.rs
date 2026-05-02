use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

fn temp_mem_file(name: &str, content: &str) -> String {
    let path = std::env::temp_dir().join(format!("celox_{name}_{}.mem", std::process::id()));
    std::fs::write(&path, content).unwrap();
    path.to_string_lossy().replace('\\', "\\\\")
}

all_backends! {

fn test_initial_readmemh_loads_unpacked_array(sim) {
    @omit_veryl;
    @setup {
        let mem_path = temp_mem_file("readmemh", "12\n34\n56\n78\n");
        let code = format!(r#"
            module Top (
                out0: output logic<8>,
                out1: output logic<8>,
                out2: output logic<8>,
                out3: output logic<8>,
            ) {{
                var mem: logic<8>[4];
                initial {{
                    $readmemh("{}", mem);
                }}
                assign out0 = mem[0];
                assign out1 = mem[1];
                assign out2 = mem[2];
                assign out3 = mem[3];
            }}
        "#, mem_path);
    }
    @build Simulator::builder(&code, "Top");
    assert_eq!(sim.get(sim.signal("out0")), BigUint::from(0x12u32));
    assert_eq!(sim.get(sim.signal("out1")), BigUint::from(0x34u32));
    assert_eq!(sim.get(sim.signal("out2")), BigUint::from(0x56u32));
    assert_eq!(sim.get(sim.signal("out3")), BigUint::from(0x78u32));
}

fn test_initial_readmemh_supports_comments_address_and_xz(sim) {
    @omit_veryl;
    @setup {
        let mem_path = temp_mem_file(
            "readmemh_xz",
            "aa\n// skip to address 2\n@2\nx5\nz0\n",
        );
        let code = format!(r#"
            module Top (
                out0: output logic<8>,
                out1: output logic<8>,
                out2: output logic<8>,
                out3: output logic<8>,
            ) {{
                var mem: logic<8>[4];
                initial {{
                    $readmemh("{}", mem);
                }}
                assign out0 = mem[0];
                assign out1 = mem[1];
                assign out2 = mem[2];
                assign out3 = mem[3];
            }}
        "#, mem_path);
    }
    @build Simulator::builder(&code, "Top").four_state(true);
    assert_eq!(sim.get_four_state(sim.signal("out0")), (BigUint::from(0xaau32), BigUint::from(0u32)));
    assert_eq!(sim.get_four_state(sim.signal("out1")).1, BigUint::from(0xffu32));
    assert_eq!(sim.get_four_state(sim.signal("out2")), (BigUint::from(0x05u32), BigUint::from(0xf0u32)));
    assert_eq!(sim.get_four_state(sim.signal("out3")), (BigUint::from(0xf0u32), BigUint::from(0xf0u32)));
}

}
