use super::*;

sv_backends! {
    fn properties_arguments_and_member_writes(sim) {
        @case "virtual_interfaces::properties_arguments_and_member_writes";
    }

    fn null_equality_and_assignment(sim) {
        @case "virtual_interfaces::null_equality_and_assignment";
    }

    fn interface_ports_and_array_elements(sim) {
        @case "virtual_interfaces::interface_ports_and_array_elements";
    }

    fn four_state_members(sim) {
        @case "virtual_interfaces::four_state_members";
    }

    fn function_results(sim) {
        @case "virtual_interfaces::function_results";
    }

    fn rejects_call_results_of_other_types(sim) {
        @case "virtual_interfaces::rejects_call_results_of_other_types";
    }

    fn modport_ports_and_constant_sized_members(sim) {
        @case "virtual_interfaces::modport_ports_and_constant_sized_members";
    }

    fn rejects_modport_ports_to_full_interface(sim) {
        @case "virtual_interfaces::rejects_modport_ports_to_full_interface";
    }

    fn rejects_integers_in_arrays_of_virtual_interfaces(sim) {
        @case "virtual_interfaces::rejects_integers_in_arrays_of_virtual_interfaces";
    }

    fn member_selects(sim) {
        @case "virtual_interfaces::member_selects";
    }

    fn rejects_integers_in_conditional_array_elements(sim) {
        @case "virtual_interfaces::rejects_integers_in_conditional_array_elements";
    }

    fn increments_arrays_and_package_variables(sim) {
        @case "virtual_interfaces::increments_arrays_and_package_variables";
    }

    fn multidimensional_and_package_arrays(sim) {
        @case "virtual_interfaces::multidimensional_and_package_arrays";
    }

    fn class_call_arguments_and_package_results(sim) {
        @case "virtual_interfaces::class_call_arguments_and_package_results";
    }

    fn rejects_package_results_of_other_interfaces(sim) {
        @case "virtual_interfaces::rejects_package_results_of_other_interfaces";
    }

    fn rejects_integers_as_class_call_arguments(sim) {
        @case "virtual_interfaces::rejects_integers_as_class_call_arguments";
    }

    fn rejects_integers_as_constructor_arguments(sim) {
        @case "virtual_interfaces::rejects_integers_as_constructor_arguments";
    }

    fn writes_from_instances_of_one_module(sim) {
        @case "virtual_interfaces::writes_from_instances_of_one_module";
    }

    fn rejects_virtual_interfaces_as_integral_references(sim) {
        @case "virtual_interfaces::rejects_virtual_interfaces_as_integral_references";
    }

    fn increments_in_expressions(sim) {
        @case "virtual_interfaces::increments_in_expressions";
    }

    fn queue_pops_and_output_arguments(sim) {
        @case "virtual_interfaces::queue_pops_and_output_arguments";
    }

    fn rejects_another_interface(sim) {
        @case "virtual_interfaces::rejects_another_interface";
    }

    fn rejects_modport_to_full_interface(sim) {
        @case "virtual_interfaces::rejects_modport_to_full_interface";
    }

    fn waits_on_member_edges(sim) {
        @case "virtual_interfaces::waits_on_member_edges";
    }

    fn waits_on_member_levels(sim) {
        @case "virtual_interfaces::waits_on_member_levels";
    }

    fn waits_with_guards_signed_members_and_automatic_tasks(sim) {
        @case "virtual_interfaces::waits_with_guards_signed_members_and_automatic_tasks";
    }

    fn mixed_event_controls(sim) {
        @case "virtual_interfaces::mixed_event_controls";
    }

    fn retargets_with_ordinary_events(sim) {
        @case "virtual_interfaces::retargets_with_ordinary_events";
    }

    fn member_pulses_within_one_activation(sim) {
        @case "virtual_interfaces::member_pulses_within_one_activation";
    }

    fn always_on_member_edges(sim) {
        @case "virtual_interfaces::always_on_member_edges";
    }

    fn type_parameter_values(sim) {
        @case "virtual_interfaces::type_parameter_values";
    }

    fn package_generic_member_reads(sim) {
        @case "virtual_interfaces::package_generic_member_reads";
    }

    fn package_configuration_database(sim) {
        @case "virtual_interfaces::package_configuration_database";
    }

    fn unit_import_package_variables(sim) {
        @case "virtual_interfaces::unit_import_package_variables";
    }
}

