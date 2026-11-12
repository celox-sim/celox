//! Streaming concatenations (IEEE 1800-2023 11.4.14).

use super::*;

sv_backends! {
    // Bounded queues keep what they hold, a `with` bound is evaluated once,
    // and operands keep their types when a call is materialized (IEEE
    // 1800-2023 11.4.14).
    fn streaming_dynamic_bounds_and_operand_types(sim) {
        @setup {
            let source = r#"module Top(output bit [6:0] flags);
                byte q1[$:1];
                byte q[$];
                byte d[];
                int calls;
                byte v = -2;
                byte a, b;
                function automatic int next_index();
                    calls++;
                    return 1;
                endfunction
                function automatic byte f();
                    return 8'h11;
                endfunction
                initial begin
                    q1 = {>>{24'h123456}};
                    flags[0] = q1.size() == 2 && q1[1] == 8'h34;
                    q = '{8'h01, 8'h02, 8'h03, 8'h04};
                    d = {>>{q with [next_index():3]}};
                    flags[1] = calls == 1 && d.size() == 3 && d[0] == 8'h02;
                    q.delete();
                    d = {>>{q, v >>> 1, f()}};
                    flags[2] = d.size() == 2 && d[0] == 8'hff && d[1] == 8'h11;
                    {>>{d with [next_index():2]}} = 16'habcd;
                    flags[3] = calls == 2 && d.size() == 3 && d[0] == 8'hff
                        && d[1] == 8'hab && d[2] == 8'hcd;
                    q = '{8'h01, 8'h02};
                    d = {>>{q with [(-2 >>> 1) + 2]}};
                    flags[4] = d.size() == 1 && d[0] == 8'h02;
                    q = '{8'h11, 8'h22, 8'h33};
                    {<< 8 {a, b}} = q;
                    flags[5] = a == 8'h22 && b == 8'h11;
                    d = {>>{q with [-1 +: 2]}};
                    flags[6] = d.size() == 2 && d[0] == 8'h00 && d[1] == 8'h11;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("bounds.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("flags")), 127u8.into());
    }
}

#[test]
fn integral_casts_to_string_zero_fill_whole_characters() {
    // IEEE 1800-2023 6.16: an integral value converted to a string is
    // zero-filled on the left to a whole number of characters, unlike the
    // right-padded bytes of a stream.
    let source = r#"module Top(output int twelve_len, output int twelve_first,
                          output int twelve_last, output int nine_len,
                          output int nine_first, output int nine_last,
                          output int one_len, output int one_first);
        string a, b, c;
        initial begin
            a = string'(12'hA41);
            b = string'(9'h141);
            c = string'(1'b1);
            twelve_len = a.len();
            twelve_first = a[0];
            twelve_last = a[1];
            nine_len = b.len();
            nine_first = b[0];
            nine_last = b[1];
            one_len = c.len();
            one_first = c[0];
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("twelve_len")), 2u32.into());
    assert_eq!(sim.get(sim.signal("twelve_first")), 0x0Au32.into());
    assert_eq!(sim.get(sim.signal("twelve_last")), 0x41u32.into());
    assert_eq!(sim.get(sim.signal("nine_len")), 2u32.into());
    assert_eq!(sim.get(sim.signal("nine_first")), 0x01u32.into());
    assert_eq!(sim.get(sim.signal("nine_last")), 0x41u32.into());
    assert_eq!(sim.get(sim.signal("one_len")), 1u32.into());
    assert_eq!(sim.get(sim.signal("one_first")), 0x01u32.into());
}

