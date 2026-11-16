//! An element of a packed array selected by a variable index lies at the
//! index times the element width (IEEE 1800-2023 7.4.5). The width may be
//! declared with a constant system function such as `$clog2`, which is a
//! constant of the declaration, not a call the select makes.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim =
        Simulation::from_sv_sources(vec![(source, Path::new("element_selects.sv"))], "Top")
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

#[test]
fn selects_elements_whose_width_calls_a_constant_function() {
    let source = r#"
    `timescale 1ns/1ns
    module Top #(parameter int N = 512, parameter int THREADS = 2);
      logic [THREADS-1:0][$clog2(N)-1:0] a, comb_written, ff_written;
      logic [0:0][$clog2(N)-1:0] single;
      logic tid;
      logic clk = 0;
      logic [$clog2(N)-1:0] continuous, procedural, clocked, one;
      assign continuous = a[tid];
      assign one = single[tid];
      always_comb procedural = a[tid];
      always @(posedge clk) clocked <= a[tid];
      always_comb begin
        comb_written = '0;
        comb_written[tid] = 9'h0aa;
      end
      always @(posedge clk) ff_written[tid] <= 9'h0aa;
      initial begin
        a = '0;
        a[1] = 9'h155;
        a[0] = 9'h003;
        single = 9'h1fe;
        ff_written = '0;
        tid = 1;
        #1 clk = 1;
        #1 $display("%h %h %h %h %h", continuous, procedural, clocked, comb_written, ff_written);
        tid = 0;
        clk = 0;
        #1 clk = 1;
        #1 $display("%h %h %h %h %h one=%h", continuous, procedural, clocked, comb_written,
                    ff_written, one);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["155 155 155 15400 15400", "003 003 003 000aa 154aa one=1fe"],
            "four-state {four_state}"
        );
    }
}
