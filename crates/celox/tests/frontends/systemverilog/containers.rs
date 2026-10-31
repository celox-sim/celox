use super::*;

sv_backends! {
    fn class_container_inheritance_static(sim) { @case "containers::class_container_inheritance_static"; }
    fn nested_containers_and_class_keys(sim) { @case "containers::nested_containers_and_class_keys"; }
    fn methods_of_nested_container_elements(sim) { @case "containers::methods_of_nested_container_elements"; }
    fn nested_updates_evaluate_indices_once(sim) { @case "containers::nested_updates_evaluate_indices_once"; }
    fn class_keys_survive_heap_collections(sim) { @case "containers::class_keys_survive_heap_collections"; }
    fn class_container_receiver_evaluation(sim) { @case "containers::class_container_receiver_evaluation"; }
    fn class_container_properties(sim) { @case "containers::class_container_properties"; }
    fn class_container_queue_pool(sim) { @case "containers::class_container_queue_pool"; }
    fn class_container_strings_handles(sim) { @case "containers::class_container_strings_handles"; }
    fn class_container_four_state(sim) { @case "containers::class_container_four_state"; }
    fn class_container_copy_and_gc(sim) { @case "containers::class_container_copy_and_gc"; }
    fn ordering_integral(sim) { @case "containers::ordering_integral"; }
    fn ordering_strings(sim) { @case "containers::ordering_strings"; }
    fn ordering_four_state(sim) { @case "containers::ordering_four_state"; }
    fn ordering_handles(sim) { @case "containers::ordering_handles"; }
    fn ordering_events(sim) { @case "containers::ordering_events"; }
    fn optional_parentheses_packed_result(sim) { @case "containers::optional_parentheses_packed_result"; }
    fn optional_parentheses(sim) { @case "containers::optional_parentheses"; }
    fn optional_parentheses_queries(sim) { @case "containers::optional_parentheses_queries"; }
    fn optional_parentheses_four_state(sim) { @case "containers::optional_parentheses_four_state"; }
    fn array_reduction_empty_identities(sim) { @case "containers::array_reduction_empty_identities"; }
    fn array_reduction_type_queries(sim) { @case "containers::array_reduction_type_queries"; }
    fn array_reduction_fixed(sim) { @case "containers::array_reduction_fixed"; }
    fn array_reduction_dynamic_queue(sim) { @case "containers::array_reduction_dynamic_queue"; }
    fn array_reduction_associative(sim) { @case "containers::array_reduction_associative"; }
    fn array_reduction_four_state(sim) { @case "containers::array_reduction_four_state"; }
    fn queue_argument_side_effects(sim) { @case "containers::queue_argument_side_effects"; }
    fn queue_argument_four_state(sim) { @case "containers::queue_argument_four_state"; }
    fn string_associative_foreach(sim) { @case "containers::string_associative_foreach"; }
    fn object_elements_keep_handle_semantics(sim) { @case "containers::object_elements_keep_handle_semantics"; }
    fn runtime_queries_and_traversal_conversion(sim) { @case "containers::runtime_queries_and_traversal_conversion"; }
    fn equal_updates_preserve_event_waits(sim) { @case "containers::equal_updates_preserve_event_waits"; }
    fn bounded_queue_and_equality(sim) { @case "containers::bounded_queue_and_equality"; }
    fn four_state_container_equality(sim) { @case "containers::four_state_container_equality"; }
    fn string_elements_and_gc(sim) { @case "containers::string_elements_and_gc"; }
    fn foreach_and_control_flow(sim) { @case "containers::foreach_and_control_flow"; }
    fn dynamic_resize_and_copy(sim) { @case "containers::dynamic_resize_and_copy"; }
    fn queue_methods(sim) { @case "containers::queue_methods"; }
    fn associative_integer_keys(sim) { @case "containers::associative_integer_keys"; }
    fn associative_string_keys(sim) { @case "containers::associative_string_keys"; }
    fn four_state_elements_and_indices(sim) { @case "containers::four_state_elements_and_indices"; }
    fn declaration_initializers_and_locals(sim) { @case "containers::declaration_initializers_and_locals"; }
}

