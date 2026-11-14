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
fn casts_to_containers_assigned_to_fixed_size_arrays_are_unsupported() {
    // A cast to a queue type is a queue value (6.24.1, 6.24.3), which a
    // fixed-size array takes element by element (7.6). That assignment reads
    // the elements of a container variable only; a cast value is rejected,
    // not lowered as some other assignment.
    for body in [
        "typedef int queue_t [$]; int q [$]; int f [2]; initial f = queue_t'(q);",
        "typedef bit queue_t [$]; bit [7:0] a; bit f [8]; initial f = queue_t'(a);",
    ] {
        let source = format!("module Top; {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("casts.sv"))],
            "Top",
        )
        .build() else {
            panic!("a cast to a container was assigned to a fixed-size array: {body}");
        };
        let error = error
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            error.contains("assignment of an array that is not dynamically sized"),
            "{body}: {error}"
        );
    }
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

sv_backends! {
    // The initializer of `new[size](init)` may be an assignment pattern: it
    // is an array of the target's type, copied into the new array, which
    // keeps `size` elements (IEEE 1800-2023 7.5.1, 10.9.1).
    fn dynamic_array_new_takes_pattern_initializers(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(output int sizes, output logic [3:0] strb, output logic [23:0] three,
                       output logic [7:0] one);
              logic [7:0] data = 8'h5a;
              initial begin
                automatic logic [3:0] s[] = new[1]('{{4{1'b1}}});
                automatic logic [7:0] t[] = new[3]('{8'h11, data});
                automatic logic [7:0] o[] = new[1]('{8'h21, 8'h22, 8'h23});
                sizes = s.size() * 100 + t.size() * 10 + o.size();
                strb = s[0];
                three = {t[0], t[1], t[2]};
                one = o[0];
              end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("new_patterns.sv"))], "Top");
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("sizes")), BigUint::from(131u32));
        assert_eq!(sim.get(sim.signal("strb")), BigUint::from(0xfu8));
        assert_eq!(sim.get(sim.signal("three")), BigUint::from(0x115a00u32));
        assert_eq!(sim.get(sim.signal("one")), BigUint::from(0x21u8));
    }
}

sv_backends! {
    // `new[size]('{default: v})` gives each of the `size` elements the
    // value `v` (IEEE 1800-2023 7.5.1, 10.9.1: every element is covered),
    // as Caliptra's AXI BFM sizes its write strobes; growing the array
    // later fills the new elements with the type's default (Verilator: the
    // same values).
    fn dynamic_array_new_takes_default_patterns(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(output int sizes, output logic [11:0] strb, output logic [15:0] data,
                       output logic [7:0] grown);
              localparam W = 32;
              logic [7:0] fill = 8'h5a;
              initial begin
                automatic logic [3:0] a[] = new[3]('{default: {W/8{1'b1}}});
                automatic logic [7:0] b[];
                automatic logic [3:0] c[] = new[0]('{default: 4'h1});
                b = new[2]('{default: fill});
                sizes = a.size() * 100 + b.size() * 10 + c.size();
                strb = {a[0], a[1], a[2]};
                data = {b[0], b[1]};
                a = new[5](a);
                grown = {a[2], a[4]};
              end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("new_default.sv"))], "Top");
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("sizes")), BigUint::from(320u32));
        assert_eq!(sim.get(sim.signal("strb")), BigUint::from(0xfffu32));
        assert_eq!(sim.get(sim.signal("data")), BigUint::from(0x5a5au32));
        assert_eq!(sim.get(sim.signal("grown")), BigUint::from(0xf0u32));
    }
}