#[test]
fn casts_of_dynamic_arrays_to_fixed_size_ones_check_the_size() {
    // IEEE 1800-2023 6.24.2, 7.6: `$cast` assigns a dynamically sized array
    // to a fixed-size one only when it has as many elements; to a subarray
    // and with elements wider than 64 bits too (the shared case
    // `containers::casts_of_dynamic_arrays_to_fixed_size_ones_check_the_size`
    // covers whole variables).
    let source = r#"module Top(output int nested, output int other, output int index,
                          output int snapshot, output int wide);
        int d []; int q [$]; int g [3][2];
        // An index that resizes the source after the source is read.
        function int shrink(); d = new[1]; return 2; endfunction
        logic [129:0] w []; logic [129:0] h [2];
        int k;
        initial begin
            d = new[3];
            q.push_back(11); q.push_back(13);
            k = 1;
            nested = $cast(g[k], q);
            if ($cast(g[k + 1], q)) nested++;
            other = $cast(g[0], d);
            index = g[0][0] + g[1][0] + g[1][1] + g[2][1];
            d = new[2]; d[0] = 21; d[1] = 23;
            snapshot = $cast(g[shrink()], d) && g[2][0] == 21 && g[2][1] == 23 && d.size() == 1;
            w = new[2]; w[0] = {2'b10, 128'h1}; w[1] = 130'h3;
            wide = $cast(h, w) && h[0] == {2'b10, 128'h1} && h[1] == 130'h3;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("nested")), 2u32.into());
    assert_eq!(sim.get(sim.signal("other")), 0u32.into());
    assert_eq!(sim.get(sim.signal("index")), 37u32.into());
    assert_eq!(sim.get(sim.signal("snapshot")), 1u32.into());
    assert_eq!(sim.get(sim.signal("wide")), 1u32.into());
    // As a task, a size that does not match is a run-time error, and one
    // that matches assigns.
    let source = r#"module Top(output int value);
        int d []; int f [2];
        initial begin
            d = new[2]; d[1] = 9;
            $cast(f, d);
            value = f[1];
            d = new[1];
            $cast(f, d);
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("value")), 9u32.into());
    let events = sim.drain_runtime_events();
    let errors = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                celox::RuntimeEvent::Report(report)
                    if report.severity == celox::Severity::Error
                        && String::from_utf8_lossy(&report.message).contains("not cast compatible")
            )
        })
        .count();
    assert_eq!(errors, 1, "{events:?}");
}

