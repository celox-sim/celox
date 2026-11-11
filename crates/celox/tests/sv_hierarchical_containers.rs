//! A hierarchical name (IEEE 1800-2023 23.6) of an associative array, a
//! dynamic array or a queue of another instance denotes that container: its
//! elements are read and written and its methods called through the name.
//! `$readmemh` and `$readmemb` load an associative array with integral
//! indices by creating the element of each address of the file (21.4.1).
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("containers.sv"))], "Top")
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

/// The bounds of a container's element type may call a constant function
/// of the declaring module; a hierarchical name of the container has them.
#[test]
fn hierarchical_names_keep_bounds_from_constant_functions() {
    let source = r#"
    `timescale 1ns/1ns
    module Memory #(parameter int N = 3);
      function automatic int width(int n); return 4 * n; endfunction
      bit [width(N)-1:0] mem [int];
    endmodule
    module Top;
      Memory #(.N(2)) a();
      initial begin
        a.mem[5] = '1;
        $display("%h %h", a.mem[5], a.mem[5] + 9'd1);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["ff 100"],
            "four-state {four_state}"
        );
    }
}

#[test]
fn hierarchical_names_denote_the_containers_of_instances() {
    let source = r#"
    `timescale 1ns/1ns
    module Memory #(parameter int W = 8);
      bit [W-1:0] mem [bit [31:0]];
      int queue [$];
      logic [7:0] dynamic [];
      initial begin
        #2;
        $display("own %0d: %h %h size=%0d", W, mem[32'h10], mem[32'h8000_0000], queue.size());
      end
    endmodule
    module Top;
      Memory a();
      Memory #(.W(12)) b();
      task automatic fill(input bit [31:0] base);
        for (int i = 0; i < 4; i++) a.mem[base + i] = 8'(i + 1);
      endtask
      initial begin
        a.mem[32'h10] = 8'haa;
        a.mem[32'h8000_0000] = 8'h55;
        b.mem[32'h10] = 12'habc;
        {a.mem[32'h21], a.mem[32'h20]} = 16'h1234;
        fill(32'h100);
        a.queue.push_back(7);
        a.queue.push_back(9);
        a.dynamic = new[3];
        a.dynamic[1] = 8'h3c;
        #1;
        $display("a: %h %h %h%h count=%0d exists=%0d%0d", a.mem[32'h10], a.mem[32'h8000_0000],
                 a.mem[32'h21], a.mem[32'h20], a.mem.num(), a.mem.exists(32'h103), a.mem.exists(32'h104));
        $display("b: %h %h", b.mem[32'h10], b.mem[32'h10] + 12'h001);
        $display("queue: %0d %0d dynamic: %0d %h", a.queue.size(), a.queue[1], a.dynamic.size(),
                 a.dynamic[1]);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        // The instances print in either order.
        let mut lines = display(source, four_state);
        lines.sort();
        assert_eq!(
            lines,
            [
                "a: aa 55 1234 count=8 exists=10",
                "b: abc abd",
                "own 12: abc 000 size=0",
                "own 8: aa 55 size=2",
                "queue: 2 9 dynamic: 3 3c",
            ],
            "four-state {four_state}"
        );
    }
}