sv_backends! {
    fn popped_nested_queue_elements_are_containers(sim) {
        @setup {
            let source = r#"module Top(output int value);
                initial begin
                    int q[$][$];
                    int row[$];
                    row.push_back(4);
                    row.push_back(2);
                    q.push_back(row);
                    row = '{};
                    row = q.pop_front();
                    value = row.size() * 100 + row[0] * 10 + q.size();
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("nested.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(240u8));
    }
}

sv_backends! {
    fn container_checkpoints_restore_all_three_kinds(sim) {
        @setup {
            let source = r#"module Top(output int value);
                timeunit 1ps / 1ps;
                int a[], q[$], aa[int];
                initial begin
                    a = new[1]; a[0] = 1; q.push_back(2); aa[0] = 3;
                    #10 value = a[0] + q[0] + aa[0];
                    #10 a[0] = 4; q.push_back(5); aa[0] = 6;
                    #10 value = a[0] + q[1] + aa[0];
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("container_checkpoint.sv"))], "Top");
        sim.run_until(5).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        let bytes = checkpoint.to_bytes().unwrap();
        // SAFETY: bytes came directly from this simulator's checkpoint.
        let persisted = unsafe { sim.checkpoint_from_bytes(&bytes) }.unwrap();
        let value = sim.signal("value");
        sim.run_until(40).unwrap();
        assert_eq!(sim.get(value), BigUint::from(15u8));
        sim.restore(&persisted).unwrap();
        sim.run_until(15).unwrap();
        assert_eq!(sim.get(value), BigUint::from(6u8));
        sim.run_until(40).unwrap();
        assert_eq!(sim.get(value), BigUint::from(15u8));
    }
}

sv_backends! {
    fn negative_dynamic_array_size_is_a_runtime_error(sim) {
        @setup { let source = "module Top; int a[]; initial a = new[-1]; endmodule"; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("negative_array.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("negative dynamic array size"), "{error}");
    }
}

#[test]
fn container_element_events_reject_mutating_index_calls() {
    for event in ["q.pop_front", "q[q.pop_front()]", "q[q.pop_back]"] {
        let source =
            format!("module Top; int q[$]; initial begin q = {{1, 0}}; @({event}); end endmodule");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("impure-container-event.sv"))],
            "Top",
        )
        .build_native()
        .err()
        .unwrap_or_else(|| panic!("a mutating call in an event expression must be rejected"))
        .to_string();
        assert!(error.contains("impure call"), "{event}: {error}");
    }
}

#[test]
fn ordering_survives_native_images_and_serialized_checkpoints() {
    let source = r#"module Top(output int value);
        timeunit 1ps / 1ps;
        int q[$];
        initial begin
            q = {3, 1, 2};
            #10 q.sort; value = q[0];
            #10 q.rsort; value = q[0];
            #10 q.reverse; value = q[0];
        end
    endmodule"#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("ordering-interpreter.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build_interpreter()
        .unwrap();
        let value = sim.signal("value");
        sim.run_until(15).unwrap();
        assert_eq!(sim.get(value), BigUint::from(1u8));
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(value), BigUint::from(3u8));
        sim.run_until(35).unwrap();
        assert_eq!(sim.get(value), BigUint::from(1u8));
    }
    let image =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("ordering-image.sv"))], "Top")
            .compile_native()
            .unwrap()
            .into_program_image();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    let mut sim = celox::Simulation::from_sv_sources(Vec::new(), "Top")
        .build_native_from_image(image)
        .unwrap();
    let value = sim.signal("value");
    sim.run_until(15).unwrap();
    assert_eq!(sim.get(value), BigUint::from(1u8));
    let bytes = sim.checkpoint().unwrap().to_bytes().unwrap();
    // SAFETY: bytes were serialized from this simulator's checkpoint.
    let checkpoint = unsafe { sim.checkpoint_from_bytes(&bytes) }.unwrap();
    sim.run_until(25).unwrap();
    assert_eq!(sim.get(value), BigUint::from(3u8));
    sim.run_until(35).unwrap();
    assert_eq!(sim.get(value), BigUint::from(1u8));
    sim.restore(&checkpoint).unwrap();
    sim.run_until(25).unwrap();
    assert_eq!(sim.get(value), BigUint::from(3u8));
    sim.run_until(35).unwrap();
    assert_eq!(sim.get(value), BigUint::from(1u8));
}

#[test]
fn class_container_native_images_and_checkpoints_preserve_object_graphs() {
    let source = r#"module Top(output int value);
        timeunit 1ps / 1ps;
        class Store; int data[], q[$], map[string]; endclass
        Store s;
        initial begin
            s = new; s.data = '{1, 2}; s.q.push_back(3); s.map["key"] = 4;
            #10 value = s.data[0] + s.q[0] + s.map["key"];
            #10 s.data[0] = 5; s.q.push_back(6); s.map["key"] = 7;
            #10 value = s.data[0] + s.q[1] + s.map["key"];
        end
    endmodule"#;
    for four_state in [false, true] {
        let mut interpreted = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("class-container-interpreter.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build_interpreter()
        .unwrap();
        interpreted.run_until(35).unwrap();
        assert_eq!(
            interpreted.get(interpreted.signal("value")),
            BigUint::from(18u8)
        );
        let image = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("class-container-image.sv"))],
            "Top",
        )
        .four_state(four_state)
        .compile_native()
        .unwrap()
        .into_program_image();
        let image =
            celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
                .unwrap();
        let mut sim = celox::Simulation::from_sv_sources(Vec::new(), "Top")
            .build_native_from_image(image)
            .unwrap();
        sim.run_until(15).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(8u8));
        let bytes = sim.checkpoint().unwrap().to_bytes().unwrap();
        // SAFETY: bytes came from this simulator's checkpoint.
        let checkpoint = unsafe { sim.checkpoint_from_bytes(&bytes) }.unwrap();
        sim.run_until(35).unwrap();
        assert_eq!(sim.get(value), BigUint::from(18u8));
        sim.restore(&checkpoint).unwrap();
        sim.run_until(35).unwrap();
        assert_eq!(sim.get(value), BigUint::from(18u8));
    }
}