#[test]
fn streams_cast_only_to_bit_stream_types() {
    // IEEE 1800-2023 11.4.14: a streaming concatenation is the operand only
    // of a cast to a bit-stream type (6.24.3), which `real` and `shortreal`
    // are not; its integral value converts after a cast to one.
    for cast in [
        "real'({>>{b}})",
        "shortreal'({<<{b}})",
        "real'({>>{b, 8'd1}})",
    ] {
        let source =
            format!("module Top; logic [7:0] b = 8'd3; real r; initial r = {cast}; endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("casts.sv"))],
            "Top",
        )
        .build() else {
            panic!("a stream was cast to a type that is not a bit-stream type: {cast}");
        };
        let error = error.to_string();
        assert!(
            error.contains("which is not a bit-stream type"),
            "{cast}: {error}"
        );
    }
    let source = r#"module Top(output int whole);
        typedef logic [15:0] half_t;
        logic [7:0] b = 8'd3;
        real r;
        initial begin
            r = real'(half_t'({>>{b, 8'd1}}));
            whole = int'(r);
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("whole")), 769u32.into());
}

#[test]
fn bit_stream_casts_of_strings_and_containers_pack_index_zero_first() {
    // IEEE 1800-2023 6.24.3: a string is a dynamic array of bytes, and a
    // cast between bit-stream types packs the source with index 0 in the
    // most significant bits, then fills the target from them.
    let source = r#"module Top(output int from_string, output int from_queue,
                          output int queue_size, output int queue_last,
                          output int from_array, output int string_size);
        typedef byte bytes_t[$];
        typedef byte pair_t [2];
        string s = "AB";
        byte q[$];
        bytes_t r;
        pair_t a;
        string t;
        initial begin
            q = {8'h01, 8'h02};
            a = '{8'h03, 8'h04};
            from_string = shortint'(s);
            from_queue = shortint'(q);
            r = bytes_t'(16'h0506);
            queue_size = r.size();
            queue_last = r[1];
            from_array = shortint'(a);
            t = string'(r);
            string_size = t.len();
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("from_string")), 0x4142u32.into());
    assert_eq!(sim.get(sim.signal("from_queue")), 0x0102u32.into());
    assert_eq!(sim.get(sim.signal("queue_size")), 2u32.into());
    assert_eq!(sim.get(sim.signal("queue_last")), 6u32.into());
    assert_eq!(sim.get(sim.signal("from_array")), 0x0304u32.into());
    assert_eq!(sim.get(sim.signal("string_size")), 2u32.into());
    // A fixed-size target takes only a source of its size (6.24.3).
    let source = r#"module Top(output int out);
        string s = "AB";
        initial out = int'(s);
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    let error = sim.run_until(1).unwrap_err().to_string();
    assert!(error.contains("differ in size"), "{error}");
    // So does a fixed-size unpacked array: a 1-byte string does not fill a
    // 2-byte array, which zero padding would hide.
    let source = r#"module Top(output int out);
        typedef byte pair_t [2];
        string s = "A";
        pair_t a;
        initial begin a = pair_t'(s); out = a[1]; end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    let error = sim.run_until(1).unwrap_err().to_string();
    assert!(error.contains("differ in size"), "{error}");
    // Fixed sizes that differ are rejected before the simulation runs.
    for cast in ["pair_t'(i)", "int'(a)"] {
        let source = format!(
            "module Top(output int out); typedef byte pair_t [2]; int i; pair_t a;
                 initial begin a = '{{8'd1, 8'd2}}; out = $bits({cast}); end endmodule"
        );
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("casts.sv"))],
            "Top",
        )
        .build() else {
            panic!("`{cast}` between 16 and 32 bits was accepted");
        };
        let error = error.to_string();
        assert!(error.contains("a bit-stream cast of"), "{error}");
    }
    let source = r#"module Top(output int hi, output int lo);
        typedef byte pair_t [2];
        string s = "AB";
        pair_t a, b;
        initial begin a = pair_t'(16'h0102); b = pair_t'(s); hi = a[0]; lo = b[1]; end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("hi")), 1u32.into());
    assert_eq!(sim.get(sim.signal("lo")), 0x42u32.into());
}

