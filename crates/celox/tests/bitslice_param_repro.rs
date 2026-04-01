use celox::{Simulator, SimulatorBuilder};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

/// Reproduction for parametric bit-slice bug:
/// `w_buf[N_CH - 1:0]` with N_CH=4 resolves to [1:0] instead of [3:0].

const AXI_LITE_REG_FILE: &str = include_str!("fixtures/bitslice/axi_lite_reg_file.veryl");

const ADDR_REQ_LAST: u32 = 0x08;

fn create_dut() -> Simulator {
    let mut sim = Simulator::builder(AXI_LITE_REG_FILE, "AxiLiteRegFile")
        .param("ADDR_W", 16u64)
        .param("N_CH", 4u64)
        .param("OUT_DEPTH", 16u64)
        .param("AXL_ADDR_W", 12u64)
        .param("AXL_DATA_W", 32u64)
        .build()
        .unwrap();

    let clk = sim.event("clk");
    let rst = sim.signal("rst");

    // Reset sequence
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Initialize AXI-Lite signals
    let s_awvalid = sim.signal("s_awvalid");
    let s_wvalid = sim.signal("s_wvalid");
    let s_bready = sim.signal("s_bready");
    let s_arvalid = sim.signal("s_arvalid");
    let s_rready = sim.signal("s_rready");
    sim.modify(|io| {
        io.set(s_awvalid, 0u8);
        io.set(s_wvalid, 0u8);
        io.set(s_bready, 0u8);
        io.set(s_arvalid, 0u8);
        io.set(s_rready, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    sim
}

fn axl_write(sim: &mut Simulator, addr: u32, data: u32) {
    let clk = sim.event("clk");
    let s_awvalid = sim.signal("s_awvalid");
    let s_awaddr = sim.signal("s_awaddr");
    let s_wvalid = sim.signal("s_wvalid");
    let s_wdata = sim.signal("s_wdata");
    let s_wstrb = sim.signal("s_wstrb");
    let s_bready = sim.signal("s_bready");
    let s_bvalid = sim.signal("s_bvalid");

    // AW + W handshake (s_awaddr is 12-bit, use u16)
    sim.modify(|io| {
        io.set(s_awvalid, 1u8);
        io.set(s_awaddr, addr as u16);
        io.set(s_wvalid, 1u8);
        io.set(s_wdata, data);
        io.set(s_wstrb, 0xfu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(s_awvalid, 0u8);
        io.set(s_wvalid, 0u8);
    })
    .unwrap();

    // Wait for B
    sim.modify(|io| io.set(s_bready, 1u8)).unwrap();
    for _ in 0..10 {
        let bv: u64 = sim.get(s_bvalid).try_into().unwrap();
        if bv != 0 {
            break;
        }
        sim.tick(clk).unwrap();
    }
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(s_bready, 0u8)).unwrap();
}

fn axl_read(sim: &mut Simulator, addr: u32) -> u32 {
    let clk = sim.event("clk");
    let s_arvalid = sim.signal("s_arvalid");
    let s_araddr = sim.signal("s_araddr");
    let s_rready = sim.signal("s_rready");
    let s_rvalid = sim.signal("s_rvalid");
    let s_rdata = sim.signal("s_rdata");

    // AR handshake (s_araddr is 12-bit, use u16)
    sim.modify(|io| {
        io.set(s_arvalid, 1u8);
        io.set(s_araddr, addr as u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(s_arvalid, 0u8)).unwrap();

    // Wait for R
    sim.modify(|io| io.set(s_rready, 1u8)).unwrap();
    for _ in 0..10 {
        let rv: u64 = sim.get(s_rvalid).try_into().unwrap();
        if rv != 0 {
            break;
        }
        sim.tick(clk).unwrap();
    }
    let val: u64 = sim.get(s_rdata).try_into().unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(s_rready, 0u8)).unwrap();
    val as u32
}

all_backends! {

    fn fill_one_literal_multi_branch(sim) {
        @setup { // Verify '1 fill-literal: unwritten bits should be 1, not 0.
let code = r#"
module Top (
sel: input logic<2>,
a: input logic,
b: input logic,
data: input logic<4>,
out: output logic<32>,
) {
always_comb {
out = '1;
if sel == 2'b00 {
out[0] = a;
out[1] = b;
} else if sel == 2'b01 {
out[0] = b;
out[1] = a;
} else if sel == 2'b10 {
out[3:0] = data;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let data = sim.signal("data");
    let out = sim.signal("out");

    // sel=0: out[0]=a=0, out[1]=b=0, upper bits should be all-1
    sim.modify(|io| {
        io.set(sel, 0u8);
        io.set(a, 0u8);
        io.set(b, 0u8);
        io.set(data, 0u8);
    })
    .unwrap();
    let val: u64 = sim.get(out).try_into().unwrap();
    eprintln!("  sel=00, a=0, b=0 → out=0x{:08x}", val);
    assert_eq!(
        val, 0xFFFF_FFFC,
        "sel=00: upper bits should be 1, got 0x{:08x}",
        val
    );

    // sel=2: out[3:0]=data=0b0101, upper bits should be all-1
    sim.modify(|io| {
        io.set(sel, 2u8);
        io.set(a, 0u8);
        io.set(b, 0u8);
        io.set(data, 0b0101u8);
    })
    .unwrap();
    let val: u64 = sim.get(out).try_into().unwrap();
    eprintln!("  sel=10, data=0b0101 → out=0x{:08x}", val);
    assert_eq!(val, 0xFFFF_FFF5, "sel=10: got 0x{:08x}", val);

    }
}

#[test]
fn axi_lite_reg_file_req_last_vec() {

    let mut sim = create_dut();
    let reg_req_last_vec = sim.signal("reg_req_last_vec");

    let patterns: &[u32] = &[0b0001, 0b0010, 0b0100, 0b1000, 0b1111, 0b1010, 0b0101];
    for &p in patterns {
        axl_write(&mut sim, ADDR_REQ_LAST, p);
        // Check the register directly (bypasses the comb read path)
        let reg_val: u64 = sim.get(reg_req_last_vec).try_into().unwrap();
        let got = axl_read(&mut sim, ADDR_REQ_LAST);
        eprintln!(
            "  wrote: 0b{:04b}, reg_req_last_vec: 0b{:04b}, read_mux: 0b{:04b}",
            p,
            reg_val & 0xf,
            got & 0xf,
        );
        assert_eq!(
            reg_val & 0xf,
            p as u64,
            "reg_req_last_vec: Pattern 0b{:04b}: got 0b{:04b}",
            p,
            reg_val & 0xf,
        );
        assert_eq!(
            got & 0xf,
            p,
            "read_mux: Pattern 0b{:04b}: got 0b{:04b}",
            p,
            got & 0xf,
        );
    }

}

#[test]
fn parametric_bitslice_multi_branch() {

    // Simplified version of AxiLiteRegFile's read path
    let code = r#"
        module Top #(
            param N: u32 = 4,
        ) (
            sel: input logic<2>,
            a: input logic,
            b: input logic,
            data: input logic<N>,
            out: output logic<32>,
        ) {
            always_comb {
                out = '0;
                if sel == 2'b00 {
                    out[0] = a;
                    out[1] = b;
                } else if sel == 2'b01 {
                    out[0] = b;
                    out[1] = a;
                } else if sel == 2'b10 {
                    out[N - 1:0] = data;
                }
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top")
        .param("N", 4u64)
        .build()
        .unwrap();
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let data = sim.signal("data");
    let out = sim.signal("out");

    // Also check sel=2'b00 and sel=2'b01
    sim.modify(|io| {
        io.set(sel, 0u8);
        io.set(a, 1u8);
        io.set(b, 0u8);
        io.set(data, 0u8);
    })
    .unwrap();
    let val: u64 = sim.get(out).try_into().unwrap();
    eprintln!("  sel=00, a=1, b=0 → out=0b{:08b}", val & 0xff);

    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    let val: u64 = sim.get(out).try_into().unwrap();
    eprintln!("  sel=01, a=1, b=0 → out=0b{:08b}", val & 0xff);

    // Test sel=2'b10: out[3:0] should = data
    for pattern in [0b0001u8, 0b0010, 0b0100, 0b1000, 0b1111, 0b1010] {
        sim.modify(|io| {
            io.set(sel, 2u8); // 2'b10
            io.set(a, 0u8);
            io.set(b, 0u8);
            io.set(data, pattern);
        })
        .unwrap();
        let val: u64 = sim.get(out).try_into().unwrap();
        eprintln!("  sel=10, data=0b{:04b}, out=0b{:04b}", pattern, val & 0xf);
        assert_eq!(
            val & 0xf,
            pattern as u64,
            "data=0b{:04b}: got 0b{:04b}",
            pattern,
            val & 0xf,
        );
    }

}

#[test]
fn hardcoded_bitslice_multi_branch() {

    // Three branches: two write individual bits, third writes a range
    let code = r#"
        module Top (
            sel: input logic<2>,
            a: input logic,
            b: input logic,
            data: input logic<4>,
            out: output logic<32>,
        ) {
            always_comb {
                out = '0;
                if sel == 2'b00 {
                    out[0] = a;
                    out[1] = b;
                } else if sel == 2'b01 {
                    out[0] = b;
                    out[1] = a;
                } else if sel == 2'b10 {
                    out[3:0] = data;
                }
            }
        }
    "#;
    // Test with optimizer disabled
    let mut sim = Simulator::builder(code, "Top")
        .optimize(false)
        .build()
        .unwrap();
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let data = sim.signal("data");
    let out = sim.signal("out");

    // Test sel=2 (else if 2'b10 branch): out[3:0] = data
    for pattern in [0b0001u8, 0b0010, 0b0100, 0b1000, 0b1111, 0b1010] {
        sim.modify(|io| {
            io.set(sel, 2u8);
            io.set(a, 0u8);
            io.set(b, 0u8);
            io.set(data, pattern);
        })
        .unwrap();
        let val: u64 = sim.get(out).try_into().unwrap();
        eprintln!(
            "  [hardcoded] sel=0, data=0b{:04b}, out=0b{:04b}",
            pattern,
            val & 0xf
        );
        assert_eq!(
            val & 0xf,
            pattern as u64,
            "[hardcoded] data=0b{:04b}: got 0b{:04b}",
            pattern,
            val & 0xf,
        );
    }

}

#[test]
fn hardcoded_bitslice_trace() {

    let code = r#"
        module Top (
            sel: input logic<2>,
            a: input logic,
            b: input logic,
            data: input logic<4>,
            out: output logic<32>,
        ) {
            always_comb {
                out = '0;
                if sel == 2'b00 {
                    out[0] = a;
                    out[1] = b;
                } else if sel == 2'b01 {
                    out[0] = b;
                    out[1] = a;
                } else if sel == 2'b10 {
                    out[3:0] = data;
                }
            }
        }
    "#;
    let trace = SimulatorBuilder::new(code, "Top")
        .optimize(false)
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();
    let output = trace.trace.format_program().unwrap();
    eprintln!("{}", output);

}