/// A `wait` through a handle that a class property, an automatic variable
/// or an argument holds would have to follow changes of the handle while
/// its condition is evaluated (IEEE 1800-2023 9.4.3), which is not
/// implemented; it is rejected rather than sampled.
#[test]
fn level_waits_through_non_static_handles_are_unsupported() {
    let waits = [
        "class C; virtual bus_if vif; task run(virtual bus_if b); vif = b; wait (vif.clk); endtask endclass",
        "class C; task run(virtual bus_if b); virtual bus_if v; v = b; wait (v.clk); endtask endclass",
    ];
    for wait in waits {
        let source = format!(
            "interface bus_if(input logic clk); endinterface
             {wait}
             module top; logic clk = 0; bus_if bus(clk);
                 initial begin automatic C c = new; c.run(bus); end
             endmodule"
        );
        let Err(error) =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("vif.sv"))], "top")
                .build_interpreter()
        else {
            panic!("a wait through a non-static handle is unsupported: {wait}");
        };
        let error = error.to_string();
        assert!(
            error.contains("wait on a member through a virtual interface held by"),
            "{wait}: {error}"
        );
    }
}

/// An event control through a handle that an argument of an automatic
/// module task holds reads the handle when the wait begins (9.4.2) and
/// waits on the member of the instance it denotes then.
#[test]
fn event_waits_through_task_arguments() {
    let source = "`timescale 1ns/1ns
        interface bus_if(input logic clk); endinterface
        module top(output int seen);
            logic clk = 0;
            bus_if bus(clk);
            always #5 clk = ~clk;
            task automatic run(virtual bus_if b); repeat (2) @(posedge b.clk); endtask
            initial begin run(bus); seen = $time; end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "top")
        .build()
        .unwrap();
    let seen = sim.signal("seen");
    sim.run_until(30).unwrap();
    assert_eq!(sim.get(seen), num_bigint::BigUint::from(15u8));
}

/// A member access compares the handle with each instance, so a handle
/// expression whose evaluation may have effects is rejected; and the
/// instance a port of an interface without members denotes is unknown.
#[test]
fn unsupported_handle_forms_are_diagnosed() {
    for (source, want) in [
        (
            "interface bus_if(input logic clk); logic data; endinterface
             module Top;
                 logic clk = 0;
                 bus_if a(clk);
                 virtual bus_if handles[2];
                 int calls;
                 function automatic int pick(); calls++; return 0; endfunction
                 initial begin handles[0] = a; handles[pick()].data = 1; end
             endmodule",
            "virtual interface handle expression with a call",
        ),
        (
            "interface empty_if; endinterface
             module Child(empty_if p);
                 virtual empty_if v;
                 initial v = p;
             endmodule
             module Top; empty_if e(); Child c(e); endmodule",
            "which declares no members",
        ),
    ] {
        let Err(error) =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "Top")
                .build_interpreter()
        else {
            panic!("expected `{want}`");
        };
        let error = error.to_string();
        assert!(error.contains(want), "{error}");
    }
}

/// A modport declares directions as if inside a module, and a variable
/// declared as an input port may not be assigned (IEEE 1800-2023 25.5,
/// 23.3.3.4), whether or not the assignment runs. The case is not shared
/// with the external simulators: Verilator 5.052 accepts it, and xezim
/// 0.11.3 rejects it without a source position.
#[test]
fn writes_to_input_modport_members_are_rejected() {
    for write in ["m.data = 1;", "m.data <= 1;", "m.data += 1;"] {
        let source = format!(
            "interface bus_if(input logic clk);
                 logic data;
                 modport mon(input data);
             endinterface
             module Top;
                 logic clk = 0;
                 bus_if bus(clk);
                 virtual bus_if.mon m;
                 initial begin m = bus; if (clk) {write} end
             endmodule"
        );
        let Err(error) =
            celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("vif.sv"))], "Top")
                .build_interpreter()
        else {
            panic!("{write} must be rejected");
        };
        let error = error.to_string();
        assert!(
            error.contains("an input of `virtual bus_if.mon`"),
            "{write}: {error}"
        );
    }
}

