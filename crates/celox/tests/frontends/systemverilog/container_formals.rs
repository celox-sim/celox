use super::*;

/// The index of a select of a dynamic element assigned a value with a call
/// is evaluated before the call and keeps its width and unknown bits: out
/// of range or unknown, it writes no element (IEEE 1800-2023 7.10.1,
/// 11.5.1).
#[test]
fn element_select_index_before_a_call_keeps_its_value() {
    let source = r#"
    module Top(output int k);
        bit [7:0] r[$];
        logic [63:0] x;
        function automatic bit [3:0] one();
            return 1;
        endfunction
        initial begin
            r = '{8'h00};
            r[64'h1_0000_0000][0] = one();
            x = 'x;
            r[x][1] = one();
            k = r[0];
        end
    endmodule"#;
    // A two-state `x` holds 0, which selects the first element.
    for (four_state, expected) in [(true, 0u8), (false, 2)] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("element-select-index.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        let k = sim.signal("k");
        sim.run_until(0).unwrap();
        assert_eq!(
            sim.get(k),
            BigUint::from(expected),
            "four_state={four_state}"
        );
    }
}

/// A select of a dynamic element at a position that changes the container
/// is evaluated before the element is read, so the write keeps the bits of
/// the element the select denotes (IEEE 1800-2023 11.4.1, 11.5.1).
#[test]
fn element_select_at_a_position_with_effects() {
    let source = "module Top(output bit [7:0] first, output int size); bit [7:0] q[$]; \
        initial begin q = '{8'h02, 8'h10}; q[0][q.pop_front()] = 1'b1; \
        first = q[0]; size = q.size(); end endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("select.sv"))], "Top")
        .build()
        .unwrap();
    let first = sim.signal("first");
    let size = sim.signal("size");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(first), BigUint::from(0x14u8));
    assert_eq!(sim.get(size), BigUint::from(1u8));
}

/// Two calls in the position of a select of a dynamic element are two
/// evaluations, each of which pops an element; the bounds of the select,
/// which repeat the position, read each once (IEEE 1800-2023 11.5.1).
#[test]
fn element_select_positions_evaluate_each_call() {
    let source = "module Top(output bit [7:0] first, output int size); bit [7:0] q[$]; \
        initial begin q = '{8'h00, 8'h01, 8'h10}; q[0][q.pop_front() + q.pop_front()] = 1'b1; \
        first = q[0]; size = q.size(); end endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("select.sv"))], "Top")
        .build()
        .unwrap();
    let first = sim.signal("first");
    let size = sim.signal("size");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(first), BigUint::from(0x12u8));
    assert_eq!(sim.get(size), BigUint::from(1u8));
}

/// An assignment evaluates its value before the address of its target,
/// which it evaluates with the values it has when it is written (IEEE
/// 1800-2023 4.9.3): a value that reads a variable an index call changes
/// reads it before the call.
#[test]
fn assignment_values_are_evaluated_before_target_addresses() {
    let source = "module Top(output bit [7:0] first, output int i); bit [7:0] q[$]; \
        function automatic int next_index(); i++; return 0; endfunction \
        initial begin q = '{8'h00, 8'h00}; i = 1; q[next_index()][3:0] = i; \
        first = q[0]; end endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("order.sv"))], "Top")
        .build()
        .unwrap();
    let first = sim.signal("first");
    let i = sim.signal("i");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(first), BigUint::from(1u8));
    assert_eq!(sim.get(i), BigUint::from(2u8));
}

/// An increment evaluates the address of its operand once, before it reads
/// it, as an assignment operator does (IEEE 1800-2023 11.4.2, 11.4.1).
#[test]
fn increments_evaluate_place_addresses_once() {
    let source = "module Top(output bit [7:0] first, output int calls); bit [7:0] q[$]; \
        function automatic int next_index(); calls++; return 0; endfunction \
        initial begin q = '{8'h0f, 8'h00}; calls = 0; q[next_index()][3:0]++; \
        first = q[0]; end endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("order.sv"))], "Top")
        .build()
        .unwrap();
    let first = sim.signal("first");
    let calls = sim.signal("calls");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(first), BigUint::from(0x00u8));
    assert_eq!(sim.get(calls), BigUint::from(1u8));
}

/// An output container formal is copied out to its actual when the
/// subroutine returns (IEEE 1800-2023 13.5.1): the actual is not a value
/// passed in, so what it held before the call does not reach the formal.
#[test]
fn output_container_formals_copy_out_only() {
    let source = r#"
    module Top(output int size, output int first, output int seen);
        parameter int W = 4;
        logic [W-1:0] r[$];
        task automatic fill(output logic [W-1:0] q[$], output int held);
            held = q.size();
            q.push_back(4'd5);
            q.push_back(4'd7);
        endtask
        initial begin
            r = '{4'd1, 4'd2, 4'd3};
            fill(r, seen);
            size = r.size();
            first = r[0];
        end
    endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("output-queue.sv"))], "Top")
            .build()
            .unwrap();
    let (size, first, seen) = (sim.signal("size"), sim.signal("first"), sim.signal("seen"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(size), BigUint::from(2u8));
    assert_eq!(sim.get(first), BigUint::from(5u8));
    assert_eq!(sim.get(seen), BigUint::from(0u8));
}
