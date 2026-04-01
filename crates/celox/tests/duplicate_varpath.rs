// Regression test for duplicate VarPath crash.
//
// When multiple scoped variables share the same VarPath (e.g., `var avail: logic;`
// declared in different `for` loop scopes within the same `always_comb`), the old
// `module_variables` implementation used VarPath as the HashMap key, silently
// overwriting entries and losing VarIds. This caused a "no entry found for key"
// panic in the Cranelift translator's memory layout lookup.
//
// The fix uses VarId as the primary key and maintains a separate path index that
// detects ambiguous paths.
use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

// Minimal reproduction: two `for` loops in `always_comb` each declare `var tmp: logic`.
// The Veryl analyzer assigns different VarIds but identical VarPaths to these scoped
// variables. Without the fix this panics during JIT compilation.
fn test_duplicate_scoped_var_in_always_comb(sim) {
    @setup { let code = r#"
        module Top (
            sel : input  logic   ,
            a   : input  logic<4>,
            b   : input  logic<4>,
            o   : output logic<4>,
        ) {
            var result: logic<4>;
            always_comb {
                result = 0 as 4;
                for i: u32 in 0..2 {
                    var tmp: logic<4>;
                    tmp = a + i as 4;
                    if sel && i == 1 {
                        result = tmp;
                    }
                }
                for j: u32 in 0..2 {
                    var tmp: logic<4>;
                    tmp = b + j as 4;
                    if !sel && j == 1 {
                        result = tmp;
                    }
                }
            }
            assign o = result;
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    // sel=1: result = a + 1
    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set(a, 5u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 6u64.into()); // 5 + 1

    // sel=0: result = b + 1
    sim.modify(|io| {
        io.set(sel, 0u8);
        io.set(a, 0u8);
        io.set(b, 10u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 11u64.into()); // 10 + 1
}

// Generate-for with same-named scoped variables in always_comb.
// Mirrors the original AdcGroup MRE: generate-for creates instances with
// internal vars, and always_comb inside uses `var flag: logic;` in multiple
// for-loop scopes.
fn test_duplicate_scoped_var_with_generate_for(sim) {
    @setup { let code = r#"
        module Top (
            clk   : input  clock  ,
            rst   : input  reset  ,
            i_data: input  logic<8>,
            o_a   : output logic<8>,
            o_b   : output logic<8>,
        ) {
            for g in 0..2 :g_ch {
                var mem: logic<8>;

                always_ff (clk, rst) {
                    if_reset { mem = 0; }
                    else     { mem = i_data + g as 8; }
                }

                always_comb {
                    for k: u32 in 0..2 {
                        var flag: logic;
                        flag = k == g;
                        if flag {
                            if g == 0 { o_a = mem; }
                            if g == 1 { o_b = mem; }
                        }
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_data = sim.signal("i_data");
    let o_a = sim.signal("o_a");
    let o_b = sim.signal("o_b");

    // Reset (AsyncLow: active when rst=0)
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();

    // Deactivate reset, set input
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_data, 10u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // g_ch[0].mem = 10 + 0 = 10, g_ch[1].mem = 10 + 1 = 11
    assert_eq!(sim.get(o_a), 10u64.into());
    assert_eq!(sim.get(o_b), 11u64.into());
}

}
