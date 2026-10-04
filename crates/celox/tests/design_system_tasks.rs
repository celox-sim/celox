use celox::{BigUint, RuntimeEvent, Simulator};
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

// `$readmemh` in a clocked block loads the file each time it runs; locations
// the file does not name keep their values (IEEE 1800-2023 21.4).
fn test_always_ff_readmemh_reloads_named_locations(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let mem_path = temp_mem_file("ff_readmemh", "12\n@2\n56\n");
        let code = format!(r#"
            module Top (
                clk: input clock,
                load: input logic,
                d: input logic<8>,
                out0: output logic<8>,
                out1: output logic<8>,
                out2: output logic<8>,
            ) {{
                var mem: logic<8>[3];
                always_ff (clk) {{
                    if load {{
                        $readmemh("{}", mem);
                    }} else {{
                        mem[0] = d;
                        mem[1] = d;
                        mem[2] = d;
                    }}
                }}
                assign out0 = mem[0];
                assign out1 = mem[1];
                assign out2 = mem[2];
            }}
        "#, mem_path);
    }
    @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let load = sim.signal("load");
    let d = sim.signal("d");
    let out0 = sim.signal("out0");
    let out1 = sim.signal("out1");
    let out2 = sim.signal("out2");

    sim.modify(|io| {
        io.set(load, 0u8);
        io.set(d, 0xa5u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out1), BigUint::from(0xa5u32));

    sim.modify(|io| io.set(load, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out0), BigUint::from(0x12u32));
    assert_eq!(sim.get(out1), BigUint::from(0xa5u32));
    assert_eq!(sim.get(out2), BigUint::from(0x56u32));
}

fn test_always_comb_readmemh_drives_file_contents(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let mem_path = temp_mem_file("comb_readmemh", "12\n34\n56\n78\n");
        let code = format!(r#"
            module Top (
                i: input logic<2>,
                q: output logic<8>,
            ) {{
                var mem: logic<8>[4];
                always_comb {{
                    $readmemh("{}", mem);
                }}
                assign q = mem[i];
            }}
        "#, mem_path);
    }
    @build Simulator::builder(&code, "Top");
    let i = sim.signal("i");
    let q = sim.signal("q");
    for (index, expected) in [(0u8, 0x12u32), (1, 0x34), (2, 0x56), (3, 0x78)] {
        sim.modify(|io| io.set(i, index)).unwrap();
        assert_eq!(sim.get(q), BigUint::from(expected));
    }
}

}

fn finish_events(sim: &mut Simulator) -> usize {
    sim.drain_runtime_events()
        .into_iter()
        .filter(|event| matches!(event, RuntimeEvent::Finish))
        .count()
}

// A design-level `$finish` is reported to the host, which decides when to stop
// (IEEE 1800-2023 20.2).
#[test]
fn test_design_finish_is_reported_from_every_block_kind() {
    let code = r#"
        module Top (clk: input clock, a: input logic<8>, q_ff: output logic<8>, q_comb: output logic<8>) {
            function check (x: input logic<8>) -> logic<8> {
                if x == 8'hfe {
                    $finish();
                }
                return x;
            }
            always_ff (clk) {
                if a == 8'hff {
                    $finish();
                }
                q_ff = check(a);
            }
            always_comb {
                if a == 8'h80 {
                    $finish();
                }
                q_comb = check(a);
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(finish_events(&mut sim), 0);

    // always_ff
    sim.modify(|io| io.set(a, 0xffu8)).unwrap();
    sim.tick(clk).unwrap();
    assert!(finish_events(&mut sim) > 0);

    // always_comb
    sim.modify(|io| io.set(a, 0x80u8)).unwrap();
    assert!(finish_events(&mut sim) > 0);

    // A function called from both kinds of block.
    sim.modify(|io| io.set(a, 0xfeu8)).unwrap();
    sim.tick(clk).unwrap();
    assert!(finish_events(&mut sim) > 0);
}
