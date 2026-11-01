//! Mailboxes (IEEE 1800-2023 15.4) in forms the shared `mailboxes::*` cases
//! do not cover.
use super::*;

fn simulation(source: &str) -> celox::SimulatorBuilder<'_, celox::Simulation> {
    celox::Simulation::from_sv_sources(vec![(source, Path::new("mailboxes.sv"))], "Top")
}

sv_backends! {
    /// 15.4.5, 15.4.7: a message unblocks every waiting peek and the first
    /// waiting get; waiting gets take messages in arrival order. Four-state
    /// messages keep their unknown bits.
    fn mailboxes_wake_peeks_and_gets_in_arrival_order(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int order, output logic [7:0] seen, output logic [7:0] last);
    mailbox #(logic [7:0]) m = new;
    initial begin
        fork
            begin logic [7:0] v; m.peek(v); seen = v; order = order * 10 + 1; end
            begin logic [7:0] v; m.peek(v); order = order * 10 + 2; end
            begin logic [7:0] v; #1 m.get(v); order = order * 10 + 3; end
            begin logic [7:0] v; #2 m.get(v); last = v; order = order * 10 + 4; end
            begin #3 m.put(8'b1x0z_0101); #1 m.put(8'h0f); end
        join
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(3).unwrap();
        let order = sim.signal("order");
        let seen = sim.signal("seen");
        let last = sim.signal("last");
        assert_eq!(sim.get(order), BigUint::from(123u32));
        assert_eq!(
            sim.get_four_state(seen),
            (BigUint::from(0b1100_0101u8), BigUint::from(0b0101_0000u8))
        );
        sim.run_until(4).unwrap();
        assert_eq!(sim.get(order), BigUint::from(1234u32));
        assert_eq!(sim.get(last), BigUint::from(0x0fu8));
    }

    /// A message keeps its object alive while only the mailbox holds it,
    /// across heap collections, as `uvm_tlm_fifo` holds transactions.
    fn mailboxes_keep_their_messages_alive(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int id, output int made);
    class Item;
        int id;
        int payload[$];
        function new(int i); id = i; endfunction
    endclass
    mailbox #(Item) m = new;
    initial begin
        Item item;
        item = new(42);
        m.put(item);
        item = null;
        repeat (200) begin
            repeat (100) begin
                Item garbage;
                garbage = new(made);
                garbage.payload.push_back(made);
                made++;
            end
            #1;
        end
        m.get(item);
        id = item.id;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(201).unwrap();
        let id = sim.signal("id");
        let made = sim.signal("made");
        assert_eq!(sim.get(made), BigUint::from(20_000u32));
        assert_eq!(sim.get(id), BigUint::from(42u8));
    }

    /// A mailbox lives while a handle reaches it, across heap collections
    /// and many mailboxes that become garbage.
    fn mailboxes_survive_heap_collections(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int made, output int got);
    class Holder;
        mailbox #(int) box;
        function new(); box = new(1); endfunction
    endclass
    Holder holder;
    initial begin
        holder = new;
        holder.box.put(7);
        repeat (100) begin
            repeat (50) begin
                mailbox #(int) garbage;
                garbage = new;
                garbage.put(made);
                made++;
            end
            #1;
        end
        holder.box.get(got);
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(101).unwrap();
        let made = sim.signal("made");
        let got = sim.signal("got");
        assert_eq!(sim.get(made), BigUint::from(5000u32));
        assert_eq!(sim.get(got), BigUint::from(7u8));
    }

    /// `num` and the `try_*` methods return `int`, which compares as signed.
    fn mailbox_results_are_signed(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [3:0] ok);
    mailbox #(int) box = new(1);
    int m;
    initial begin
        ok[0] = box.num() > -1;
        ok[1] = box.try_put(1) > -1;
        ok[2] = box.try_peek(m) > -1;
        ok[3] = box.try_get(m) > -1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(0b1111u8));
    }

    /// Waiting processes are served in arrival order whatever order they run
    /// in: consecutive puts go to the gets in the order they began to wait,
    /// a waiting peek sees the message that a waiting get takes, and room
    /// goes to the puts in the order they began to wait (15.4.3, 15.4.5,
    /// 15.4.7).
    fn mailboxes_serve_waiting_processes_in_arrival_order(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int late, output int early, output int peeked, output int got,
           output int order);
    mailbox #(int) m = new, n = new, b = new(1);
    initial begin
        fork
            begin #1 m.get(late); end
            begin m.get(early); end
            begin #2 m.put(1); m.put(2); end
        join
        fork
            begin n.get(got); end
            begin n.peek(peeked); end
            begin #1 n.put(5); end
        join
        b.put(0);
        fork
            begin #1 b.put(2); end
            begin b.put(1); end
            begin
                int v;
                #2 repeat (3) begin b.get(v); order = order * 10 + v; end
            end
        join
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(6).unwrap();
        assert_eq!(sim.get(sim.signal("early")), BigUint::from(1u8));
        assert_eq!(sim.get(sim.signal("late")), BigUint::from(2u8));
        assert_eq!(sim.get(sim.signal("peeked")), BigUint::from(5u8));
        assert_eq!(sim.get(sim.signal("got")), BigUint::from(5u8));
        assert_eq!(sim.get(sim.signal("order")), BigUint::from(12u8));
    }

    /// A process that ends after a message was handed to it takes the
    /// message along, as one that ends after a semaphore gave it keys keeps
    /// them; one that ends while its message waits for room never puts it.
    /// Both leave the mailbox working.
    fn mailboxes_forget_ended_waiters(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int x, output int y, output int z, output bit [1:0] ok);
    mailbox #(int) m = new, b = new(1);
    process p, q;
    initial begin
        fork begin p = process::self(); m.get(x); end join_none
        #1 m.put(1);
        p.kill();
        m.put(2);
        ok[0] = m.num() == 1;
        m.get(y);
        b.put(3);
        fork begin q = process::self(); b.put(9); end join_none
        #1 q.kill();
        b.get(z);
        ok[1] = b.num() == 0;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(sim.signal("x")), BigUint::from(0u8));
        assert_eq!(sim.get(sim.signal("y")), BigUint::from(2u8));
        assert_eq!(sim.get(sim.signal("z")), BigUint::from(3u8));
        assert_eq!(sim.get(sim.signal("ok")), BigUint::from(0b11u8));
    }

    /// A message is assigned to a wider variable as a value of its own
    /// signedness.
    fn mailbox_messages_keep_their_signedness(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int got, output int peeked, output int tried,
           output longint unsigned wide);
    mailbox #(byte) m = new;
    mailbox #(byte unsigned) u = new;
    initial begin
        m.put(-1);
        m.peek(peeked);
        void'(m.try_get(tried));
        m.put(-2);
        m.get(got);
        u.put(8'hff);
        u.get(wide);
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("peeked")), BigUint::from(u32::MAX));
        assert_eq!(sim.get(sim.signal("tried")), BigUint::from(u32::MAX));
        assert_eq!(sim.get(sim.signal("got")), BigUint::from(u32::MAX - 1));
        assert_eq!(sim.get(sim.signal("wide")), BigUint::from(0xffu8));
    }

    /// A process that a mailbox serves while it is suspended collects the
    /// result when it is resumed.
    fn mailboxes_serve_suspended_processes(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int x, output bit [1:0] done);
    mailbox #(int) m = new, b = new(1);
    process p, q;
    initial begin
        int v;
        fork begin p = process::self(); m.get(x); done[0] = 1; end join_none
        b.put(1);
        fork begin q = process::self(); b.put(2); done[1] = 1; end join_none
        #1 p.suspend();
        q.suspend();
        #1 m.put(5);
        b.get(v);
        #1 p.resume();
        q.resume();
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("done")), BigUint::from(0u8));
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(sim.signal("x")), BigUint::from(5u8));
        assert_eq!(sim.get(sim.signal("done")), BigUint::from(0b11u8));
    }

    /// A message narrower than the message type is extended as its own
    /// signedness says.
    fn mailbox_puts_extend_messages_by_their_signedness(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int a, output int unsigned b, output int c);
    mailbox #(int) m = new;
    mailbox #(int unsigned) u = new;
    byte unsigned v = 8'hff;
    byte s = -1;
    initial begin
        m.put(v);
        m.get(a);
        u.put(s);
        u.get(b);
        void'(m.try_put(v));
        m.get(c);
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("a")), BigUint::from(0xffu8));
        assert_eq!(sim.get(sim.signal("b")), BigUint::from(u32::MAX));
        assert_eq!(sim.get(sim.signal("c")), BigUint::from(0xffu8));
    }

    /// A mailbox type names a mailbox type as its message type directly.
    fn mailboxes_of_mailboxes(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int v, output int w);
    mailbox #(mailbox #(int)) boxes = new;
    std::mailbox #(std::mailbox #(int)) scoped = new;
    initial begin
        mailbox #(int) inner = new, got;
        boxes.put(inner);
        boxes.get(got);
        got.put(4);
        inner.get(v);
        scoped.put(inner);
        scoped.get(got);
        got.put(6);
        inner.get(w);
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("v")), BigUint::from(4u8));
        assert_eq!(sim.get(sim.signal("w")), BigUint::from(6u8));
    }

    /// Mailboxes of process and event handles, whose messages a variable of
    /// the same type receives.
    fn mailboxes_of_processes_and_events(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [1:0] ok);
    mailbox #(event) events = new;
    mailbox #(process) processes = new;
    event e;
    initial begin
        event got;
        process p;
        events.put(e);
        events.get(got);
        fork begin #1 ->e; end join_none
        @got ok[0] = 1;
        processes.put(process::self());
        void'(processes.try_get(p));
        ok[1] = p.status() == process::RUNNING;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("ok")), BigUint::from(0b11u8));
    }

    /// The methods that return a message write it to a class property, also
    /// one a method of its class names.
    fn mailboxes_write_class_properties(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int id, output int count, output bit ok);
    class item;
        int id;
        string name;
        int count;
        task take(mailbox #(int) m);
            m.get(count);
        endtask
    endclass
    mailbox #(int) ids = new;
    mailbox #(string) names = new;
    initial begin
        item t = new;
        ids.put(7);
        names.put("seven");
        ids.get(t.id);
        void'(names.try_peek(t.name));
        fork t.take(ids); join_none
        #1 ids.put(3);
        #1 id = t.id;
        count = t.count;
        ok = t.name == "seven";
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(sim.signal("id")), BigUint::from(7u8));
        assert_eq!(sim.get(sim.signal("count")), BigUint::from(3u8));
        assert_eq!(sim.get(sim.signal("ok")), BigUint::from(1u8));
    }
}