/// A chandle member read through a virtual interface is a chandle (IEEE
/// 1800-2023 6.14, 25.9): it compares with null, and is no integral operand.
#[test]
fn chandle_members_through_virtual_interfaces_are_chandles() {
    let source = |test: &str| {
        format!(
            "interface bus_if; chandle h; endinterface
             module Top;
                 bus_if bus();
                 virtual bus_if v;
                 int n;
                 initial begin v = bus; n = {test} ? 1 : 0; end
             endmodule"
        )
    };
    let build = |test: &str| {
        let source = source(test);
        celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("vif.sv"))], "Top")
            .build_interpreter()
            .map(drop)
            .map_err(|error| error.to_string())
    };
    build("v.h == null").unwrap();
    let error = build("v.h + 1 != 0").unwrap_err();
    assert!(error.contains("chandle"), "{error}");
}

/// An element of a queue of virtual interfaces, indexed or popped, is a
/// virtual interface of the queue's element type (IEEE 1800-2023 7.10,
/// 25.9): it is assigned to a virtual interface of that type, and only of
/// that type.
#[test]
fn queue_elements_of_virtual_interfaces_are_virtual_interfaces() {
    let build = |target: &str, value: &str| {
        let source = format!(
            "interface bus_if(input logic clk); logic [7:0] data; endinterface
             interface other_if(input logic clk); logic [7:0] data; endinterface
             module Top;
                 logic clk = 0;
                 bus_if bus(clk);
                 virtual bus_if q[$];
                 virtual {target} v;
                 initial begin q.push_back(bus); v = {value}; end
             endmodule"
        );
        celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("vif.sv"))], "Top")
            .build_interpreter()
            .map(drop)
            .map_err(|error| error.to_string())
    };
    for value in ["q[0]", "q.pop_front()", "q.pop_back()"] {
        build("bus_if", value).unwrap_or_else(|error| panic!("{value}: {error}"));
        assert!(
            build("other_if", value).is_err(),
            "{value} assigned to other_if"
        );
    }
}

/// A member through a virtual interface is written only through the
/// selection among the instances; a concatenation target or a continuous
/// assignment through one is unsupported rather than writing nothing.
#[test]
fn other_writes_through_members_are_unsupported() {
    for write in [
        "initial begin v = bus; {v.data} = 8'h1; end",
        "initial begin v = bus; {v.data[3:0], v.data[7:4]} = 8'h1; end",
    ] {
        let source = format!(
            "interface bus_if(input logic clk); logic [7:0] data; endinterface
             module Top;
                 logic clk = 0;
                 bus_if bus(clk);
                 virtual bus_if v;
                 {write}
             endmodule"
        );
        let Err(error) =
            celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("vif.sv"))], "Top")
                .build_interpreter()
        else {
            panic!("{write} must be rejected");
        };
        let error = error.to_string();
        assert!(
            error.contains("this form of assignment to `data` through a virtual interface"),
            "{write}: {error}"
        );
    }
}

/// Copying an array of virtual interfaces requires assignment-compatible
/// elements (IEEE 1800-2023 25.9, 7.6). Not a shared case: Verilator 5.052
/// emits C++ that fails to compile, and xezim 0.11.3 accepts the copy.
#[test]
fn copies_of_arrays_of_another_interface_are_rejected() {
    let source = "interface bus_if(input logic clk); logic data; endinterface
        interface other_if(input logic clk); logic data; endinterface
        module Top;
            virtual bus_if dst[2];
            virtual other_if src[2];
            initial dst = src;
        endmodule";
    let Err(error) = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "Top")
        .build_interpreter()
    else {
        panic!("an array of another interface must be rejected");
    };
    let error = error.to_string();
    assert!(error.contains("virtual other_if"), "{error}");
}

/// The modport of a virtual interface is part of its type (IEEE 1800-2023
/// 25.9), so arrays whose elements select another modport, or none, are
/// not of equivalent element types and are not assignment compatible (7.6),
/// although a single virtual interface without a modport may be assigned to
/// one with a modport.
#[test]
fn copies_of_arrays_of_another_modport_are_rejected() {
    for (dst, src) in [("bus_if.mp", "bus_if"), ("bus_if", "bus_if.mp")] {
        let source = format!(
            "interface bus_if(input logic clk); logic data; modport mp(input data); endinterface
            module Top;
                virtual {dst} dst[2];
                virtual {src} src[2];
                initial dst = src;
            endmodule"
        );
        let Err(error) =
            celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("vif.sv"))], "Top")
                .build_interpreter()
        else {
            panic!("an array of virtual {src} must not be assigned to one of virtual {dst}");
        };
        let error = error.to_string();
        assert!(error.contains(&format!("virtual {src} [2]")), "{error}");
    }
}

