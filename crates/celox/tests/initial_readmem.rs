use celox::{BigUint, LoweringPhase, ParserError, Simulator, SimulatorErrorKind};
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

fn temp_mem_file(name: &str, content: &str) -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("celox_{name}_{}_{id}.mem", std::process::id()));
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

fn test_initial_readmemh_supports_const_if(sim) {
    @omit_veryl;
    @setup {
        let hex_path = temp_mem_file("readmemh_if", "21\n43\n65\n87\n");
        let other_path = temp_mem_file("readmemh_if_dead", "00\n00\n00\n00\n");
        let code = format!(r#"
            module Top (out0: output logic<8>, out3: output logic<8>) {{
                var mem: logic<8>[4];
                initial {{
                    if 1'd1 {{
                        $readmemh("{}", mem);
                    }} else {{
                        $readmemh("{}", mem);
                    }}
                }}
                assign out0 = mem[0];
                assign out3 = mem[3];
            }}
        "#, hex_path, other_path);
    }
    @build Simulator::builder(&code, "Top");
    assert_eq!(sim.get(sim.signal("out0")), BigUint::from(0x21u32));
    assert_eq!(sim.get(sim.signal("out3")), BigUint::from(0x87u32));
}

fn test_initial_readmemh_supports_const_for(sim) {
    @omit_veryl;
    @setup {
        let mem_path = temp_mem_file("readmemh_for", "11\n22\n33\n44\n");
        let code = format!(r#"
            module Top (out0: output logic<8>, out2: output logic<8>) {{
                var mem: logic<8>[4];
                initial {{
                    for i in 0..2 {{
                        if i == 1 {{
                            $readmemh("{}", mem);
                        }}
                    }}
                }}
                assign out0 = mem[0];
                assign out2 = mem[2];
            }}
        "#, mem_path);
    }
    @build Simulator::builder(&code, "Top");
    assert_eq!(sim.get(sim.signal("out0")), BigUint::from(0x11u32));
    assert_eq!(sim.get(sim.signal("out2")), BigUint::from(0x33u32));
}

fn test_initial_readmemh_supports_indexed_destination(sim) {
    @omit_veryl;
    @setup {
        let mem_path = temp_mem_file("readmemh_indexed", "aa\nbb\n");
        let code = format!(r#"
            module Top (
                out0: output logic<8>,
                out1: output logic<8>,
                out2: output logic<8>,
                out3: output logic<8>,
            ) {{
                var mem: logic<8>[4];
                initial {{
                    $readmemh("{}", mem[1]);
                }}
                assign out0 = mem[0];
                assign out1 = mem[1];
                assign out2 = mem[2];
                assign out3 = mem[3];
            }}
        "#, mem_path);
    }
    @build Simulator::builder(&code, "Top").four_state(true);
    assert_eq!(sim.get_four_state(sim.signal("out0")).1, BigUint::from(0xffu32));
    assert_eq!(sim.get_four_state(sim.signal("out1")), (BigUint::from(0xaau32), BigUint::from(0u32)));
    assert_eq!(sim.get_four_state(sim.signal("out2")), (BigUint::from(0xbbu32), BigUint::from(0u32)));
    assert_eq!(sim.get_four_state(sim.signal("out3")).1, BigUint::from(0xffu32));
}

fn test_initial_readmemh_multiple_files_merge_in_order(sim) {
    @omit_veryl;
    @setup {
        let first_path = temp_mem_file("readmemh_multi_first", "11\n22\n33\n44\n");
        let second_path = temp_mem_file("readmemh_multi_second", "aa\nbb\n");
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
                    $readmemh("{}", mem[1]);
                }}
                assign out0 = mem[0];
                assign out1 = mem[1];
                assign out2 = mem[2];
                assign out3 = mem[3];
            }}
        "#, first_path, second_path);
    }
    @build Simulator::builder(&code, "Top");
    assert_eq!(sim.get(sim.signal("out0")), BigUint::from(0x11u32));
    assert_eq!(sim.get(sim.signal("out1")), BigUint::from(0xaau32));
    assert_eq!(sim.get(sim.signal("out2")), BigUint::from(0xbbu32));
    assert_eq!(sim.get(sim.signal("out3")), BigUint::from(0x44u32));
}

}

#[test]
fn test_initial_readmemb_reports_unsupported() {
    let mem_path = temp_mem_file("readmemb", "00010010\n00110100\n01010110\n01111000\n");
    let code = format!(
        r#"
            module Top (out0: output logic<8>) {{
                var mem: logic<8>[4];
                initial {{
                    $readmemb("{}", mem);
                }}
                assign out0 = mem[0];
            }}
        "#,
        mem_path
    );

    let err = Simulator::builder(&code, "Top")
        .build()
        .expect_err("$readmemb should not be silently ignored");
    match err.kind() {
        SimulatorErrorKind::SIRParser(ParserError::Unsupported {
            issue,
            phase,
            feature,
            detail,
            ..
        }) => {
            assert_eq!(*issue, 111);
            assert_eq!(*phase, LoweringPhase::SimulatorParser);
            assert_eq!(*feature, "initial statement");
            assert!(detail.contains("only direct $readmemh"));
        }
        other => panic!("expected unsupported initial statement error, got {other:?}"),
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn test_initial_readmemh_applies_to_shared_native_simulator() {
    let mem_path = temp_mem_file("readmemh_shared_native", "ca\nfe\n");
    let code = format!(
        r#"
            module Top (out0: output logic<8>, out1: output logic<8>) {{
                var mem: logic<8>[2];
                initial {{
                    $readmemh("{}", mem);
                }}
                assign out0 = mem[0];
                assign out1 = mem[1];
            }}
        "#,
        mem_path
    );

    let sim = Simulator::builder(&code, "Top").build_native().unwrap();
    let shared = sim.shared_code();
    let (program, _) = celox::compile_to_sir(
        &[(&code, std::path::Path::new(""))],
        "Top",
        &[],
        &[],
        false,
        &celox::TraceOptions::default(),
        None,
        None,
        None,
        None,
        &[],
        &celox::OptimizeOptions::default(),
    )
    .unwrap();
    let mut sim = Simulator::from_shared(shared, program);

    assert_eq!(sim.get(sim.signal("out0")), BigUint::from(0xcau32));
    assert_eq!(sim.get(sim.signal("out1")), BigUint::from(0xfeu32));
}