#[test]
fn class_container_methods_reject_null_receivers() {
    let source =
        "module Top; class Store; int q[$]; endclass Store s; initial s.q.push_back(1); endmodule";
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(source, Path::new("null-class-container.sv"))],
        "Top",
    )
    .build_native()
    .unwrap();
    let error = sim.run_until(0).unwrap_err().to_string();
    assert!(error.contains("null object handle"), "{error}");
}

const EMPTY_REDUCTION_MODES: &str = r#"
module Top(output bit [63:0] dp, da, qp, qa, ap, aa, sp, sa, output bit [3:0] flags);
    bit [63:0] values[], q[$], numbers[int], words[string];
    initial begin
        dp = values.product(); da = values.and();
        qp = q.product(); qa = q.and();
        ap = numbers.product(); aa = numbers.and();
        sp = words.product(); sa = words.and();
        flags[0] = values.sum() == 0 && values.or() == 0 && values.xor() == 0;
        q = {6, 3};
        flags[1] = q.product() == 18 && q.and() == 2;
        numbers[-5] = 6; numbers[10] = 3;
        flags[2] = numbers.product() == 18 && numbers.and() == 2;
        words["alpha"] = 6; words["beta"] = 3;
        flags[3] = words.product() == 18 && words.and() == 2;
    end
endmodule
"#;

fn check_empty_reduction_mode<B: celox::SimBackend>(mut sim: celox::Simulation<B>, legacy: bool) {
    sim.run_until(0).unwrap();
    for name in ["dp", "qp", "ap", "sp"] {
        assert_eq!(
            sim.get(sim.signal(name)),
            BigUint::from(u8::from(!legacy)),
            "{name}, legacy={legacy}"
        );
    }
    for name in ["da", "qa", "aa", "sa"] {
        assert_eq!(
            sim.get(sim.signal(name)),
            BigUint::from(if legacy { 0 } else { u64::MAX }),
            "{name}, legacy={legacy}"
        );
    }
    assert_eq!(sim.get(sim.signal("flags")), BigUint::from(15u8));
}

#[test]
fn empty_reduction_compatibility_is_local_to_each_build() {
    // Alternate modes with identical source text, including defaults after opt-in.
    for legacy in [false, true, false] {
        for four_state in [false, true] {
            let build = || {
                celox::Simulation::from_sv_sources(
                    vec![(EMPTY_REDUCTION_MODES, Path::new("empty_modes.sv"))],
                    "Top",
                )
                .legacy_empty_reductions(legacy)
                .four_state(four_state)
            };
            check_empty_reduction_mode(build().build_interpreter().unwrap(), legacy);
            check_empty_reduction_mode(build().build_cranelift().unwrap(), legacy);
            check_empty_reduction_mode(build().build_wasm().unwrap(), legacy);
            check_empty_reduction_mode(build().build_native().unwrap(), legacy);
            check_empty_reduction_mode(
                build()
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_native()
                    .unwrap(),
                legacy,
            );
        }
    }
}

#[test]
fn empty_reduction_compatibility_survives_native_images() {
    for legacy in [false, true] {
        let image = celox::Simulation::from_sv_sources(
            vec![(EMPTY_REDUCTION_MODES, Path::new("empty_modes.sv"))],
            "Top",
        )
        .legacy_empty_reductions(legacy)
        .compile_native()
        .unwrap()
        .into_program_image();
        let image =
            celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
                .unwrap();
        check_empty_reduction_mode(
            celox::Simulation::from_sv_sources(Vec::new(), "Top")
                .build_native_from_image(image)
                .unwrap(),
            legacy,
        );
    }
}

#[test]
fn empty_reduction_compatibility_reaches_mixed_hierarchies() {
    for legacy in [false, true] {
        let sim = celox::Simulation::from_sources(
            vec![("module Unused {}", Path::new("unused.veryl"))],
            "Top",
        )
        .with_sv_sources(vec![(EMPTY_REDUCTION_MODES, Path::new("empty_modes.sv"))])
        .legacy_empty_reductions(legacy)
        .build_native()
        .unwrap();
        check_empty_reduction_mode(sim, legacy);
    }
}
