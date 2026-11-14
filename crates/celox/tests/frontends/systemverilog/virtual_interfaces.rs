use super::*;

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

/// An output argument copies the formal out to the actual (13.5.1): the
/// actual handle takes the instance the task chose, and receives no value
/// of the formal's type on the way in.
#[test]
fn output_arguments_copy_handles_out() {
    let source = "`timescale 1ns/1ns
        interface bus_if(input logic clk); logic [7:0] data; endinterface
        module top(output logic [7:0] seen);
            logic clk = 0;
            bus_if a(clk);
            bus_if b(clk);
            virtual bus_if chosen;
            task automatic pick(input bit second, output virtual bus_if v);
                if (second) v = b; else v = a;
            endtask
            initial begin
                a.data = 8'd3;
                b.data = 8'd7;
                pick(1'b1, chosen);
                #1 seen = chosen.data;
            end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "top")
        .build()
        .unwrap();
    let seen = sim.signal("seen");
    sim.run_until(2).unwrap();
    assert_eq!(sim.get(seen), num_bigint::BigUint::from(7u8));
}

/// The formal of an output or inout argument is copied out to its actual
/// (13.5.1), so an array of virtual interfaces is only copied to an actual
/// array of a compatible virtual interface type (25.9, 7.6).
#[test]
fn output_arrays_of_virtual_interfaces_need_compatible_actuals() {
    for (direction, actual, accepted) in [
        ("output", "virtual bus_if", true),
        ("output", "virtual other_if", false),
        ("inout", "virtual other_if", false),
    ] {
        let source = format!(
            "interface bus_if(input logic clk); logic data; endinterface
             interface other_if(input logic clk); logic data; endinterface
             module top;
                 logic clk = 0;
                 bus_if a(clk);
                 {actual} chosen[2];
                 task automatic pick({direction} virtual bus_if v[2]);
                     v[0] = a;
                     v[1] = a;
                 endtask
                 initial pick(chosen);
             endmodule"
        );
        let result =
            celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("vif.sv"))], "top")
                .build();
        assert_eq!(
            result.is_ok(),
            accepted,
            "{direction} {actual}: {:?}",
            result.err()
        );
    }
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

/// A class of a package reads and writes members through a virtual
/// interface at the widths and signedness the interface declares (IEEE
/// 1800-2023 25.9), though the package does not see the instance's
/// signals, as a UVM driver does.
#[test]
fn package_classes_read_members_at_their_declared_widths() {
    let source = "`timescale 1ns/1ns
        interface bus_if;
            logic clk;
            logic ready;
            logic valid;
            logic [31:0] read_data;
            logic signed [7:0] offset;
        endinterface
        package p;
            class drv;
                virtual bus_if BUS;
                int data;
                task run();
                    BUS.valid <= 1;
                    while (BUS.ready != 1) @(posedge BUS.clk);
                    data = BUS.read_data;
                    $display(\"%0t %h %0d %0d\", $time, data, BUS.valid, BUS.offset);
                endtask
            endclass
        endpackage
        module top;
            import p::*;
            bus_if b();
            drv d;
            initial begin b.clk = 0; b.ready = 0; b.offset = -3; forever #5 b.clk = ~b.clk; end
            initial begin #22 b.ready = 1; b.read_data = 32'hdeadbeef; end
            initial begin d = new; d.BUS = b; d.run(); end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("vif.sv"))], "top")
        .build()
        .unwrap();
    sim.run_until(40).unwrap();
    let lines: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(lines, ["25 deadbeef 1 -3"]);
}

/// The members of an interface that names a package (IEEE 1800-2023 26.3)
/// reach the methods of a class of another package through a virtual
/// interface of it, each in its own source, as the uvm-1.2 example
/// `uvm_use_models_pipelined_item_done` declares `mbus_if`
/// (`import mbus_types_pkg::*;`) and its driver: the declarations of the
/// interface are built analyzing only the packages it names.
#[test]
fn interfaces_naming_packages_reach_classes_through_virtual_interfaces() {
    let interface = "`timescale 1ns/1ns
        package types_pkg;
            typedef enum logic [1:0] {IDLE, SINGLE, BURST} op_e;
        endpackage
        interface bus_if;
            import types_pkg::*;
            logic clk;
            logic resetn;
            op_e opcode;
            types_pkg::op_e last;
        endinterface";
    let driver = "`timescale 1ns/1ns
        package driver_pkg;
            import types_pkg::*;
            class driver;
                virtual bus_if bus;
                int seen;
                task run();
                    @(posedge bus.resetn);
                    bus.opcode = BURST;
                    @(posedge bus.clk);
                    seen = bus.opcode + bus.last;
                endtask
            endclass
        endpackage";
    let top = "`timescale 1ns/1ns
        module Top(output int seen);
            import driver_pkg::*;
            bus_if bus();
            driver d;
            initial begin
                d = new;
                d.bus = bus;
                bus.last = types_pkg::SINGLE;
                bus.resetn = 0;
                bus.clk = 0;
                fork d.run(); join_none
                #1 bus.resetn = 1;
                #1 bus.clk = 1;
                #1 seen = d.seen;
            end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (interface, Path::new("bus_if.sv")),
            (driver, Path::new("driver_pkg.sv")),
            (top, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(sim.signal("seen")), 3u32.into());
}

