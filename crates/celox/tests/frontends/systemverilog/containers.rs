use super::*;

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
fn dollar_indices_reject_receivers_with_side_effects() {
    // `$` reads the queue's size through the receiver again; with a call in
    // the receiver, the two reads could name different queues.
    for read in ["m[next_index()][$]", "m[next_index()][$ - 1]"] {
        let source = format!(
            "module Top; int m[int][$]; int calls; int v;
                function automatic int next_index(); calls++; return calls; endfunction
                initial v = {read};
            endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("dollar-receiver.sv"))],
            "Top",
        )
        .build_cranelift()
        .err()
        .unwrap_or_else(|| panic!("`{read}` must be rejected"))
        .to_string();
        assert!(
            error.contains("`$` index of a queue read through"),
            "{read}: {error}"
        );
    }
    // An explicit size query is a read of its own: both calls are made.
    let source = r#"module Top(output int value);
        int m[int][$];
        int calls;
        int v;
        function automatic int next_index(); calls++; return calls; endfunction
        initial begin
            m[1] = '{10, 11, 12};
            m[2] = '{20, 21};
            v = m[next_index()][m[next_index()].size() - 1];
            value = calls;
        end
    endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("explicit.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(0).unwrap();
    let value = sim.signal("value");
    assert_eq!(sim.get(value), BigUint::from(2u8));
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

sv_backends! {
    fn shuffle_checkpoint_replays_the_process_stream(sim) {
        @setup {
            let source = r#"module Top(output bit [63:0] signature);
                timeunit 1ps / 1ps;
                int q[$] = {0,1,2,3,4,5,6,7};
                initial begin
                    #10; q.shuffle;
                    foreach (q[i]) signature = (signature << 8) | q[i];
                    #10; q.shuffle;
                    signature = 0;
                    foreach (q[i]) signature = (signature << 8) | q[i];
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("shuffle_checkpoint.sv"))], "Top");
        sim.run_until(15).unwrap();
        let signature = sim.signal("signature");
        let bytes = sim.checkpoint().unwrap().to_bytes().unwrap();
        // SAFETY: bytes were serialized by this simulation.
        let saved = unsafe { sim.checkpoint_from_bytes(&bytes) }.unwrap();
        sim.run_until(25).unwrap();
        let expected = sim.get(signature);
        assert_ne!(expected, BigUint::from(0u8));
        sim.restore(&saved).unwrap();
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(signature), expected);
    }
}

#[test]
fn shuffle_shares_the_seeded_process_rng() {
    let source = r#"module Top(output bit same);
        int q[$] = {0,1,2,3,4,5,6,7}, saved[$];
        process current;
        string before_state, after_state;
        initial begin
            current = process::self();
            current.srandom(123);
            before_state = current.get_randstate();
            q.shuffle(); saved = q;
            after_state = current.get_randstate();
            same = before_state != after_state;
            q = {0,1,2,3,4,5,6,7};
            current.srandom(123);
            q.shuffle;
            same = same && q == saved;
        end
    endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("shuffle_seed.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("same")), BigUint::from(1u8));
}

/// An assignment operator evaluates the address of its target once, `$`
/// included, before it reads the target, so a value that changes the
/// queue's size changes neither the element read nor the one written (IEEE
/// 1800-2023 7.10.1, 11.4.1). The standard does not say when the address is
/// evaluated relative to the value, and Verilator 5.052 evaluates `$` after
/// it, so this is not a shared case.
#[test]
fn last_element_compound_updates() {
    let source = r#"
    module Top(output bit [7:0] first, output bit [7:0] second, output bit [7:0] owned,
               output bit [7:0] pushed, output int size);
        class Box;
            bit [7:0] q[$];
        endclass
        bit [7:0] q[$];
        Box box;
        function automatic bit [3:0] grow();
            q.push_back(8'h20);
            return 4'h1;
        endfunction
        function automatic bit [3:0] grow_box();
            box.q.push_back(8'h40);
            return 4'h2;
        endfunction
        initial begin
            q = '{8'h10};
            q[$][3:0] += grow();
            first = q[0];
            second = q[1];
            size = q.size();
            box = new;
            box.q.push_back(8'h30);
            box.q[$][7:4] += grow_box();
            owned = box.q[0];
            pushed = box.q[1];
        end
    endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("containers.sv"))], "Top")
            .build()
            .unwrap();
    let signals = ["first", "second", "owned", "pushed", "size"].map(|name| sim.signal(name));
    sim.run_until(0).unwrap();
    let values = signals.map(|signal| sim.get(signal));
    assert_eq!(values, [0x11u8, 0x20, 0x50, 0x40, 2].map(BigUint::from));
}

