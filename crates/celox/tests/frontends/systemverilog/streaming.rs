//! Streaming concatenations (IEEE 1800-2023 11.4.14).

use super::*;

sv_backends! {
    fn streaming_reorder(sim) { @case "streaming::streaming_reorder"; }
    fn streaming_pack_alignment(sim) { @case "streaming::streaming_pack_alignment"; }
    fn streaming_unpack(sim) { @case "streaming::streaming_unpack"; }
    fn streaming_arrays(sim) { @case "streaming::streaming_arrays"; }
    fn streaming_combinational(sim) { @case "streaming::streaming_combinational"; }
    fn streaming_contexts(sim) { @case "streaming::streaming_contexts"; }
    fn streaming_operand_types(sim) { @case "streaming::streaming_operand_types"; }
    fn streaming_dynamic_pack(sim) { @case "streaming::streaming_dynamic_pack"; }
    fn streaming_dynamic_unpack(sim) { @case "streaming::streaming_dynamic_unpack"; }
    fn streaming_dynamic_alignment(sim) { @case "streaming::streaming_dynamic_alignment"; }
    fn streaming_dynamic_with(sim) { @case "streaming::streaming_dynamic_with"; }
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
