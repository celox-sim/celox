//! Named events (IEEE 1800-2023 15.5) in forms the external simulators do not
//! run alike; see the `events::*` shared cases for the portable ones.
use super::*;

fn simulation(source: &str) -> celox::SimulatorBuilder<'_, celox::Simulation> {
    celox::Simulation::from_sv_sources(vec![(source, Path::new("events.sv"))], "Top")
}

sv_backends! {
    /// 15.5.5: assignment merges events, a process blocked on a variable
    /// before the assignment stays blocked, and `null` breaks the link;
    /// triggering it has no effect. Events compare as handles, and
    /// `$typename` names their type.
    fn events_merge_by_assignment_and_compare_as_handles(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int seen, output bit [7:0] checks);
    event a, b, c, E1, E2;
    task automatic count(event ev, int weight);
        forever begin @ev; seen = seen + weight; end
    endtask
    initial begin
        checks[0] = a != b && a && c != null;
        a = b;
        checks[1] = a == b && a === b;
        fork
            count(a, 1);
            count(b, 10);
            count(c, 100);
            T1: forever @ E2 seen = seen + 1000;
        join_none
        #1 -> a;             // also triggers b: 11
        #1 -> b;             // 22
        a = c;
        b = a;
        // The tasks wait for copies of the old handle: only the waiter of c
        // wakes (122).
        #1 -> a;
        E2 = E1;
        #1 -> E2;            // T1 waits for the old E2: 122
        c = null;
        checks[2] = !c && c == null && c !== a;
        #1 -> c;             // no effect
        checks[3] = !c.triggered;
        checks[4] = $typename(c) == "event";
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(5).unwrap();
        let seen = sim.signal("seen");
        let checks = sim.signal("checks");
        assert_eq!(sim.get(seen), BigUint::from(122u32));
        assert_eq!(sim.get(checks), BigUint::from(0b1_1111u8));
    }

    /// 15.5.1: `->>` triggers in the NBA region, after the nonblocking
    /// assignments issued before it; a process `->` unblocks runs in the
    /// active region before them.
    fn events_nonblocking_trigger_runs_in_the_nba_region(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int q, output int r);
    event e, f;
    int x;
    initial begin @e; q = x; end
    initial begin @f; r = x; end
    initial begin #1; x <= 5; ->> e; -> f; end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        let q = sim.signal("q");
        let r = sim.signal("r");
        assert_eq!(sim.get(q), BigUint::from(5u8));
        assert_eq!(sim.get(r), BigUint::from(0u8));
    }

    /// The example of 15.5.3: an alias and a task argument name the event
    /// they were assigned from.
    fn events_aliases_and_task_arguments_name_one_event(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done_at, output int blast_at);
    event done, blast;
    event done_too = done;
    task trigger(event ev);
        -> ev;
    endtask
    initial begin
        fork
            @ done_too;
            #1 trigger(done);
        join
        done_at = $time;
        fork
            -> blast;
            wait (blast.triggered);
        join
        blast_at = $time + 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(3).unwrap();
        let done_at = sim.signal("done_at");
        let blast_at = sim.signal("blast_at");
        assert_eq!(sim.get(done_at), BigUint::from(1u8));
        assert_eq!(sim.get(blast_at), BigUint::from(2u8));
    }

    /// The uses of `uvm_event`: a protected event property, `@` and
    /// `triggered` in tasks, a trigger in a function, and a reset that
    /// replaces the event with a new local one.
    fn events_of_a_uvm_event_like_class(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int t1, output int t2, output int t3, output int waiters, output bit on);
    class my_event;
        protected event m_event;
        protected int num_waiters;
        protected bit on;
        virtual task wait_trigger();
            num_waiters++;
            @m_event;
            num_waiters--;
        endtask
        virtual task wait_ptrigger();
            if (m_event.triggered) return;
            num_waiters++;
            @m_event;
            num_waiters--;
        endtask
        virtual function void trigger();
            -> m_event;
            on = 1;
        endfunction
        virtual function void reset(bit wakeup = 0);
            event e;
            if (wakeup) -> m_event;
            else if (num_waiters == 0) m_event = e;
            on = 0;
        endfunction
        function int get_num_waiters(); return num_waiters; endfunction
        function bit is_on(); return on; endfunction
    endclass
    my_event ev = new;
    initial begin
        fork
            begin ev.wait_trigger(); t1 = $time; end
            begin #2 ev.trigger(); end
            // Whether it runs before or after the trigger.
            begin #2 ev.wait_ptrigger(); t2 = $time; end
        join
        on = ev.is_on();
        ev.reset();
        fork
            begin ev.wait_trigger(); t3 = $time; end
            begin #1 ev.reset(1); end
        join
        waiters = ev.get_num_waiters();
        on = on && !ev.is_on();
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(4).unwrap();
        for (name, value) in [("t1", 2u8), ("t2", 2), ("t3", 3), ("waiters", 0), ("on", 1)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// The uses of `uvm_objection`: events of objects held in an associative
    /// array, triggered and waited for through the array, and a static event.
    fn events_of_objects_in_an_associative_array(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int log);
    class objection_events;
        int waiters;
        event raised;
        event dropped;
        static event all_dropped;
    endclass
    objection_events m_events[string];
    task automatic wait_for(string name);
        if (!m_events.exists(name)) m_events[name] = new;
        m_events[name].waiters++;
        @(m_events[name].raised or m_events[name].dropped);
        m_events[name].waiters--;
        log = log * 10 + m_events.size();
    endtask
    initial begin
        fork
            wait_for("a");
            wait_for("b");
            begin
                #1 -> m_events["a"].raised;
                #1 -> m_events["b"].dropped;
                #1 -> objection_events::all_dropped;
            end
            begin @(objection_events::all_dropped); log = log * 10 + 9; end
        join
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(3).unwrap();
        let log = sim.signal("log");
        assert_eq!(sim.get(log), BigUint::from(229u32));
    }
}