/// So does one that names a package through an import of its source's
/// compilation unit (IEEE 1800-2023 3.12.1, 26.3), in a design whose other
/// sources import packages at their compilation unit's scope too.
#[test]
fn interfaces_naming_packages_through_unit_imports_reach_classes() {
    let interface = "`timescale 1ns/1ns
        package types_pkg;
            typedef enum logic [1:0] {IDLE, SINGLE, BURST} op_e;
        endpackage
        import types_pkg::*;
        interface bus_if;
            logic resetn;
            op_e opcode;
        endinterface";
    let driver = "`timescale 1ns/1ns
        package driver_pkg;
            import types_pkg::*;
            class driver;
                virtual bus_if bus;
                task run();
                    @(posedge bus.resetn);
                    bus.opcode = BURST;
                endtask
            endclass
        endpackage";
    let top = "`timescale 1ns/1ns
        import driver_pkg::*;
        module Top(output int seen);
            bus_if bus();
            driver d;
            initial begin
                d = new;
                d.bus = bus;
                bus.resetn = 0;
                fork d.run(); join_none
                #1 bus.resetn = 1;
                #1 seen = bus.opcode;
            end
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (interface, Path::new("bus_if.sv")),
            (driver, Path::new("driver_pkg.sv")),
            (top, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(sim.signal("seen")), 2u32.into());
}

/// A member of an enumeration type keeps its type through a virtual
/// interface: its value is assigned to a variable of that type without a
/// cast and has the enumeration's methods (IEEE 1800-2023 6.19.3, 6.19.5),
/// as the uvm-1.2 example `uvm_use_models_pipelined_item_done` reads
/// `req.MRESP = MBUS.MRESP;`.
#[test]
fn enumeration_members_keep_their_type_through_virtual_interfaces() {
    let source = "`timescale 1ns/1ns
        package types_pkg;
            typedef enum logic [2:0] {NONE, READ_VALID, WRITE_COMPLETE} resp_e;
        endpackage
        interface bus_if;
            import types_pkg::*;
            resp_e resp;
        endinterface
        module Top(output int code, output bit named);
            import types_pkg::*;
            class item;
                resp_e resp;
            endclass
            bus_if bus();
            virtual bus_if vif;
            item it;
            initial begin
                it = new;
                vif = bus;
                bus.resp = WRITE_COMPLETE;
                #1;
                it.resp = vif.resp;
                code = it.resp;
                named = vif.resp.name() == \"WRITE_COMPLETE\";
            end
        endmodule";
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("vif_enum.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(2).unwrap();
    assert_eq!(sim.get(sim.signal("code")), 2u32.into());
    assert_eq!(sim.get(sim.signal("named")), 1u8.into());
}

// A header port of an enumeration type keeps it too, and the bounds of an
// enumeration type take the values of the constants where it is declared,
// not those of a reader's constants of the same name.
#[test]
fn header_ports_and_package_constant_bounds_keep_their_enumeration_through_virtual_interfaces() {
    let source = "`timescale 1ns/1ns
        package types_pkg;
            localparam int W = 3;
            typedef enum logic [2:0] {NONE, READ_VALID, WRITE_COMPLETE} resp_e;
            typedef enum logic [W-1:0] {IDLE = 0, BUSY = 5} state_e;
        endpackage
        interface bus_if(input types_pkg::resp_e resp);
            types_pkg::state_e state;
        endinterface
        module Top(output int code, output bit named, output int width, output bit state_named);
            import types_pkg::*;
            localparam int W = 8;
            class item;
                resp_e resp;
            endclass
            resp_e driven;
            bus_if bus(driven);
            virtual bus_if vif;
            item it;
            initial begin
                it = new;
                vif = bus;
                driven = READ_VALID;
                bus.state = BUSY;
                #1;
                it.resp = vif.resp;
                code = it.resp;
                named = vif.resp.name() == \"READ_VALID\";
                width = $bits(vif.state);
                state_named = vif.state.name() == \"BUSY\";
            end
        endmodule";
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("vif_enum_ports.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(2).unwrap();
    assert_eq!(sim.get(sim.signal("code")), 1u32.into());
    assert_eq!(sim.get(sim.signal("named")), 1u8.into());
    assert_eq!(sim.get(sim.signal("width")), 3u32.into());
    assert_eq!(sim.get(sim.signal("state_named")), 1u8.into());
}
