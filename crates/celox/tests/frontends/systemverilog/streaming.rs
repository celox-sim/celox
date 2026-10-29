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