#[test]
fn dynamic_casts_assign_cast_compatible_values_of_any_singular_type() {
    // IEEE 1800-2023 6.24.2: `$cast` assigns a source that is cast
    // compatible with its singular destination and returns 1; otherwise it
    // leaves the destination unchanged and returns 0, or, as a task, reports
    // a run-time error.
    let source = r#"module Top(output int rounded, output int to_int, output int text,
                          output int length, output int from_string, output int kept,
                          output int from_handle);
        class C; endclass
        initial begin
            int i;
            real r;
            string t, s;
            C c;
            r = 2.5;
            to_int = $cast(i, r);
            rounded = i;
            text = $cast(t, 16'h4142);
            length = t.len();
            s = "x";
            r = 7.0;
            from_string = $cast(r, s);
            kept = int'(r);
            c = new;
            from_handle = $cast(i, c);
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("to_int")), 1u32.into());
    assert_eq!(sim.get(sim.signal("rounded")), 3u32.into());
    assert_eq!(sim.get(sim.signal("text")), 1u32.into());
    assert_eq!(sim.get(sim.signal("length")), 2u32.into());
    assert_eq!(sim.get(sim.signal("from_string")), 0u32.into());
    assert_eq!(sim.get(sim.signal("kept")), 7u32.into());
    assert_eq!(sim.get(sim.signal("from_handle")), 0u32.into());
    // A destination is any place: a class property or a container element
    // takes the bits in its own declared type.
    let source = r#"module Top(output int text, output int element);
        class H; string s; endclass
        initial begin
            H h;
            string names[$];
            h = new;
            names = {"", ""};
            text = $cast(h.s, 16'h4142) ? h.s.len() : -1;
            element = $cast(names[1], 8'h43) ? names[1].len() : -1;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("text")), 2u32.into());
    assert_eq!(sim.get(sim.signal("element")), 1u32.into());
    // Arguments that are not singular must be assignment compatible: a bit
    // stream is no valid source of an unpacked array, nor one of them of an
    // integral destination; the destination keeps its value.
    let source = r#"module Top(output int pair, output int last, output int queue,
                          output int queue_size, output int names_cast, output int from_array);
        initial begin
            byte b [2];
            byte q [$];
            string s;
            string names [2];
            int i;
            b = '{8'h05, 8'h06};
            pair = $cast(b, 16'h0102);
            last = b[1];
            s = "AB";
            queue = $cast(q, s);
            queue_size = q.size();
            names_cast = $cast(names, 16'h4142);
            i = 9;
            from_array = $cast(i, b) ? -1 : i;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("pair")), 0u32.into());
    assert_eq!(sim.get(sim.signal("last")), 6u32.into());
    assert_eq!(sim.get(sim.signal("queue")), 0u32.into());
    assert_eq!(sim.get(sim.signal("queue_size")), 0u32.into());
    assert_eq!(sim.get(sim.signal("names_cast")), 0u32.into());
    assert_eq!(sim.get(sim.signal("from_array")), 9u32.into());
    // Assignment compatible handles are assigned: a chandle of a chandle, a
    // class handle of a derived class's. A chandle is no value of an int,
    // and a source that is not assigned is still evaluated, for its calls.
    let source = r#"module Top(output int chandles, output int upcast, output int from_chandle,
                          output int kept, output int to_queue, output int to_enum,
                          output int from_array, output int from_handle,
                          output int from_property, output int from_member, output int calls);
        typedef enum {A, B} E;
        class C; endclass
        class D extends C; endclass
        int n;
        function int f(); n++; return 7; endfunction
        function D g(); n++; g = new; endfunction
        typedef int pair_t [2];
        function pair_t pair(); n++; return '{1, 2}; endfunction
        function chandle none(); n++; return null; endfunction
        class Owner; chandle handle; int arr [$]; endclass
        Owner o = new;
        function Owner owner(); n++; return o; endfunction
        initial begin
            chandle x, y;
            C c;
            D d;
            int i;
            byte q [$];
            E e;
            chandles = $cast(x, y);
            d = new;
            upcast = $cast(c, d) && c == d;
            i = 3;
            from_chandle = $cast(i, x);
            kept = i;
            to_queue = $cast(q, f());
            e = B;
            to_enum = $cast(e, g()) || e != B;
            from_array = $cast(i, pair()) || i != 3;
            from_handle = $cast(i, none()) || i != 3;
            from_property = $cast(i, owner().handle) || i != 3;
            from_member = $cast(i, owner().arr) || i != 3;
            calls = n;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("chandles")), 1u32.into());
    assert_eq!(sim.get(sim.signal("upcast")), 1u32.into());
    assert_eq!(sim.get(sim.signal("from_chandle")), 0u32.into());
    assert_eq!(sim.get(sim.signal("kept")), 3u32.into());
    assert_eq!(sim.get(sim.signal("to_queue")), 0u32.into());
    assert_eq!(sim.get(sim.signal("to_enum")), 0u32.into());
    assert_eq!(sim.get(sim.signal("from_array")), 0u32.into());
    assert_eq!(sim.get(sim.signal("from_handle")), 0u32.into());
    assert_eq!(sim.get(sim.signal("from_property")), 0u32.into());
    assert_eq!(sim.get(sim.signal("from_member")), 0u32.into());
    assert_eq!(sim.get(sim.signal("calls")), 6u32.into());
    // As a task, a cast that is not valid is a run-time error.
    let source = r#"module Top;
        initial begin real r; string s; s = "x"; $cast(r, s); end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    let events = sim.drain_runtime_events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            celox::RuntimeEvent::Report(report)
                if report.severity == celox::Severity::Error
                    && String::from_utf8_lossy(&report.message).contains("not cast compatible")
        )),
        "{events:?}"
    );
}