/// An equality may compare a virtual interface with a conditional of
/// virtual interfaces of the same interface (IEEE 1800-2023 11.4.11, 25.9).
/// Not a shared case: Verilator 5.052 rejects a virtual interface operand of
/// a conditional, and xezim 0.11.3 compares it unequal.
#[test]
fn equalities_with_conditional_virtual_interfaces() {
    let source = "interface bus_if(input logic clk); endinterface
        module Top(output bit same, output bit other);
            logic clk = 0;
            bus_if a(clk);
            bus_if b(clk);
            virtual bus_if v;
            virtual bus_if w;
            bit flag = 1;
            initial begin
                v = a;
                w = b;
                same = w == (flag ? w : v);
                other = v == (flag ? w : v);
            end
        endmodule";
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "Top")
                .four_state(four_state)
                .build_native()
                .unwrap();
        sim.run_until(0).unwrap();
        let (same, other) = (sim.signal("same"), sim.signal("other"));
        assert_eq!(sim.get(same), BigUint::from(1u8));
        assert_eq!(sim.get(other), BigUint::from(0u8));
    }
}

/// A virtual interface is assigned and compared, not computed with
/// (IEEE 1800-2023 25.9): a compound assignment or an increment of a handle
/// would retarget it to an arbitrary instance.
#[test]
fn arithmetic_on_handles_is_rejected() {
    for write in ["v += 1;", "v++;", "v--;", "v -= 1;"] {
        let source = format!(
            "interface bus_if(input logic clk); logic data; endinterface
             module Top;
                 logic clk = 0;
                 bus_if a(clk);
                 bus_if b(clk);
                 virtual bus_if v;
                 initial begin v = a; {write} end
             endmodule"
        );
        let Err(error) =
            celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("vif.sv"))], "Top")
                .build_interpreter()
        else {
            panic!("{write} must be rejected");
        };
        let error = error.to_string();
        assert!(error.contains("operator"), "{write}: {error}");
    }
}

/// A handle is an operand of the event expression, so retargeting it to
/// `null` while a process waits on a member through it is an access through
/// `null` (IEEE 1800-2023 9.4.2, 25.9), not a falling edge of the member.
#[test]
fn retargeting_a_waited_handle_to_null_is_an_error() {
    let source = "`timescale 1ns/1ns
        interface bus_if(input logic clk); endinterface
        module Top(output bit woke);
            logic high = 1;
            bus_if a(high);
            virtual bus_if vif;
            initial begin
                vif = a;
                fork
                    begin @(negedge vif.clk); woke = 1; end
                    begin #2 vif = null; end
                join
            end
        endmodule";
    for four_state in [false, true] {
        let builder = || {
            celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "Top")
                .four_state(four_state)
        };
        let runs = [
            builder()
                .build_native()
                .map(|mut sim| sim.run_until(5).map(|_| sim.get(sim.signal("woke")))),
            builder()
                .build_cranelift()
                .map(|mut sim| sim.run_until(5).map(|_| sim.get(sim.signal("woke")))),
            builder()
                .build_wasm()
                .map(|mut sim| sim.run_until(5).map(|_| sim.get(sim.signal("woke")))),
        ];
        for run in runs {
            let run = run.expect("the design builds");
            let Err(error) = run else {
                panic!("an access through null must stop the run: {run:?}");
            };
            let error = error.to_string();
            assert!(error.contains("null virtual interface accessed"), "{error}");
        }
    }
}

/// The reads of an event control through a virtual interface that a class
/// property holds are its timing, not reads of an enclosing `always @*`
/// (IEEE 1800-2023 9.4.2.2): a change of the interface member does not run
/// the `always @*` again.
#[test]
fn implicit_events_leave_out_waits_through_class_properties() {
    let source = "`timescale 1ns/1ns
        interface clk_if; logic clk = 0; endinterface
        class Env; virtual clk_if m_if; endclass
        module top(output int runs);
            clk_if i0();
            Env e = new;
            bit go;
            event ev;
            always #5 i0.clk = ~i0.clk;
            always @* begin
                -> ev;
                if (go) @(posedge e.m_if.clk);
            end
            // The first run at 0 races the counting process; count after it.
            always @(ev) if ($time > 0) runs++;
            initial begin e.m_if = i0; #2 go = 1; end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "top")
        .build()
        .unwrap();
    let runs = sim.signal("runs");
    sim.run_until(60).unwrap();
    assert_eq!(sim.get(runs), num_bigint::BigUint::from(1u8));
}