sv_backends! {
    fn members_of_container_elements_are_written_through_the_element(sim) {
        @setup {
            // A member write of a container element rewrites the element
            // with the member's bits replaced (IEEE 1800-2023 7.2, 7.10):
            // elements of records (string members), of integral structures
            // and of packed structures; through `$`, a run-time index, a
            // nested container and an associative array; whole, by an
            // operator, by a part-select and as a whole unpacked structure.
            // A copy of the container keeps its own elements.
            let source = r#"module Top(output int strings, output int counts,
                                    output int bits, output int others);
                typedef struct packed { logic [3:0] hi; logic [3:0] lo; } pk_t;
                typedef struct { int a; byte b; } in_t;
                typedef struct { string arg; int unsigned used; in_t s; pk_t p; bit [7:0] v; } e_t;
                e_t q[$], c[$];
                e_t m[string];
                in_t nq[$][$];
                pk_t pq[$];
                in_t plain[$];
                initial begin
                    e_t t;
                    in_t i;
                    t.arg = "one"; t.used = 5; t.s.a = 7; t.s.b = 3; t.p = 8'h12; t.v = 8'hf0;
                    q.push_back(t); q.push_back(t);
                    q[0].used++;
                    q[1].used += 10;
                    for (int k = 0; k < 2; k++) q[k].used += k;
                    q[0].arg = "uno";
                    q[1].arg = {q[0].arg, "!"};
                    q[0].s.a = -2;
                    q[1].s = '{a: 40, b: 4};
                    q[0].p.hi = 4'h9;
                    q[1].v[3:0] = 4'ha;
                    q[$].v[7] = 1'b0;
                    q[$].used *= 2;
                    c = q;
                    c[0].used = 99;
                    c[0].arg = "copy";
                    m["k"] = t; m["k"].used = 77;
                    i.a = 1; i.b = 2;
                    nq = '{'{i, i}, '{i}};
                    nq[0][1].a = 70;
                    pq.push_back(8'h34); pq[0].hi = 4'hf;
                    plain.push_back(i); plain[0].a = -5; plain[0].b[2] = 1'b1;
                    strings = (q[0].arg == "uno") + 2 * (q[1].arg == "uno!")
                        + 4 * (c[0].arg == "copy") + 8 * (t.arg == "one");
                    counts = q[0].used * 1000000 + q[1].used * 1000 + c[0].used;
                    bits = {q[0].p, q[1].v, q[0].s.b, q[1].s.b};
                    others = q[0].s.a * 1000000 + q[1].s.a * 10000 + m["k"].used * 100
                        + nq[0][1].a + nq[0][0].a + pq[0] - 8'hf4 + plain[0].a + plain[0].b;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("element_members.sv"))], "Top");
        sim.run_until(0).unwrap();
        let strings = sim.signal("strings");
        assert_eq!(sim.get(strings), BigUint::from(15u8));
        // q[0]: 5 + 1 + 0; q[1]: (5 + 10 + 1) * 2; the copy's change stays there.
        let counts = sim.signal("counts");
        assert_eq!(sim.get(counts), BigUint::from(6_032_099u32));
        let bits = sim.signal("bits");
        assert_eq!(sim.get(bits), BigUint::from(0x92_7a_03_04u32));
        // -2, 40, 77, 70 + 1, 0, -5 + 6.
        let expected: i64 = -2 * 1_000_000 + 40 * 10_000 + 77 * 100 + 70 + 1 - 5 + 6;
        let others = sim.signal("others");
        assert_eq!(sim.get(others), BigUint::from(expected as u32));
    }
}

sv_backends! {
    fn members_of_class_container_elements_are_written(sim) {
        @setup {
            // Elements of a fixed-size array property, of a queue property
            // and of a static queue property, written by member in methods
            // and through a handle; the static queue of structures with a
            // string member is UVM 1.2's `m_uvm_applied_cl_action`.
            let source = r#"module Top(output int pairs, output int records, output int statics);
                typedef struct { int a; byte b; } pair_t;
                typedef struct { string arg; int unsigned used; } rec_t;
                class C;
                    pair_t pairs[2];
                    rec_t recs[$];
                    static rec_t applied[$];
                    function void bump(int i); pairs[i].b += 2; recs[i].used++; endfunction
                    static function void apply(string arg);
                        rec_t t;
                        t.arg = arg;
                        applied.push_back(t);
                        foreach (applied[i]) applied[i].used++;
                    endfunction
                endclass
                initial begin
                    C c = new;
                    rec_t r;
                    r.arg = "r"; r.used = 10;
                    c.recs.push_back(r); c.recs.push_back(r);
                    c.pairs[0].a = 1; c.pairs[1].a++; c.pairs[0].b += 1;
                    c.bump(1);
                    c.recs[0].arg = "zero";
                    C::apply("a"); C::apply("b");
                    pairs = c.pairs[0].a * 1000 + c.pairs[0].b * 100 + c.pairs[1].a * 10 + c.pairs[1].b;
                    records = (c.recs[0].arg == "zero") * 1000 + c.recs[0].used * 10 + c.recs[1].used - 10;
                    statics = C::applied[0].used * 10 + C::applied[1].used + (C::applied[1].arg == "b") * 100;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("class_element_members.sv"))], "Top");
        sim.run_until(0).unwrap();
        let pairs = sim.signal("pairs");
        assert_eq!(sim.get(pairs), BigUint::from(1112u32));
        let records = sim.signal("records");
        assert_eq!(sim.get(records), BigUint::from(1101u32));
        let statics = sim.signal("statics");
        assert_eq!(sim.get(statics), BigUint::from(121u32));
    }
}