/// A negative bound is an error (15.4.1).
#[test]
fn negative_mailbox_bounds_are_errors() {
    for bound in ["-1", "n"] {
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top; mailbox #(int) m; byte n = -1; initial begin #1 m = new({bound}); end endmodule"
        );
        let mut sim = simulation(&source).build().unwrap();
        let error = format!("{:?}", sim.run_until(2).unwrap_err());
        assert!(error.contains("negative bound"), "{bound}: {error}");
    }
}

/// Mailbox forms that Celox does not implement, or that IEEE 1800-2023 15.4
/// does not allow, are rejected.
#[test]
fn unsupported_mailbox_forms_are_rejected() {
    for (body, construct) in [
        ("mailbox m;", "mailbox without a message type"),
        (
            "mailbox #(logic [64:0]) m;",
            "mailbox messages wider than 64 bits",
        ),
        (
            "mailbox #(int) m = new; function void f(); int v; m.get(v); endfunction",
            "mailbox put, get or peek",
        ),
        (
            "mailbox #(int) m = new; int x; initial x = m.put(1);",
            "mailbox put, which is a task, used as a value",
        ),
        (
            "mailbox #(int) m = new; string s; initial m.get(s);",
            "mailbox get into a variable of another type than its messages",
        ),
        (
            "mailbox #(event) m = new; process p; initial m.get(p);",
            "mailbox get into a variable of another type than its messages",
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