/// Each item of an event control evaluates its handle when the wait begins,
/// once: an index with a call that selects another interface each time
/// gives each item its own (IEEE 1800-2023 9.4.2).
#[test]
fn event_items_evaluate_their_handles_separately() {
    let source = "`timescale 1ns/1ns
        interface clk_if; logic clk = 0; logic other = 0; endinterface
        module top(output int woke);
            clk_if i0();
            clk_if i1();
            virtual clk_if handles[2];
            int calls;
            function automatic int pick();
                calls++;
                return calls - 1;
            endfunction
            task automatic watch();
                @(posedge handles[pick()].clk or posedge handles[pick()].other);
            endtask
            initial begin #5 i0.clk = 1; end
            initial begin #3 i1.other = 1; end
            initial begin handles[0] = i0; handles[1] = i1; watch(); woke = $time; end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "top")
        .build()
        .unwrap();
    let woke = sim.signal("woke");
    sim.run_until(10).unwrap();
    assert_eq!(sim.get(woke), num_bigint::BigUint::from(3u8));
}

/// A handle that design storage holds, a variable or an element of an array
/// of the design, is an operand of the wait's event expression: a change of
/// the index or of the variable during the wait retargets it (IEEE
/// 1800-2023 9.4.2).
#[test]
fn waits_follow_design_storage_handles() {
    let source = "`timescale 1ns/1ns
        interface clk_if; logic clk = 0; endinterface
        module top(output int by_index, output int by_variable);
            clk_if i0();
            clk_if i1();
            virtual clk_if handles[2];
            virtual clk_if current;
            int idx;
            initial begin #5 i0.clk = 1; end
            initial begin #3 i1.clk = 1; end
            initial begin
                handles[0] = i0;
                handles[1] = i1;
                current = i0;
                fork
                    begin @(posedge handles[idx].clk); by_index = $time; end
                    begin @(posedge current.clk); by_variable = $time; end
                    begin #2 idx = 1; current = i1; end
                join
            end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "top")
        .build()
        .unwrap();
    let (by_index, by_variable) = (sim.signal("by_index"), sim.signal("by_variable"));
    sim.run_until(10).unwrap();
    assert_eq!(sim.get(by_index), num_bigint::BigUint::from(3u8));
    assert_eq!(sim.get(by_variable), num_bigint::BigUint::from(3u8));
}

/// A wait through a virtual interface that a ref formal names would follow
/// the actual's storage, which is not implemented: it is rejected rather
/// than sampled or left without events.
#[test]
fn waits_through_reference_formals_are_unsupported() {
    let source = "interface bus_if(input logic clk); endinterface
        module top;
            logic clk = 0;
            bus_if bus(clk);
            virtual bus_if current;
            task automatic watch(ref virtual bus_if h); @(posedge h.clk); endtask
            initial begin current = bus; watch(current); end
        endmodule";
    let Err(error) = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "top")
        .build_interpreter()
    else {
        panic!("a wait through a ref formal is unsupported");
    };
    assert!(error.to_string().contains("a ref formal names"), "{error}");
}

/// A handle that a container variable of the design or a static local holds
/// is storage of the design too: a change of the index, or of the static by
/// an overlapping activation, during the wait retargets it (IEEE 1800-2023
/// 6.21, 9.4.2).
#[test]
fn waits_follow_containers_and_statics() {
    let source = "`timescale 1ns/1ns
        interface clk_if; logic clk = 0; endinterface
        module top(output int by_queue, output int first, output int second);
            clk_if i0();
            clk_if i1();
            virtual clk_if queue_handles[$];
            int idx;
            task watch(virtual clk_if v);
                static virtual clk_if s;
                s = v;
                @(posedge s.clk);
            endtask
            initial begin #5 i0.clk = 1; end
            initial begin #3 i1.clk = 1; end
            initial begin
                queue_handles.push_back(i0);
                queue_handles.push_back(i1);
                fork
                    begin @(posedge queue_handles[idx].clk); by_queue = $time; end
                    begin #2 idx = 1; end
                    begin watch(i0); first = $time; end
                    begin #2 watch(i1); second = $time; end
                join
            end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "top")
        .build()
        .unwrap();
    let signals = [
        sim.signal("by_queue"),
        sim.signal("first"),
        sim.signal("second"),
    ];
    sim.run_until(10).unwrap();
    for signal in signals {
        assert_eq!(sim.get(signal), num_bigint::BigUint::from(3u8));
    }
}

