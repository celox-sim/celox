//! Clocking blocks (IEEE 1800-2023 clause 14): clocking block events, input
//! sampling and synchronous drives.
use super::*;

fn simulation(source: &str) -> celox::SimulatorBuilder<'_, celox::Simulation> {
    celox::Simulation::from_sv_sources(vec![(source, Path::new("clocking.sv"))], "Top")
}

sv_backends! {
    /// 14.10, 14.13: a process waiting for the clocking block event runs
    /// after the NBA region of the clocking event's slot. An input of
    /// `#1step` skew holds the value of the end of the previous slot, one
    /// of `#0` skew the value in the Observed region.
    fn clocking_block_events_follow_the_nba_region(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [15:0] raw, output bit [15:0] stepped, output bit [15:0] zero);
    bit clk;
    logic [3:0] d = 0, z = 0;
    always #5 clk = ~clk;
    always @(posedge clk) begin d <= d + 1; z = z + 1; end
    clocking cb @(posedge clk);
        input d;
        input #0 z;
    endclocking
    initial for (int i = 0; i < 4; i++) begin
        @(cb);
        raw[i*4 +: 4] = d;
        stepped[i*4 +: 4] = cb.d;
        zero[i*4 +: 4] = cb.z;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(40).unwrap();
        for (name, value) in [("raw", 0x4321u16), ("stepped", 0x3210), ("zero", 0x4321)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 14.4, 14.13: an input of a delay skew samples the value its signal
    /// had at the end of the time slot the skew before the clocking event,
    /// the last of the changes in that slot.
    fn delay_skews_sample_the_slot_the_skew_earlier(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [7:0] early, output bit [7:0] late);
    bit clk;
    logic [3:0] d = 0;
    always #5 clk = ~clk;
    clocking cb @(posedge clk);
        input #2 d;
    endclocking
    initial begin
        #1 d = 1;
        #2 d = 2;
        #1 d = 3;
        #9 d = 4;
        d = 5;
        #1 d = 6;
    end
    initial begin
        @(cb) early = {4'h0, cb.d};
        @(cb) late = {4'h0, cb.d};
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(20).unwrap();
        // At 5 the value at the end of 3, at 15 the value at the end of 13.
        for (name, value) in [("early", 2u32), ("late", 5)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 14.15: an event control on a clockvar waits for a change of the
    /// sampled value, which a `#0` input takes in the Observed region of the
    /// clocking event's slot. A clocking event may be a named event or have
    /// an `iff` guard, evaluated at the edge.
    fn clockvar_events_wake_in_the_clocking_event_slot(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [23:0] rose_at, output bit [23:0] guarded_at, output bit [23:0] named_at);
    bit clk;
    event tick;
    logic [3:0] d = 0;
    always #5 clk = ~clk;
    always @(negedge clk) -> tick;
    always @(posedge clk) d <= d + 1;
    clocking guarded @(posedge clk iff d != 2);
        input #0 d;
    endclocking
    clocking named @(tick);
        input d;
    endclocking
    initial for (int i = 0; i < 3; i++) begin @(posedge guarded.d[0]); rose_at[i*8 +: 8] = $time; end
    initial for (int i = 0; i < 3; i++) begin @(guarded); guarded_at[i*8 +: 8] = $time; end
    initial for (int i = 0; i < 3; i++) begin @(named.d); named_at[i*8 +: 8] = $time; end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(100).unwrap();
        // `guarded` samples 1, 2, 4, 5, 6, 7 at 5, 15, 35, 45, 55, 65.
        for (name, times) in [
            ("rose_at", 0x41_2d_05u32),
            ("guarded_at", 0x23_0f_05),
            ("named_at", 0x1e_14_0a),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(times), "{name}");
        }
    }

    /// 14.13: the samples of all the inputs are published together in the
    /// Observed region, so a process woken by a change of a `#1step` input
    /// reads the `#0` inputs of the same clocking event.
    fn samples_are_published_together(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] seen);
    bit clk;
    logic [3:0] d = 0, z = 0;
    always #5 clk = ~clk;
    always @(posedge clk) begin d <= d + 1; z <= z + 1; end
    clocking cb @(posedge clk);
        input d;
        input #0 z;
    endclocking
    initial for (int i = 0; i < 2; i++) begin @(cb.d); seen[i*4 +: 4] = cb.z; end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(20).unwrap();
        let seen = sim.signal("seen");
        assert_eq!(sim.get(seen), BigUint::from(0x21u32));
    }

    /// 24.7: a program ends when its `initial` procedures end; the process
    /// of its clocking block, which never ends, is no part of it.
    fn programs_with_clocking_blocks_end(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit clk;
    always #5 clk = ~clk;
    test t(clk, done);
endmodule
program test(input bit clk, output int done);
    clocking cb @(posedge clk);
        input clk;
    endclocking
    // The program ends at its end, after the port takes the value.
    initial begin @(cb); @(cb); done = $time; #1; end
endprogram
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(100).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(15u32));
        assert!(sim.is_finished());
    }

    /// 14.4: a skew is a delay in the time unit of the scope, rounded at
    /// its precision, also a fixed-point literal.
    fn fixed_point_skews_are_scaled_to_the_time_unit(sim) {
        @setup {
            let source = r#"`timescale 1ns/100ps
module Top(output logic [3:0] early, output logic [3:0] late);
    bit clk;
    logic [3:0] d = 0;
    always #5 clk = ~clk;
    clocking cb @(posedge clk);
        input #0.5 d;
    endclocking
    initial begin #4.4 d = 1; #0.2 d = 2; end
    initial begin @(cb) early = cb.d; @(cb) late = cb.d; end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(200).unwrap();
        for (name, value) in [("early", 1u32), ("late", 2)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 14.16, 9.6: a synchronous drive executed between clocking events is
    /// scheduler work of its own, which neither `disable fork` cancels nor
    /// `wait fork` waits for.
    fn pending_drives_are_no_child_processes(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] q, output int waited_at);
    bit clk;
    always #5 clk = ~clk;
    clocking cb @(posedge clk);
        output q;
    endclocking
    initial begin #2 cb.q[3:0] <= 4'h3; disable fork; end
    initial begin #3 cb.q[7:4] <= 4'h5; wait fork; waited_at = $time; end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(10).unwrap();
        for (name, value) in [("q", 0x53u32), ("waited_at", 3)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 14.16: a synchronous drive updates its signal in the Re-NBA region
    /// of the clocking event's slot plus the output skew; one executed
    /// between clocking events matures at the next one. Of the drives of a
    /// clockvar that mature in one Re-NBA region only the last is driven
    /// (14.16.2): `a` never rises.
    fn synchronous_drives_mature_in_the_re_nba_region(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [7:0] q5, output bit [7:0] q6, output bit [7:0] r6, output bit [7:0] r7,
           output bit [7:0] s6, output bit [7:0] s7, output bit [7:0] q16, output int rises);
    bit clk, a;
    bit [7:0] q, r, s;
    always #5 clk = ~clk;
    always @(posedge a) rises++;
    clocking cb @(posedge clk);
        output q, a;
        output #1step r;
        output #2 s;
    endclocking
    initial begin
        #3 cb.q <= 7; cb.a <= 1;
        @(cb);
        q5 = q;
        cb.q <= 9; cb.r <= 1; cb.s <= 2; cb.a <= 0;
        #1 q6 = q; r6 = r; s6 = s;
        #1 r7 = r; s7 = s;
        @(cb);
        cb.q[3:0] <= 4'hf;
        #1 q16 = q;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(30).unwrap();
        for (name, value) in [
            ("q5", 0u32),
            ("q6", 9),
            ("r6", 0),
            ("r7", 1),
            ("s6", 0),
            ("s7", 0),
            ("q16", 0x0f),
            ("rises", 0),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 14.16.2: of the drives of a bit that mature together only the last
    /// one is driven, also when another drive covers more bits: `q[0]`
    /// never rises, `q` changes once, and the other bits of the wider drive
    /// are still driven.
    fn overlapping_drives_drive_the_last_value_of_each_bit(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [3:0] q6, output int rises, output int waits, output int changes);
    bit clk;
    bit [3:0] q;
    always #5 clk = ~clk;
    // An edge of a vector is an edge of its least significant bit (9.4.2).
    always @(posedge q) rises++;
    initial forever begin @(posedge q); waits++; end
    initial forever begin @(q); changes++; end
    clocking cb @(posedge clk);
        output q;
    endclocking
    initial begin
        #3 cb.q <= 4'b0111; cb.q[1:0] <= 2'b10; cb.q[3] <= 1'b1;
        @(cb);
        #1 q6 = q;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(30).unwrap();
        for (name, value) in [("q6", 0b1110u32), ("rises", 0), ("waits", 0), ("changes", 1)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 14.16.2: of the drives that mature at one clocking event the last
    /// executed is driven, bit by bit: a drive executed at the event is
    /// later than one executed before it, whichever process runs first at
    /// the event. `r` takes its high bits from the later of two drives
    /// executed before the event and its low bits from the drive at it.
    fn drives_at_the_event_are_later_than_drives_before_it(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [7:0] q6, output bit [7:0] r6);
    bit clk;
    bit [7:0] q, r;
    always #5 clk = ~clk;
    clocking cb @(posedge clk);
        output q, r;
    endclocking
    initial begin
        #2 cb.r[7:4] <= 4'ha;
        #1 cb.q <= 1; cb.r <= 8'h11;
    end
    initial begin
        @(posedge clk);
        cb.q <= 2;
        cb.r[3:0] <= 4'h5;
        #1 q6 = q; r6 = r;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(30).unwrap();
        for (name, value) in [("q6", 2u32), ("r6", 0x15)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 14.16.2: drives executed before a clocking event mature at it in
    /// the order they were executed, whatever processes the runtime runs
    /// them in: the drive through `fast` frees its process at 3, which the
    /// second drive of `cb.q` may take over from the first.
    fn drives_before_an_event_mature_in_their_order(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [7:0] q6, output bit r4);
    bit clk, fclk, r;
    bit [7:0] q;
    always #5 clk = ~clk;
    always #1 fclk = ~fclk;
    clocking cb @(posedge clk);
        output q;
    endclocking
    clocking fast @(posedge fclk);
        output r;
    endclocking
    initial begin
        #2 fast.r <= 1;
        cb.q <= 1;
        #2 r4 = r;
        cb.q <= 2;
        #2 q6 = q;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(30).unwrap();
        for (name, value) in [("q6", 2u32), ("r4", 1)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 14.16.2: one drive statement executed twice before a clocking event
    /// matures as two drives, the later driven, and the drives after them
    /// mature at the next event (see the `clocking::drives_*` shared cases).
    fn one_drive_statement_executed_twice_before_an_event(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [7:0] q6, output bit [7:0] q16);
    bit clk;
    bit [7:0] q;
    always #5 clk = ~clk;
    clocking cb @(posedge clk);
        output q;
    endclocking
    initial begin
        for (int i = 1; i <= 2; i++) begin #1; cb.q <= i; end
        #5 cb.q <= 7;
    end
    initial begin #6 q6 = q; #10 q16 = q; end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(30).unwrap();
        for (name, value) in [("q6", 2u32), ("q16", 7)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 14.3, 14.16.1: an inout clockvar reads the value sampled at the
    /// latest clocking event, not the value its drive puts on the signal;
    /// in a program the clocking block works alike (24.3).
    fn inout_clockvars_read_their_samples(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [23:0] seen);
    bit clk;
    bit [7:0] q, d;
    always #5 clk = ~clk;
    always @(posedge clk) d <= d + 1;
    test t(clk, q, d, seen);
endmodule
program test(input bit clk, output bit [7:0] q, input bit [7:0] d, output bit [23:0] seen);
    clocking cb @(posedge clk);
        inout q;
        input d;
    endclocking
    initial for (int i = 0; i < 3; i++) begin
        @(cb);
        cb.q <= cb.d + 8'd10;
        #1 seen[i*8 +: 8] = cb.q;
        #1;
    end
endprogram
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(30).unwrap();
        let seen = sim.signal("seen");
        assert_eq!(sim.get(seen), BigUint::from(0x0b_0a_00u32));
    }

    /// 14.13: what the Observed region samples settles in its time slot,
    /// also for a clocking block of a named event, which no process waits
    /// for through a signal: `q` follows `cb.d` at once.
    fn observed_samples_of_named_event_blocks_settle(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [7:0] q, output bit [7:0] q3);
    event tick;
    bit [7:0] d;
    clocking cb @(tick);
        input #0 d;
    endclocking
    assign q = cb.d;
    initial begin
        d = 5;
        #1 -> tick;
        #2 q3 = q;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("q")), BigUint::from(5u32), "q at 2");
        sim.run_until(10).unwrap();
        assert_eq!(sim.get(sim.signal("q3")), BigUint::from(5u32), "q3");
    }
}