sv_backends! {
    /// An event declared through a typedef is an event too; an event control
    /// on a conditional waits for the event it selects; and a process waiting
    /// for a null event stays blocked when it is suspended and resumed
    /// (15.5.5.2).
    fn events_through_typedefs_conditionals_and_null_waits(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [2:0] q);
    typedef event event_t;
    event_t a, b, none;
    bit select = 1;
    process waiter;
    initial begin @a; q[0] = 1; end
    initial begin @(select ? b : a); @(select ? b : null); q[1] = 1; end
    initial begin
        none = null;
        waiter = process::self();
        @none;
        q[2] = 1;
    end
    initial begin
        #1 -> a;
        #1 -> b;
        #1 -> b;
        waiter.suspend();
        #1 waiter.resume();
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        let q = sim.signal("q");
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(q), BigUint::from(0b001u8));
        sim.run_until(5).unwrap();
        assert_eq!(sim.get(q), BigUint::from(0b011u8));
    }

    /// A wildcard `always` and an `always_ff` that trigger an event, also
    /// through a function, run as processes; they do not block.
    fn events_triggered_by_always_procedures(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int seen);
    event e, f, g;
    bit c, d, clk;
    function void fire(); -> g; endfunction
    always @* if (c) -> e;
    always @* if (d) fire();
    always_ff @(posedge clk) -> f;
    initial forever begin @e; seen = seen + 1; end
    initial forever begin @f; seen = seen + 10; end
    initial forever begin @g; seen = seen + 100; end
    initial begin #1 c = 1; #1 d = 1; #1 clk = 1; end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(3).unwrap();
        let seen = sim.signal("seen");
        assert_eq!(sim.get(seen), BigUint::from(111u32));
    }

    /// 15.5.3: `triggered` is a one-bit value in a self-determined context.
    fn events_triggered_is_one_bit_wide(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output longint w);
    event e;
    initial begin #1 -> e; w = {1'b1, e.triggered}; end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        let w = sim.signal("w");
        assert_eq!(sim.get(w), BigUint::from(3u8));
    }

    /// A process suspended while it waits for an event misses a trigger
    /// during its suspension: after `resume` it waits for a later one.
    fn events_missed_while_suspended(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int q, output int r);
    event e;
    process p;
    initial begin
        fork begin p = process::self(); @e; q = 1; end join_none
        #1 p.suspend();
        #1 -> e;
        #1 p.resume();
        #1 r = q + 10;
        -> e;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(5).unwrap();
        let q = sim.signal("q");
        let r = sim.signal("r");
        assert_eq!(sim.get(r), BigUint::from(10u8));
        assert_eq!(sim.get(q), BigUint::from(1u8));
    }
}

/// Forms of named events that Celox does not implement are rejected.
#[test]
fn unsupported_named_event_forms_are_rejected() {
    for (body, construct) in [
        (
            "event e; initial ->> #1 e;",
            "delayed nonblocking event trigger",
        ),
        (
            "event e; logic c; initial @(e or posedge c);",
            "event control mixing named events with other events",
        ),
        ("event e[2];", "array of named events"),
        ("event e[]; initial e = new[2];", "array of named events"),
        ("event e[$];", "array of named events"),
        (
            "event e; function event f(); return e; endfunction",
            "function returning an event",
        ),
        (
            "event e; bit b; initial wait (e.triggered || b);",
            "`triggered` in a wait condition with other operands",
        ),
        (
            "event e; always_comb -> e;",
            "event trigger or process method in an always_comb body",
        ),
        ("typedef struct { event e; } S; S s;", "unpacked struct"),
        (
            "event e; function bit ready(); return e.triggered; endfunction initial wait (ready());",
            "`triggered` read by a function called in a wait condition",
        ),
        (
            "int x; initial -> x;",
            "event trigger of a value that is not a named event",
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
    let error = simulation("module Top(output event e); endmodule")
        .build()
        .err()
        .unwrap_or_else(|| panic!("an event port must be rejected"));
    assert!(format!("{error:?}").contains("event port"), "{error:?}");
}

/// Named events stay out of state files: a save is refused while a process
/// waits for an event or a trigger is still visible to `triggered`.
#[test]
fn state_files_refuse_pending_named_events() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
    event e;
    initial begin @e; q = 1; end
    initial #2 -> e;
endmodule
"#;
    let mut sim = simulation(source).build().unwrap();
    sim.run_until(1).unwrap();
    assert!(matches!(
        sim.save_state(),
        Err(celox::StateError::Objects(_))
    ));
    sim.run_until(2).unwrap();
    assert!(matches!(
        sim.save_state(),
        Err(celox::StateError::Objects(_))
    ));
    sim.run_until(3).unwrap();
    assert_eq!(sim.get(sim.signal("q")), BigUint::from(1u8));
    sim.save_state().unwrap();
}
