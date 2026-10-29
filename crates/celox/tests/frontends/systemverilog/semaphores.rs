//! Semaphores (IEEE 1800-2023 15.3) in forms the shared `semaphores::*` cases
//! do not cover.
use super::*;

fn simulation(source: &str) -> celox::SimulatorBuilder<'_, celox::Simulation> {
    celox::Simulation::from_sv_sources(vec![(source, Path::new("semaphores.sv"))], "Top")
}

sv_backends! {
    /// The uses of `uvm_reg` and `uvm_vreg`: `std::semaphore` properties,
    /// `void'(try_get(1))` before `put(1)` to release a key that may not be
    /// held, and a negative initial count that keys must first make up for.
    fn semaphores_of_uvm_register_models(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int busy_at, output bit [2:0] q);
    class reg_model;
        local std::semaphore m_atomic;
        function new(); m_atomic = new(1); endfunction
        task do_write(int delay);
            m_atomic.get(1);
            #delay;
            m_atomic.put(1);
        endtask
        function void reset();
            void'(m_atomic.try_get(1));
            m_atomic.put(1);
        endfunction
        function bit idle();
            if (m_atomic.try_get(1) == 0) return 0;
            m_atomic.put(1);
            return 1;
        endfunction
    endclass
    reg_model r = new;
    semaphore deficit = new(-1);
    initial begin
        fork
            r.do_write(3);
            begin #1 q[0] = !r.idle(); end
        join
        r.reset();
        q[1] = r.idle();
        fork
            begin deficit.get(1); busy_at = $time; end
            begin #1 deficit.put(1); #1 deficit.put(1); end
        join
        q[2] = deficit.try_get(1) == 0;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(6).unwrap();
        let busy_at = sim.signal("busy_at");
        let q = sim.signal("q");
        assert_eq!(sim.get(busy_at), BigUint::from(5u8));
        assert_eq!(sim.get(q), BigUint::from(0b111u8));
    }
}

sv_backends! {
    /// 15.3.3: the processes waiting for keys obtain them in arrival order, so
    /// a later small request waits behind an earlier large one. Verilator and
    /// xezim unblock any process whose request the keys meet.
    fn semaphores_serve_waiting_processes_in_arrival_order(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int order, output int at);
    semaphore s = new;
    initial begin
        fork
            begin s.get(3); order = order * 10 + 1; at = $time; end
            begin #1 s.get(1); order = order * 10 + 2; end
            begin #2 s.put(1); #1 s.put(3); end
        join
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(2).unwrap();
        let order = sim.signal("order");
        let at = sim.signal("at");
        assert_eq!(sim.get(order), BigUint::from(0u8));
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(order), BigUint::from(12u8));
        assert_eq!(sim.get(at), BigUint::from(3u8));
    }

    /// A process that is killed or disabled while it waits for keys stops
    /// waiting: the processes behind it get keys, and a process that reuses
    /// its activation waits for its own request.
    fn semaphores_forget_cancelled_waiters(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int order, output int reused);
    semaphore s = new, t = new;
    process a;
    initial begin
        fork begin a = process::self(); s.get(3); order = order * 10 + 1; end join_none
        #1 fork begin s.get(1); order = order * 10 + 2; end join_none
        #1 s.put(1);
        #1 a.kill();
    end
    initial begin
        fork begin t.get(3); reused = 1; end join_none
        #1 disable fork;
        #1 fork begin t.get(1); reused = reused * 10 + 2; end join_none
        #1 t.put(1);
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(2).unwrap();
        let order = sim.signal("order");
        let reused = sim.signal("reused");
        assert_eq!(sim.get(order), BigUint::from(0u8));
        sim.run_until(5).unwrap();
        assert_eq!(sim.get(order), BigUint::from(2u8));
        assert_eq!(sim.get(reused), BigUint::from(2u8));
    }

    /// Key counts are `int` arguments: a narrower signed count is sign
    /// extended, so `new(n)` with `byte n = -1` starts with a deficit. The
    /// `int` that `try_get` returns compares as signed.
    fn semaphores_sign_extend_key_counts(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [2:0] ok);
    semaphore s;
    byte n = -1;
    initial begin
        s = new(n);
        s.put(1);
        ok[0] = s.try_get(1);
        s.put(1);
        ok[1] = s.try_get(1);
        ok[2] = s.try_get(1) > -1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(0b110u8));
    }

    /// A semaphore lives while a handle reaches it, across heap collections
    /// and many semaphores that become garbage.
    fn semaphores_survive_heap_collections(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int made, output bit ok);
    class Holder;
        semaphore lock;
        function new(); lock = new(2); endfunction
    endclass
    Holder holder;
    initial begin
        holder = new;
        repeat (100) begin
            repeat (50) begin
                semaphore garbage;
                garbage = new(made);
                made++;
            end
            #1;
        end
        ok = holder.lock.try_get(2) && !holder.lock.try_get(1);
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(101).unwrap();
        let made = sim.signal("made");
        let ok = sim.signal("ok");
        assert_eq!(sim.get(made), BigUint::from(5000u32));
        assert_eq!(sim.get(ok), BigUint::from(1u8));
    }
}

/// A negative key count is an error (15.3.2–15.3.4).
#[test]
fn negative_semaphore_key_counts_are_errors() {
    for call in [
        "s.put(-1);",
        "s.get(-2);",
        "void'(s.try_get(-1));",
        "begin byte n = -1; s.put(n); end",
    ] {
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top; semaphore s = new(1); initial begin #1 {call} end endmodule"
        );
        let mut sim = simulation(&source).build().unwrap();
        let error = format!("{:?}", sim.run_until(2).unwrap_err());
        assert!(error.contains("negative key count"), "{call}: {error}");
    }
}

/// `get` is a task and `put` returns nothing.
#[test]
fn semaphore_tasks_are_rejected_where_they_cannot_run() {
    for (body, construct) in [
        (
            "semaphore s = new; function void f(); s.get(); endfunction",
            "semaphore get",
        ),
        (
            "semaphore s = new; int x; initial x = s.put(1);",
            "semaphore put, which returns nothing, used as a value",
        ),
        (
            "semaphore s = new; initial s.peek();",
            "semaphore method `peek`",
        ),
    ] {
        let source = format!("module Top; {body} endmodule");
        let error = simulation(&source)
            .build()
            .err()
            .unwrap_or_else(|| panic!("{body} must be rejected"));
        let error = format!("{error:?}");
        assert!(error.contains(construct), "{body}: {error}");
    }
}

/// Like other objects, a semaphore that a variable holds keeps a portable
/// state file from being saved.
#[test]
fn state_files_refuse_live_semaphores() {
    let source = "`timescale 1ns/1ns\nmodule Top; semaphore s; initial begin #1 s = new(1); #1 s.get(1); end endmodule";
    let mut sim = simulation(source).build().unwrap();
    sim.run_until(0).unwrap();
    sim.save_state().unwrap();
    sim.run_until(1).unwrap();
    assert!(matches!(
        sim.save_state(),
        Err(celox::StateError::Objects(_))
    ));
}