#[test]
fn fixed_size_arrays_of_another_size_or_element_reject_dynamic_arrays() {
    // IEEE 1800-2023 7.6: a dynamic array of another number of elements
    // than its fixed-size target is a run-time error (the assignments that
    // succeed are the shared case
    // `containers::fixed_size_arrays_take_dynamic_array_elements`).
    let source = r#"module Top(output int out);
        int d []; int f [3];
        initial begin d = '{1, 2}; f = d; out = f[0]; end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("arrays.sv"))], "Top")
        .build()
        .unwrap();
    let error = sim.run_until(1).unwrap_err().to_string();
    assert!(error.contains("differ in size"), "{error}");
    // The elements must be equivalent (6.22.2), and an associative array is
    // no source of a fixed-size one.
    for (declarations, expected) in [
        ("byte d []; int f [2];", "not assignment compatible"),
        ("int d [int]; int f [2];", "not assignment compatible"),
    ] {
        let source = format!("module Top; {declarations} initial f = d; endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("arrays.sv"))],
            "Top",
        )
        .build() else {
            panic!("`{declarations}` was assigned");
        };
        let error = error
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(error.contains(expected), "{declarations}: {error}");
    }
}

#[test]
fn fixed_size_arrays_of_other_elements_check_dynamic_array_sizes() {
    // IEEE 1800-2023 7.6: a dynamic array of elements of any type, here
    // strings, of another number of elements than its fixed-size target is
    // a run-time error (the assignments that succeed are the shared case
    // `containers::fixed_size_arrays_take_dynamic_arrays_of_any_element_type`).
    let source = r#"module Top(output int out);
        string d []; string f [3];
        initial begin d = '{"a", "b"}; f = d; out = 1; end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("arrays.sv"))], "Top")
        .build()
        .unwrap();
    let error = sim.run_until(1).unwrap_err().to_string();
    assert!(error.contains("differ in size"), "{error}");
}

#[test]
fn fixed_size_arrays_of_container_typedefs_are_unsupported() {
    // A fixed-size array whose elements are queues, dynamic or associative
    // arrays has no lowering yet; declared through a typedef it is rejected
    // with that reason, as `int a [2][$]` is, rather than typed as an array
    // of integers.
    for body in [
        "typedef int queue_t [$]; queue_t f [2]; int x; initial x = $size(f);",
        "typedef int dyn_t []; dyn_t f [2];",
        "typedef int map_t [int]; map_t f [2];",
        "typedef int queue_t [$]; typedef queue_t pair_t [2]; pair_t f;",
        "typedef int queue_t [$]; initial begin queue_t f [2]; end",
    ] {
        let source = format!("module Top; {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("arrays.sv"))],
            "Top",
        )
        .build() else {
            panic!("a fixed-size array of containers was accepted: {body}");
        };
        let error = error
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            error.contains("fixed-size unpacked array of dynamically sized arrays"),
            "{body}: {error}"
        );
    }
    // Containers of containers stay supported.
    let source = "module Top(output int out);
        typedef int queue_t [$];
        queue_t m [int];
        initial begin m[3].push_back(7); out = m[3][0]; end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("arrays.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("out")), 7u32.into());
}
