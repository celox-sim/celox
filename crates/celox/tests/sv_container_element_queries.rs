//! A data query of an element of a dynamic array, a queue or an associative
//! array is one of the element type of the container (IEEE 1800-2023
//! 20.6.2, 20.7): the index is not evaluated, and the element need not
//! exist.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("queries.sv"))], "Top")
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
fn queries_of_container_elements_have_the_element_type() {
    let source = r#"
    `timescale 1ns/1ns
    module Top;
      typedef struct packed { logic [3:0] a; logic [8:0] b; } pair_t;
      bit   [11:0]      assoc [bit [31:0]];
      int               queue [$];
      logic [7:0]       dynamic [];
      logic [2:1][3:0]  wide [string];
      pair_t            pairs [$];
      int               nested [$][$];
      logic [99:0]      big [$];
      bit [$bits(assoc[0])-1:0] sized;
      initial begin
        $display("bits: %0d %0d %0d %0d %0d %0d", $bits(assoc[32'h10]), $bits(queue[0]),
                 $bits(dynamic[5]), $bits(wide["k"]), $bits(pairs[0]), $bits(nested[0][0]));
        $display("last: %0d sized: %0d", $bits(queue[$]), $bits(sized));
        $display("shape: %0d %0d %0d %0d", $left(wide["k"]), $right(wide["k"]),
                 $size(wide["k"]), $dimensions(wide["k"]));
        $display("select: %0d %0d", $bits(wide["k"][1]), $bits(assoc[1][3:0]));
        $display("big: %0d %0d", $bits(big[0]), $bits(big[0][99]));
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            [
                "bits: 12 32 8 8 13 32",
                "last: 32 sized: 12",
                "shape: 2 1 2 2",
                "select: 4 4",
                "big: 100 1",
            ],
            "four-state {four_state}"
        );
    }
}

/// The size of a whole container depends on its elements; that is not a
/// constant, and is not supported as a value either.
#[test]
fn bits_of_a_whole_container_is_no_constant() {
    let error = Simulation::from_sv_sources(
        vec![(
            "module Top; int queue [$]; initial $display(\"%0d\", $bits(queue)); endmodule",
            Path::new("queries.sv"),
        )],
        "Top",
    )
    .build()
    .expect_err("the query is rejected")
    .to_string();
    assert!(error.contains("$bits"), "{error}");
}