#[test]
fn size_and_signing_casts_take_only_integral_operands() {
    // IEEE 1800-2023 6.24.1: the expression inside the cast shall be an
    // integral value when changing the size or signing.
    for (cast, operand) in [
        ("16'(s)", "string"),
        ("signed'(s)", "string"),
        ("16'(r)", "real"),
        ("unsigned'(r)", "real"),
    ] {
        let source = format!(
            "module Top(output int out); string s = \"AB\"; real r = 2.5;
                 initial out = {cast}; endmodule"
        );
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("casts.sv"))],
            "Top",
        )
        .build() else {
            panic!("`{cast}` of a {operand} was accepted");
        };
        // The report wraps its lines inside a frame.
        let error = error
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            error.contains("not an integral value") && error.contains(operand),
            "{error}"
        );
    }
    let source = "module Top(output int sized, output int signing, output int named);
        typedef enum logic [3:0] {A = 4'd9} e_t;
        typedef struct packed { bit [3:0] hi, lo; } p_t;
        e_t e = A; p_t p = 8'hF5; int i = 300;
        initial begin
            sized = 8'(i);
            signing = signed'(e);
            named = 4'(p);
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("sized")), 44u32.into());
    assert_eq!(sim.get(sim.signal("signing")), 0xFFFF_FFF9u32.into());
    assert_eq!(sim.get(sim.signal("named")), 5u32.into());
}

#[test]
fn bit_stream_casts_lay_out_both_sides_by_their_types() {
    // IEEE 1800-2023 6.24.3: a structure streams its members in order and an
    // array its elements, a string its bytes. A layout the lowering cannot
    // give yet is reported, not packed as another shape.
    let source = r#"module Top(output int from_struct);
        typedef struct { bit [7:0] hi; bit [7:0] lo; } pair_t;
        pair_t p;
        initial begin p.hi = 8'h12; p.lo = 8'h34; from_struct = shortint'({>>{p}}); end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("from_struct")), 0x1234u32.into());
    let rejected = |declarations: &str, cast: &str| {
        let source = format!(
            "module Top(output int out); {declarations} initial begin {cast} end endmodule"
        );
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("casts.sv"))],
            "Top",
        )
        .build() else {
            panic!("`{cast}` was packed without a layout");
        };
        error
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let names = "typedef string names_t [2]; names_t names;";
    for cast in ["out = shortint'(names);", "names = names_t'(16'h4142);"] {
        let error = rejected(names, cast);
        assert!(
            error.contains("fixed-size array of string"),
            "{cast}: {error}"
        );
    }
    // A structure target, or one holding a string array, is not supported
    // where it is declared or assigned, before any stream is laid out.
    let pair = "typedef struct { bit [7:0] hi; bit [7:0] lo; } pair_t; pair_t p;";
    let holder = "typedef string names_t [2]; typedef struct { int i; names_t names; } holder_t;
                  holder_t holder;";
    for (declarations, cast) in [
        (pair, "p = pair_t'(16'h1234);"),
        (
            "typedef struct { bit [7:0] hi; bit [7:0] lo; } pair_t;
             function automatic int f(pair_t x); return x.lo; endfunction",
            "out = f(pair_t'(16'h1234));",
        ),
        (holder, "out = $bits(shortint'(holder));"),
        (holder, "holder = holder_t'(32'h41424344);"),
    ] {
        let error = rejected(declarations, cast);
        assert!(
            error.contains("Unsupported SystemVerilog construct"),
            "{cast}: {error}"
        );
    }
}