sv_backends! {
    fn selects_of_container_element_members_use_their_declared_ranges(sim) {
        @setup {
            // A bit-select or part-select written into a member of a
            // container element takes the positions of the member's declared
            // range (IEEE 1800-2023 7.4.6, 11.5.1), descending or ascending.
            let source = r#"module Top(output bit [15:0] value);
                typedef struct { bit [15:8] hi; string s; logic [0:7] up; } h_t;
                h_t q[$];
                initial begin
                    h_t t;
                    t.hi = 8'h00; t.up = 8'h00;
                    q.push_back(t);
                    q[0].hi[8] = 1;
                    q[0].hi[15:12] = 4'hc;
                    q[0].hi[9] ^= 1;
                    q[0].up[0] = 1;
                    q[0].up[6:7] = 2'b11;
                    value = {q[0].hi, q[0].up};
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("element_member_ranges.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(0xc383u16));
    }
}

sv_backends! {
    fn compound_member_updates_of_the_last_element_fix_its_index_first(sim) {
        @setup {
            // `q[$].m op= f()` reads and writes the element that `$` named
            // before `f()` appended to the queue: the address of the target
            // is evaluated once (IEEE 1800-2023 11.4.1), for elements of any
            // type.
            let source = r#"module Top(output int structs, output int records, output int ints);
                typedef struct { int a; byte b; } pair_t;
                typedef struct { string s; int n; } rec_t;
                pair_t q[$];
                rec_t r[$];
                int v[$];
                function automatic int grow_q();
                    pair_t p;
                    p.a = 100;
                    q.push_back(p);
                    return 5;
                endfunction
                function automatic int grow_r();
                    rec_t e;
                    e.n = 100;
                    r.push_back(e);
                    return 5;
                endfunction
                function automatic int grow_v();
                    v.push_back(100);
                    return 5;
                endfunction
                initial begin
                    pair_t p;
                    rec_t e;
                    p.a = 1;
                    e.n = 1;
                    q.push_back(p);
                    r.push_back(e);
                    v.push_back(1);
                    q[$].a += grow_q();
                    r[$].n += grow_r();
                    v[$] += grow_v();
                    structs = q[0].a * 1000 + q[1].a;
                    records = r[0].n * 1000 + r[1].n;
                    ints = v[0] * 1000 + v[1];
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("last_element_updates.sv"))], "Top");
        sim.run_until(0).unwrap();
        let structs = sim.signal("structs");
        assert_eq!(sim.get(structs), BigUint::from(6100u32));
        let records = sim.signal("records");
        assert_eq!(sim.get(records), BigUint::from(6100u32));
        let ints = sim.signal("ints");
        assert_eq!(sim.get(ints), BigUint::from(6100u32));
    }
}
