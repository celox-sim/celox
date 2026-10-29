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
    assert!(error.contains("an array of `virtual other_if`"), "{error}");
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