/// Each activation of a wait that reads its handle when it begins has its
/// own copy: overlapping activations of the same statement, forked in a
/// loop, wait on the interfaces they read (IEEE 1800-2023 9.4.2).
#[test]
fn handle_copies_belong_to_their_activation() {
    let source = "`timescale 1ns/1ns
        interface clk_if; logic clk = 0; endinterface
        class Env; virtual clk_if m_if; endclass
        module top(output int woke0, output int woke1);
            clk_if i0();
            clk_if i1();
            Env objs[2];
            initial begin #5 i0.clk = 1; end
            initial begin #3 i1.clk = 1; end
            initial begin
                objs[0] = new;
                objs[1] = new;
                objs[0].m_if = i0;
                objs[1].m_if = i1;
                for (int j = 0; j < 2; j++)
                    fork
                        automatic int k = j;
                        begin
                            @(posedge objs[k].m_if.clk);
                            if (k == 0) woke0 = $time; else woke1 = $time;
                        end
                    join_none
            end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "top")
        .build()
        .unwrap();
    let (woke0, woke1) = (sim.signal("woke0"), sim.signal("woke1"));
    sim.run_until(10).unwrap();
    assert_eq!(sim.get(woke0), num_bigint::BigUint::from(5u8));
    assert_eq!(sim.get(woke1), num_bigint::BigUint::from(3u8));
}

#[test]
fn virtual_interface_members_keep_package_and_unit_types() {
    // The members of an interface that a virtual interface reaches have the
    // types the interface declares them with, also through a package import
    // or a compilation-unit typedef, which building its declarations needs
    // the packages for (IEEE 1800-2023 25.9, 26.3).
    let source = r#"
        typedef logic [11:0] word_t;
        package P;
            typedef logic [7:0] byte_t;
        endpackage
        interface ifc;
            import P::*;
            byte_t data;
            word_t wide;
        endinterface
        module Top(output logic [7:0] data, output logic [11:0] wide);
            ifc bus();
            virtual ifc vif;
            initial begin
                bus.data = 8'hab;
                bus.wide = 12'hcde;
                vif = bus;
                #1;
                data = vif.data;
                wide = vif.wide;
            end
        endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(
            &format!("`timescale 1ns/1ns\n{source}"),
            Path::new("vif_types.sv"),
        )],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(2).unwrap();
    assert_eq!(sim.get(sim.signal("data")), 0xabu8.into());
    assert_eq!(sim.get(sim.signal("wide")), 0xcdeu16.into());
}

sv_backends! {
    // An enclosing `@*` reads only the statements, not the handle and member
    // of an event control nested in a named block that a `disable` names
    // (IEEE 1800-2023 9.4.2.2, 9.6.2): the clock the nested control waits
    // for does not rerun the statements.
    fn implicit_event_ignores_nested_control_in_disabled_block(sim) {
        @setup { let source = r#"`timescale 1ns/1ns
interface bus_if(input logic clk); endinterface
module Top(output int outs, output int last);
    logic tick = 0;
    always #2 tick = ~tick;
    bus_if ticking(tick);
    virtual bus_if clocked;
    logic [7:0] data = 0;
    always @* begin : sample
        @(posedge clocked.clk);
        if (data == 8'h00) disable sample;
        outs = outs + 1;
        last = int'($time);
    end
    initial begin
        clocked = ticking;
        #3 data = 8'haa;
    end
endmodule
"#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("implicit_event_in_disabled_block.sv"))], "Top");
        sim.run_until(12).unwrap();
        assert_eq!(sim.get(sim.signal("outs")), 1u32.into());
        assert_eq!(sim.get(sim.signal("last")), 6u32.into());
    }
}
