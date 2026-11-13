//! Class objects, properties, and methods (IEEE 1800-2023 clause 8).

use super::*;

sv_backends! {
    // Actuals are evaluated in the order they are written, as for other
    // calls with named arguments.
    fn named_constructor_arguments_are_evaluated_in_written_order(sim) {
        @setup {
            let source = r#"module Top(output int x, output int y);
                class Point;
                    int x;
                    int y;
                    function new(int x = 0, int y = 0);
                        this.x = x;
                        this.y = y;
                    endfunction
                endclass
                int count;
                function int next();
                    count++;
                    return count;
                endfunction
                Point point;
                initial begin
                    point = new(.y(next()), .x(next()));
                    x = point.x;
                    y = point.y;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("order.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("x")), 2u32.into());
        assert_eq!(sim.get(sim.signal("y")), 1u32.into());
    }

    fn named_method_arguments_are_evaluated_in_written_order(sim) {
        @setup {
            let source = r#"module Top(output int x, output int y);
                class Point;
                    int x;
                    int y;
                    function void set(int x = 0, int y = 0);
                        this.x = x;
                        this.y = y;
                    endfunction
                endclass
                int count;
                function int next();
                    count++;
                    return count;
                endfunction
                Point point;
                initial begin
                    point = new;
                    point.set(.y(next()), .x(next()));
                    x = point.x;
                    y = point.y;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("order.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("x")), 2u32.into());
        assert_eq!(sim.get(sim.signal("y")), 1u32.into());
    }

    // The shape of uvm_factory's `m_matches_type_pair` calls: an
    // out-of-block method bound by name, with a `null` actual. Out of the
    // shared cases because Icarus does not support out-of-block methods and
    // xezim evaluates them wrongly in an instantiated module.
    fn named_arguments_of_out_of_block_methods(sim) {
        @setup {
            let source = r#"module Top(output int hits);
                class Pair;
                    string name;
                    function new(string name);
                        this.name = name;
                    endfunction
                endclass
                class Factory;
                    extern protected function bit matches_type_pair(Pair match_type_pair,
                        Pair requested_type, string requested_type_name);
                    function int find(Pair pair, string name);
                        int found = 0;
                        if (matches_type_pair(.match_type_pair(pair),
                                              .requested_type(null),
                                              .requested_type_name(name)))
                            found += 1;
                        if (matches_type_pair(.requested_type_name("other"),
                                              .requested_type(pair),
                                              .match_type_pair(pair)))
                            found += 10;
                        return found;
                    endfunction
                endclass
                function bit Factory::matches_type_pair(Pair match_type_pair,
                    Pair requested_type, string requested_type_name);
                    return match_type_pair == requested_type
                        || match_type_pair.name == requested_type_name;
                endfunction
                Factory factory;
                Pair pair;
                initial begin
                    factory = new;
                    pair = new("x");
                    hits = factory.find(pair, "x") * 100 + factory.find(pair, "y");
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("factory.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("hits")), 1110u32.into());
    }

    fn null_dereferences_are_runtime_errors(sim) {
        @setup {
            let source = r#"module Top(output int value, output bit done);
                class C;
                    int x;
                endclass
                C c;
                initial begin
                    value = c.x;
                    done = 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("null.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("null object handle"), "{error}");
    }

    fn shallow_copies_of_null_are_runtime_errors(sim) {
        @setup {
            let source = r#"module Top(output bit done);
                class C;
                    int x;
                endclass
                C a;
                C b;
                initial begin
                    b = new a;
                    done = 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("null.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("null object handle"), "{error}");
    }

    fn virtual_calls_through_null_are_runtime_errors(sim) {
        @setup {
            let source = r#"module Top(output int value, output bit done);
                class C;
                    virtual function int f();
                        return 1;
                    endfunction
                endclass
                C c;
                initial begin
                    value = c.f();
                    done = 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("null.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("null object handle"), "{error}");
    }

    fn checkpoints_keep_objects_that_state_files_cannot_record(sim) {
        @setup {
            let source = r#"module Top(output int value);
                timeunit 1ps / 1ps;
                class C;
                    int x;
                endclass
                C c;
                initial begin
                    c = new;
                    c.x = 7;
                    #10 value = c.x;
                    #10 c.x = 8;
                    #10 value = c.x;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("checkpoint.sv"))], "Top");
        sim.run_until(5).unwrap();
        let error = sim.save_state().unwrap_err().to_string();
        assert!(error.contains("class objects"), "{error}");
        let checkpoint = sim.checkpoint().unwrap();
        let value = sim.signal("value");
        sim.run_until(40).unwrap();
        assert_eq!(sim.get(value), BigUint::from(8u8));
        sim.restore(&checkpoint).unwrap();
        sim.run_until(15).unwrap();
        assert_eq!(sim.get(value), BigUint::from(7u8));
        sim.run_until(40).unwrap();
        assert_eq!(sim.get(value), BigUint::from(8u8));
    }
}

sv_backends! {

    fn four_state_properties_are_two_state_in_two_state_mode(sim) {
        @setup {
            let source = r#"module Top(output longint t, output bit [3:0] l, output bit [3:0] k);
                class C;
                    time t;
                    logic [3:0] l;
                endclass
                C c;
                initial begin
                    c = new;
                    t = c.t;
                    l = c.l;
                    c.l = 4'b1z0x;
                    k = c.l;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
            .four_state(false);
        sim.run_until(0).unwrap();
        for (name, value) in [("t", 0u8), ("l", 0), ("k", 0b1000)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }
}

/// The members of an enumeration type that a property declares in place are
/// constants of the class scope, as those of a typedef in the class are
/// (IEEE 1800-2023 6.19, 8.3): the class's methods and a derived class's name
/// them, and `C::NAME` reads them outside, as UVM's `uvm_cmdline_verbosity`
/// does with `enum {STANDARD, NON_STANDARD, ILLEGAL} src`.
#[test]
fn members_of_enumerations_declared_by_properties() {
    let source = r#"
    module Top(output int non_standard, output int illegal, output int marked, output bit [1:0] mode_bits);
        class Setting;
            enum {STANDARD, NON_STANDARD, ILLEGAL} src;
            enum bit [1:0] {OFF = 1, ON = 2} mode;
            static Setting settings[$];
            function void mark(bit legal);
                src = legal ? NON_STANDARD : ILLEGAL;
                mode = ON;
            endfunction
            static function int count(int which);
                int n = 0;
                foreach (settings[i])
                    if (which == 0 ? settings[i].src == NON_STANDARD : settings[i].src == ILLEGAL)
                        n++;
                return n;
            endfunction
        endclass
        class Marked extends Setting;
            function bit standard();
                return src == STANDARD && mode != OFF;
            endfunction
        endclass
        initial begin
            Setting a = new;
            Setting b = new;
            Marked c = new;
            a.mark(1);
            b.mark(0);
            c.mode = Setting::ON;
            Setting::settings.push_back(a);
            Setting::settings.push_back(b);
            Setting::settings.push_back(c);
            non_standard = Setting::count(0);
            illegal = Setting::count(1);
            marked = c.standard();
            mode_bits = a.mode;
        end
    endmodule"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(0).unwrap();
        for (name, expected) in [
            ("non_standard", 1u8),
            ("illegal", 1),
            ("marked", 1),
            ("mode_bits", 2),
        ] {
            let signal = sim.signal(name);
            assert_eq!(
                sim.get(signal),
                BigUint::from(expected),
                "{name}, four-state {four_state}"
            );
        }
    }
}

/// A static method called without its empty parentheses may be the receiver
/// of a member, and a method that returns a chandle so called gives a chandle
/// (IEEE 1800-2023 13.5.5).
#[test]
fn parenless_static_factories_and_chandle_results() {
    let source = r#"
    module Top(output int latest_id, output int latest_value, output bit empty);
        class Node;
            static Node last;
            int id;
            chandle ch;
            function new(int id); this.id = id; last = this; endfunction
            function int get_id(); return id; endfunction
            function chandle get_handle(); return ch; endfunction
            static function Node latest(); return last; endfunction
        endclass
        Node n;
        chandle h;
        initial begin
            n = new(3);
            n = new(5);
            latest_id = Node::latest.get_id();
            latest_value = Node::latest.id;
            h = n.get_handle;
            empty = h == null;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let latest_id = sim.signal("latest_id");
    let latest_value = sim.signal("latest_value");
    let empty = sim.signal("empty");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(latest_id), BigUint::from(5u8));
    assert_eq!(sim.get(latest_value), BigUint::from(5u8));
    assert_eq!(sim.get(empty), BigUint::from(1u8));
}

/// A method that returns an unpacked structure or a virtual interface and is
/// called without its empty parentheses gives a value of that type, to an
/// assignment and to an argument (IEEE 1800-2023 13.5.5).
#[test]
fn parenless_method_results_of_structure_and_interface_types() {
    let source = r#"
    interface Bus; int data; endinterface
    module Top(output int sum, output int passed, output int read);
        typedef struct { int a; int b; } pair_t;
        class Holder;
            pair_t p;
            virtual Bus bus;
            function pair_t get(); return p; endfunction
            function virtual Bus get_bus(); return bus; endfunction
        endclass
        function automatic int total(pair_t q); return q.a + q.b; endfunction
        Bus b();
        Holder h;
        pair_t g;
        virtual Bus v;
        initial begin
            h = new;
            h.p.a = 2; h.p.b = 5;
            h.bus = b;
            b.data = 7;
            g = h.get;
            sum = g.a + g.b;
            passed = total(h.get);
            v = h.get_bus;
            read = v.data;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let sum = sim.signal("sum");
    let passed = sim.signal("passed");
    let read = sim.signal("read");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sum), BigUint::from(7u8));
    assert_eq!(sim.get(passed), BigUint::from(7u8));
    assert_eq!(sim.get(read), BigUint::from(7u8));
}

/// A select or a member of a property is the target of a for-loop's
/// initialization and step, and a tagged expression assigned to a tagged union property takes
/// the property's type (IEEE 1800-2023 12.7.1, 11.9). Verilator 5.052
/// rejects a select as a for-loop target, and Icarus and xezim tagged
/// unions, so this is not a shared case.
#[test]
fn property_targets_of_loop_headers_and_tagged_values() {
    let source = r#"
    module Top(output int steps, output bit [8:0] full);
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { bit [3:0] x; bit [3:0] y; } pair_t;
        class Info; bit [7:0] bits; item_t item; pair_t pair; endclass
        Info info;
        initial begin
            info = new;
            steps = 0;
            for (info.bits[3:0] = 4'h1; info.bits[3:0] < 4'h4; info.bits[3:0] += 1) steps++;
            for (info.pair.x = 4'h1; info.pair.x < 4'h3; info.pair.x++) steps++;
            info.item = tagged Full 8'h5a;
            full = 9'(info.item);
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let steps = sim.signal("steps");
    let full = sim.signal("full");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(steps), BigUint::from(5u8));
    assert_eq!(sim.get(full), BigUint::from(0x15au16));
}

/// An assignment operator evaluates the address of its target once
/// (IEEE 1800-2023 11.4.1), before it reads the target, so it writes the
/// object it read even when its value changes what the target names. The
/// standard does not say when the address is evaluated, and Verilator
/// 5.052 evaluates it after the value, so this is not a shared case.
#[test]
fn assignment_operators_write_the_place_they_read() {
    let source = r#"
    module Top(output bit [7:0] bits, output bit [7:0] element, output bit [7:0] other_bits,
               output bit [7:0] other_element);
        class Info; bit [7:0] bits; bit [7:0] q[$]; endclass
        Info info, other, saved;
        function automatic bit [3:0] rebind(); info = other; return 4'h1; endfunction
        initial begin
            info = new;
            other = new;
            saved = info;
            info.bits = 8'h10;
            info.q.push_back(8'h10);
            other.q.push_back(8'h20);
            info.bits[3:0] += rebind();
            info = saved;
            info.q[0][3:0] += rebind();
            bits = saved.bits;
            element = saved.q[0];
            other_bits = other.bits;
            other_element = other.q[0];
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let signals = ["bits", "element", "other_bits", "other_element"].map(|name| sim.signal(name));
    sim.run_until(0).unwrap();
    let values = signals.map(|signal| sim.get(signal));
    assert_eq!(values, [0x11u8, 0x11, 0x00, 0x20].map(BigUint::from));
}

/// A const class property is written only by its initializer or, without
/// one, in its class's constructor (IEEE 1800-2023 8.19), whatever form
/// the write takes.
#[test]
fn const_property_writes_are_rejected() {
    let writes = [
        "c.k = 8'h1;",
        "c.k[3:0] = 4'h1;",
        "c.k += 8'h1;",
        "c.k++;",
        "{c.k, c.v} = 16'h1;",
        "c.s = \"x\";",
        "c.s.putc(0, \"y\");",
        "c.h = null;",
        "c.fixed = 8'h1;",
        "c.p.x = 4'hf;",
        "void'($sscanf(\"5\", \"%d\", c.k));",
    ];
    for write in writes {
        let source = format!(
            "module Top;
                typedef struct packed {{ bit [3:0] x; bit [3:0] y; }} pair_t;
                class C;
                    const pair_t p;
                    const bit [7:0] k;
                    const bit [7:0] fixed = 8'h3;
                    bit [7:0] v;
                    const string s;
                    const C h;
                    function new();
                        k = 8'h2;
                        s = \"a\";
                        h = null;
                    endfunction
                endclass
                initial begin
                    automatic C c = new;
                    {write}
                end
            endmodule"
        );
        let error =
            celox::Simulation::from_sv_sources(vec![(&*source, Path::new("class.sv"))], "Top")
                .build()
                .err()
                .unwrap_or_else(|| panic!("`{write}` was accepted"))
                .to_string();
        assert!(error.contains("const class property"), "`{write}`: {error}");
    }
}

/// A member of a tagged union nested in a packed structure property is
/// read and written with the check of its active member (IEEE 1800-2023
/// 7.3.2), as one of a variable is: an inactive member stops the run.
#[test]
fn inactive_tagged_members_of_properties_stop_the_run() {
    for access in [
        "r = info.record.item.Full;",
        "info.record.item.Full = 8'h42;",
        // The check of the value is the value's, whatever it is assigned to.
        "info.record.n = info.record.item.Full[3:0];",
        "info.record.n = value.item.Full[3:0];",
    ] {
        let source = format!(
            "module Top(output bit [7:0] r);
                typedef union tagged packed {{ void Empty; bit [7:0] Full; }} item_t;
                typedef struct packed {{ item_t item; bit [3:0] n; }} record_t;
                class Info; record_t record; endclass
                Info info;
                record_t value;
                initial begin info = new; value.item = tagged Empty; info.record = value; {access} end
            endmodule"
        );
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build()
        .unwrap();
        let error = sim.run_until(0).expect_err(access).to_string();
        assert!(error.contains("active tag"), "{access}: {error}");
    }
}

/// The check of a tagged member that an assignment writes reads the tag
/// when the member is written, after the value is evaluated (IEEE 1800-2023
/// 7.3.2, 10.4.1). Icarus and xezim reject tagged unions, so this is not a
/// shared case.
#[test]
fn tagged_member_targets_are_checked_when_written() {
    let source = r#"
    module Top(output bit [8:0] full);
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { item_t item; } record_t;
        class Info; record_t record; endclass
        Info info;
        function automatic bit [7:0] fill();
            info.record.item = tagged Full 8'h00;
            return 8'h5a;
        endfunction
        initial begin
            info = new;
            info.record.item = tagged Empty;
            info.record.item.Full = fill();
            full = 9'(info.record.item);
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let full = sim.signal("full");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(full), BigUint::from(0x15au16));
}

/// An integral update of a whole tagged union would change its tag with
/// its bits (IEEE 1800-2023 7.3.2), so it is rejected whatever form the
/// target takes: a variable, a member of a variable, a class property, a
/// member of a property, or an element of an array of unions.
#[test]
fn integral_updates_of_tagged_union_targets_are_rejected() {
    for update in [
        "item += 1;",
        "record.item += 1;",
        "items[1] += 1;",
        "info.item += 1;",
        "info.record.item += 1;",
        "info.record.item++;",
        "unpacked[0] += 1;",
        "info.items[0]++;",
    ] {
        let source = format!(
            "module Top;
                typedef union tagged packed {{ void Empty; bit [7:0] Full; }} item_t;
                typedef struct packed {{ item_t item; bit [3:0] n; }} record_t;
                class Info; item_t item; record_t record; item_t items[$]; endclass
                item_t item;
                item_t [1:0] items;
                item_t unpacked[2];
                record_t record;
                initial begin
                    automatic Info info = new;
                    {update}
                end
            endmodule"
        );
        let error =
            celox::Simulation::from_sv_sources(vec![(&*source, Path::new("class.sv"))], "Top")
                .build()
                .err()
                .unwrap_or_else(|| panic!("`{update}` was accepted"))
                .to_string();
        assert!(error.contains("tagged union"), "`{update}`: {error}");
    }
}

/// A select of the bits of an element of an unpacked array of structures
/// that hold a tagged union is an ordinary vector, which an integral update
/// may write: the target's type is the type the selection of the whole
/// array gives (IEEE 1800-2023 7.4.6, 11.5.1).
#[test]
fn bit_selects_of_elements_holding_tagged_unions_are_vectors() {
    let source = r#"
    module Top(output bit [12:0] whole);
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { item_t item; bit [3:0] n; } record_t;
        record_t records[2];
        initial begin
            records[0].item = tagged Full 8'h41;
            records[0].n = 4'h2;
            records[0][3:0] += 1;
            whole = records[0];
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let whole = sim.signal("whole");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(whole), BigUint::from(0x1413u16));
}

/// An assignment operator reads its target, with the target's checks,
/// before it evaluates its value (IEEE 1800-2023 11.4.1): a value that
/// makes an inactive member active does not hide the invalid read.
#[test]
fn assignment_operators_check_their_target_before_the_value() {
    let source = r#"
    module Top;
        typedef union tagged packed { void Empty; bit [7:0] Full; } item_t;
        typedef struct packed { item_t item; bit [3:0] n; } record_t;
        class Info; record_t record; endclass
        Info info;
        function automatic bit [7:0] activate();
            info.record.item = tagged Full 8'h00;
            return 8'h01;
        endfunction
        initial begin
            info = new;
            info.record.item = tagged Empty;
            info.record.item.Full += activate();
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    let error = sim.run_until(0).unwrap_err().to_string();
    assert!(error.contains("active tag"), "{error}");
}

/// A two-state member of a four-state packed structure property takes no
/// unknown bits (IEEE 1800-2023 7.2.1), assigned as a member.
#[test]
fn two_state_members_of_packed_structure_properties_drop_unknown_bits() {
    let source = r#"
    module Top(output logic [1:0] whole);
        typedef struct packed { bit b; logic l; } mixed_t;
        class Info; mixed_t p; endclass
        Info info;
        initial begin info = new; info.p.b = 1'bx; info.p.l = 1'b1; whole = info.p; end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .four_state(true)
        .build()
        .unwrap();
    let whole = sim.signal("whole");
    sim.run_until(0).unwrap();
    let (value, mask) = sim.get_four_state(whole);
    assert_eq!((value, mask), (BigUint::from(1u8), BigUint::from(0u8)));
}

/// A store of a class property wakes a process waiting on it (IEEE
/// 1800-2023 9.4.3), whose condition reads it in a method through `this`,
/// through a handle, through a queue held in a property and through a
/// function. The random mode of a property has no such count, so a wait on
/// it is rejected instead of never waking. A store of the same property of
/// another object or of its own value checks the condition again, so a
/// condition that reads a property must be a value of what it reads: one
/// with an effect or reading a mailbox or the time is rejected.
#[test]
fn waits_on_class_properties_wake_on_stores() {
    let source = |wait: &str| {
        format!(
            "`timescale 1ns/1ns
            class C;
                int m_state;
                int q[$];
                rand int r;
                int calls;
                mailbox #(int) mb = new;
                C other;
                function bit reached(int s); return m_state >= s; endfunction
                function bit counted(int s); calls++; return m_state >= s; endfunction
                task wait_for(int s); {wait} endtask
            endclass
            module Top(output int t);
                initial begin
                    automatic C c = new;
                    c.other = c;
                    fork
                        begin c.wait_for(2); t = $time; end
                        begin #1 c.m_state = 1; #1 c.m_state = 2; c.q.push_back(1); end
                    join
                end
            endmodule"
        )
    };
    let waits = [
        "wait (m_state >= s);",
        "wait (other.m_state >= s);",
        "wait (q.size() != 0);",
        "wait (reached(s));",
    ];
    for wait in waits {
        let source = source(wait);
        for four_state in [false, true] {
            let builder = || {
                celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("class.sv"))],
                    "Top",
                )
                .four_state(four_state)
            };
            fn check<B: celox::SimBackend>(
                sim: Result<celox::Simulation<B>, celox::SimulatorError>,
                case: &str,
            ) {
                let mut sim = sim.unwrap_or_else(|error| panic!("{case}: {error}"));
                sim.run_until(5).unwrap();
                let t = sim.signal("t");
                assert_eq!(sim.get(t), BigUint::from(2u8), "{case}");
            }
            let case = format!("{wait}, four-state {four_state}");
            check(builder().build_native(), &case);
            check(builder().build_cranelift(), &case);
            check(builder().build_wasm(), &case);
            check(builder().build_interpreter(), &case);
        }
    }
    for (wait, construct) in [
        ("wait (r.rand_mode() == 0);", "in a wait condition"),
        (
            "wait (counted(s));",
            "property write in a wait condition that reads a class property",
        ),
        (
            "wait (mb.num() > 0);",
            "in a wait condition that reads a class property",
        ),
        (
            "wait (m_state >= s && $time > 0);",
            "$time in a wait condition that reads a class property",
        ),
    ] {
        let source = source(wait);
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build_interpreter()
        .expect_err(wait)
        .to_string();
        assert!(error.contains(construct), "{wait}: {error}");
    }
}

/// The default of an argument is read only where a call omits the argument,
/// so a wait that supplies it wakes on the argument, and one that omits it
/// on stores of the property the default reads.
#[test]
fn waits_read_property_defaults_only_when_omitted() {
    let source = |call: &str| {
        format!(
            "`timescale 1ns/1ns
            class C; int state; endclass
            module Top(output int seen);
                C c;
                int signal;
                function automatic bit ready(int x = c.state); return x != 0; endfunction
                initial begin
                    c = new;
                    fork
                        begin wait ({call}); seen = $time; end
                        begin #2 signal = 1; end
                    join
                end
            endmodule"
        )
    };
    let supplied = source("ready(signal)");
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(supplied.as_str(), Path::new("class.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    sim.run_until(5).unwrap();
    let seen = sim.signal("seen");
    assert_eq!(sim.get(seen), BigUint::from(2u8));
    // A default that reads a property makes the wait watch the property.
    let omitted = source("ready()").replace("#2 signal = 1;", "#3 c.state = 1;");
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(omitted.as_str(), Path::new("class.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    sim.run_until(5).unwrap();
    let seen = sim.signal("seen");
    assert_eq!(sim.get(seen), BigUint::from(3u8));
}

/// The defaults of a method use the object it is called on, as the call
/// evaluated its receiver, also when an argument then assigns the variable
/// the receiver names (IEEE 1800-2023 13.5.3). Verilator 5.052 reads the
/// variable again, so this is not a shared case.
#[test]
fn method_defaults_use_the_receiver_the_call_evaluated() {
    let source = r#"
    module Top(output int r);
        class Pair;
            int n;
            function int f(int x, int y = n); return x + y; endfunction
        endclass
        Pair a, b, cur;
        function automatic int swap(); cur = b; return 0; endfunction
        initial begin
            a = new;
            a.n = 1;
            b = new;
            b.n = 2;
            cur = a;
            r = cur.f(swap());
        end
    endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("receiver.sv"))], "Top")
            .build()
            .unwrap();
    let r = sim.signal("r");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(r), BigUint::from(1u8));
}

/// A constraint may not name a property that a constraint program does not
/// hold, and such a property may not be random; the class is otherwise
/// randomized with it as state.
#[test]
fn constraints_on_handle_and_string_properties_are_rejected() {
    let wide = "class C; rand bit [3:0] x; bit [127:0] w; constraint k { x < 5; } endclass";
    for (body, call, want) in [
        (
            "class C; rand bit [3:0] x; string s; constraint k { x < s.len(); } endclass",
            "c.randomize()",
            "constraint",
        ),
        (
            "class N; endclass class C; rand bit [3:0] x; N n; constraint k { x == n; } endclass",
            "c.randomize()",
            "a handle, string, container or property wider than 64 bits in a constraint",
        ),
        (
            "class C; rand bit [3:0] x; bit [127:0] w; constraint k { x < w; } endclass",
            "c.randomize()",
            "a handle, string, container or property wider than 64 bits in a constraint",
        ),
        (
            "class C; rand bit [3:0] x; rand string q[$]; constraint k { x < 5; } endclass",
            "c.randomize()",
            "rand dynamic arrays and queues currently require 2-state integral elements",
        ),
        (
            "class C; rand bit [3:0] x; rand logic [3:0] d[]; constraint k { x < 5; } endclass",
            "c.randomize()",
            "rand dynamic arrays and queues currently require 2-state integral elements",
        ),
        (
            "class C; rand bit [3:0] x; randc bit [3:0] d[]; constraint k { x < 5; } endclass",
            "c.randomize()",
            "a randc dynamic array or queue",
        ),
        (
            "class C; rand int x; int a[]; constraint k { foreach (a[i]) x != a[i + 1]; } endclass",
            "c.randomize()",
            "constraint array element other than its loop's",
        ),
        // A member of a loop element is subject to its visibility (8.18).
        (
            "class R; local int m; endclass \
             class C; rand int x; R rs[$]; constraint k { foreach (rs[i]) x != rs[i].m; } endclass",
            "c.randomize()",
            "local member `m` of class",
        ),
        (
            "class R; protected int m; endclass \
             class C; rand int x; R rs[$]; constraint k { foreach (rs[i]) x != rs[i].m; } endclass",
            "c.randomize()",
            "protected member `m` of class",
        ),
        // An inline constraint accesses members as the caller does (18.7).
        (
            "class R; local int m; endclass \
             class C; rand int x; R rs[$]; endclass",
            "c.randomize() with { foreach (rs[i]) x != rs[i].m; }",
            "local member `m` of class",
        ),
        // A chandle has no value a constraint takes (6.14).
        (
            "class R; chandle h; endclass \
             class C; rand int x; R rs[$]; constraint k { foreach (rs[i]) x != rs[i].h; } endclass",
            "c.randomize()",
            "a chandle in a constraint",
        ),
        // A process reference is no value either (9.7).
        (
            "class C; rand int x; process ps[$]; constraint k { foreach (ps[i]) x != ps[i]; } endclass",
            "c.randomize()",
            "a constraint array element other than 2-state integral",
        ),
        (
            "class C; rand int x; chandle hs[$]; constraint k { foreach (hs[i]) x != hs[i]; } endclass",
            "c.randomize()",
            "a constraint array element other than 2-state integral",
        ),
        // The array of a loop is subject to its visibility too (8.18).
        (
            "class C; rand int x; local int a[]; endclass",
            "c.randomize() with { foreach (a[i]) x != a[i]; }",
            "local member `a` of class",
        ),
        (
            "class B; local int a[]; endclass \
             class C extends B; rand int x; constraint k { foreach (a[i]) x != a[i]; } endclass",
            "c.randomize()",
            "local member `a` of class",
        ),
        // A real has no integral value a constraint takes (18.3).
        (
            "class C; rand int x; real t; constraint k { x < t; } endclass",
            "c.randomize()",
            "a real in a constraint",
        ),
        (
            "class R; real t; endclass \
             class C; rand int x; R rs[$]; constraint k { foreach (rs[i]) x < rs[i].t; } endclass",
            "c.randomize()",
            "a real in a constraint",
        ),
        // The size and indices of an associative array are not random (18.4).
        (
            "class C; rand bit [3:0] x; rand int a[int]; constraint k { x < 5; } endclass",
            "c.randomize()",
            "constraints currently require scalar integral rand properties",
        ),
        (
            "class C; rand bit [3:0] x; rand bit [127:0] w; constraint k { x < 5; } endclass",
            "c.randomize()",
            "constraints currently require scalar integral rand properties",
        ),
        // Selecting a wide property makes it random, which the solver
        // cannot do, with constraints of the class or of the call.
        (
            wide,
            "c.randomize(w)",
            "randomize argument wider than 64 bits under constraints",
        ),
        (
            "class C; rand bit [3:0] x; bit [127:0] w; endclass",
            "c.randomize(w) with { x < 5; }",
            "randomize argument wider than 64 bits under constraints",
        ),
        // A base handle dispatches to the randomizer of a constrained
        // derived class (18.6.2).
        (
            "class C; rand bit [3:0] x; bit [127:0] w; endclass \
             class D extends C; constraint k { x < 5; } endclass",
            "c.randomize(w)",
            "randomize argument wider than 64 bits under constraints",
        ),
    ] {
        let source =
            format!("module Top; {body} C c; initial begin c = new; void'({call}); end endmodule");
        let Err(error) = Simulator::from_sv_sources(vec![(&source, Path::new("class.sv"))], "Top")
            .build_cranelift()
        else {
            panic!("{body} {call} is unsupported");
        };
        let error = error.to_string();
        assert!(error.contains(want), "{body} {call}: {error}");
    }
}

/// The objects rand object handles reach form a graph that may be cyclic:
/// each object is solved, and its hooks run, once per call (IEEE 1800-2023
/// 18.4, 18.6.2). Verilator 5.052 rejects a rand member of the containing
/// class's type and does not finish compiling a cycle through two classes.
#[test]
fn rand_object_cycles_are_solved_once() {
    let source = r#"module Top(output int sum, output bit done);
    class Leaf;
        rand bit [3:0] v;
        int pre, post;
        constraint c { v inside {[3:5]}; }
        function void pre_randomize(); pre++; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    class SubLeaf extends Leaf;
        constraint d { v != 4; }
    endclass
    class Stuck extends Leaf;
        constraint e { v == 9; }
    endclass
    class Node;
        rand bit [3:0] w;
        rand Leaf leaf;
        rand Leaf none;
        Leaf plain;
        rand Node self_ref;
        constraint k { w < 3; }
    endclass
    class Holder;
        rand Leaf leaf;
    endclass
    Node n;
    SubLeaf s;
    Holder h;
    int bad;
    initial begin
        n = new;
        s = new;
        n.leaf = s;
        n.plain = new;
        n.self_ref = n;
        for (int i = 0; i < 20; i++) begin
            if (!n.randomize()) $fatal(1, "randomize");
            if (n.w >= 3 || !(s.v inside {3, 5})) bad++;
        end
        // A non-rand handle is not randomized; the hooks ran once per call.
        sum = s.pre + s.post + n.plain.v + n.plain.pre;
        // A class without constraints of its own solves its rand objects.
        h = new;
        h.leaf = new;
        if (!h.randomize() || !(h.leaf.v inside {[3:5]}) || h.leaf.pre != 1) bad++;
        // An unsatisfiable member fails the call, changes nothing and runs
        // no post_randomize.
        begin
            Stuck stuck = new;
            n.leaf = stuck;
            n.w = 2;
            if (n.randomize() || n.w != 2 || stuck.v != 0 || stuck.pre != 1 || stuck.post != 0) bad++;
        end
        $display("%0d %0d", sum, bad);
        done = bad == 0;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("cycle.sv"))], "Top")
        .build()
        .unwrap();
    let (sum, done) = (sim.signal("sum"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sum), BigUint::from(40u8));
    assert_eq!(sim.get(done), BigUint::from(1u8));
}

/// post_randomize runs on the rand object members the solve took, also when
/// the containing object's post_randomize clears or replaces the handle
/// first, and pre_randomize on those the object's own pre_randomize
/// allocates (IEEE 1800-2023 18.6.2). A randomize call inside a hook does
/// not change them, and the hooks it dispatches are those of the classes
/// its rand handles can refer to, not the hook calling it. randomize(null)
/// makes the rand handles state too (18.11, 18.11.1): it checks the
/// object's own constraints and runs the hooks of no member.
#[test]
fn post_randomize_runs_on_the_solved_members() {
    let source = r#"module Top(output int bad, output bit done);
    class Leaf;
        rand bit [3:0] v;
        int pre, post;
        constraint c { v inside {[3:5]}; }
        function void pre_randomize(); pre++; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    class Other;
        rand Leaf leaf;
    endclass
    class Parent;
        rand Leaf leaf;
        int mode;
        Leaf spare;
        Other other;
        function void pre_randomize();
            if (mode == 3) leaf = new;
        endfunction
        function void post_randomize();
            if (mode == 1) leaf = null;
            if (mode == 2) leaf = spare;
            if (mode == 4) void'(other.randomize());
        endfunction
    endclass
    Parent p;
    Leaf first, second;
    initial begin
        p = new;
        p.spare = new;
        // Cleared by the parent's post_randomize.
        first = new;
        p.leaf = first;
        p.mode = 1;
        if (!p.randomize() || p.leaf != null || first.pre != 1 || first.post != 1
            || !(first.v inside {[3:5]})) bad++;
        // Replaced: the solved leaf's hook runs, the replacement's does not.
        second = new;
        p.leaf = second;
        p.mode = 2;
        if (!p.randomize() || p.leaf != p.spare || second.post != 1 || p.spare.pre != 0
            || p.spare.post != 0) bad++;
        // Allocated by the parent's pre_randomize: solved, both hooks run.
        p.mode = 3;
        p.leaf = null;
        if (!p.randomize() || p.leaf == null || p.leaf.pre != 1 || p.leaf.post != 1
            || !(p.leaf.v inside {[3:5]})) bad++;
        // A randomize call in the parent's post_randomize keeps the
        // parent's members.
        p.mode = 4;
        p.other = new;
        p.other.leaf = new;
        first = new;
        p.leaf = first;
        if (!p.randomize() || first.post != 1 || p.other.leaf.post != 1
            || !(p.other.leaf.v inside {[3:5]})) bad++;
        // randomize(null): the member's constraint and hooks are not part
        // of the check.
        p.mode = 0;
        first = new;
        first.v = 9;
        p.leaf = first;
        if (p.randomize(null) != 1 || first.v != 9 || first.pre != 0 || first.post != 0) bad++;
        $display("%0d", bad);
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("hooks.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// Each object's rand object handles are read once its pre_randomize ran,
/// at every depth, so a member a nested hook allocates gets its
/// pre_randomize, is solved and gets its post_randomize; each object of a
/// cycle is visited once (IEEE 1800-2023 18.6.2).
#[test]
fn pre_randomize_discovers_members_at_every_depth() {
    let source = r#"module Top(output int bad, output bit done);
    class Leaf;
        rand bit [3:0] v;
        int pre, post;
        constraint c { v inside {[3:5]}; }
        function void pre_randomize(); pre++; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    class Node;
        rand Leaf leaf;
        int pre, post;
        function void pre_randomize(); pre++; if (leaf == null) leaf = new; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    class Parent;
        rand Node node;
    endclass
    class Pair;
        rand bit [3:0] w;
        rand Pair other;
        int pre, post;
        constraint k { w == 6; }
        function void pre_randomize(); pre++; endfunction
        function void post_randomize(); post++; endfunction
    endclass
    Parent p;
    Pair a, b;
    initial begin
        p = new;
        p.node = new;
        if (!p.randomize() || p.node.leaf == null || p.node.pre != 1 || p.node.post != 1
            || p.node.leaf.pre != 1 || p.node.leaf.post != 1
            || !(p.node.leaf.v inside {[3:5]})) bad++;
        a = new;
        b = new;
        a.other = b;
        b.other = a;
        if (!a.randomize() || a.w != 6 || b.w != 6 || a.pre != 1 || a.post != 1
            || b.pre != 1 || b.post != 1) bad++;
        $display("%0d", bad);
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("depth.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// A rand array whose rand_mode is off keeps its size and elements (IEEE
/// 1800-2023 18.8), an inline constraint may name its size (18.7), and
/// randomize(null) checks its size constraint against its size and changes
/// nothing (18.11.1); a disabled block names no size (18.9); a selected
/// array is random for the call (18.11); a bounded queue takes at most its bound plus one
/// elements (7.10). Verilator 5.052 resizes an array whose rand_mode is
/// off, ignores the inline size constraint, does not support
/// randomize(null) on a class with a rand container and does not finish
/// randomizing a bounded queue; xezim 0.11.4 grows a bounded queue past its
/// bound.
#[test]
fn rand_array_modes_inline_sizes_and_checks() {
    let source = r#"module Top(output int bad, output bit done);
    class Item;
        rand bit [63:0] value[];
        constraint max_values { value.size() > 0 && value.size() < 10; }
        bit [7:0] other[];
    endclass
    class Plain;
        int unsigned d[];
        bit [3:0] x;
    endclass
    class Bounded;
        rand bit few[$:2];
        constraint c { few.size() >= 0; }
    endclass
    Item item;
    Bounded b;
    initial begin
        item = new;
        b = new;
        for (int i = 0; i < 20; i++) if (b.randomize() == 0 || b.few.size() > 3) bad++;
        item.value = '{7};
        item.value.rand_mode(0);
        if (item.randomize() == 0 || item.value.size() != 1 || item.value[0] != 7) bad++;
        item.value.rand_mode(1);
        if ((item.randomize() with { value.size() == 2; }) == 0 || item.value.size() != 2) bad++;
        // A selected array is random for the call (18.11): at its size, or
        // the size the call's constraints give.
        begin
            Plain plain = new;
            int changed = 0;
            plain.d = new[4];
            for (int i = 0; i < 10; i++) begin
                if (plain.randomize(d) == 0 || plain.d.size() != 4 || plain.x != 0) bad++;
                foreach (plain.d[j]) if (plain.d[j] != 0) changed++;
            end
            if (changed == 0) bad++;
            if ((item.randomize(other) with { other.size() == 3; }) == 0 || item.other.size() != 3)
                bad++;
        end
        // Without its block, no constraint names the size: it is kept.
        item.max_values.constraint_mode(0);
        item.value = new[12];
        if (item.randomize() == 0 || item.value.size() != 12) bad++;
        item.max_values.constraint_mode(1);
        item.value.delete();
        if (item.randomize(null) != 0) bad++;
        item.value = new[3];
        if (item.randomize(null) != 1 || item.value.size() != 3 || item.value[0] != 0) bad++;
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("check.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// A loop over a state array whose elements leave no solution fails the
/// call and changes nothing (IEEE 1800-2023 18.5.8.1, 18.6.1). Verilator
/// 5.052 prints the unsatisfied constraints.
#[test]
fn constraint_loops_without_a_solution() {
    let source = r#"module Top(output int bad, output bit done);
    class Region;
        bit [7:0] first, last;
    endclass
    class Policy;
        rand bit [7:0] start;
        Region in_use[$];
        constraint c { foreach (in_use[i]) !(start >= in_use[i].first && start <= in_use[i].last); }
    endclass
    Policy p;
    Region r;
    initial begin
        p = new;
        r = new;
        r.first = 0;
        r.last = 255;
        p.in_use.push_back(r);
        p.start = 9;
        if (p.randomize() != 0 || p.start != 9) bad++;
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("loops.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// An element of nested loops over one array is the element of the loop
/// whose variable subscripts it (IEEE 1800-2023 18.5.8.1), and a class
/// derived from the class of the elements sees its protected members (8.18);
/// an inline constraint sees the members its caller's class sees and the
/// caller's variables inside a loop (18.7). A member of an element that the
/// call solves through a rand handle is its solver variable (18.4).
#[test]
fn constraint_loops_by_their_variables() {
    let source = r#"module Top(output int bad, output bit done);
    class Region;
        protected bit [7:0] m;
        local bit [7:0] secret;
        rand bit [7:0] pick;
        Region kids[$];
        function new(bit [7:0] value); m = value; secret = value; endfunction
        function int choose();
            return randomize() with { foreach (kids[i]) pick != kids[i].secret; pick < 2; };
        endfunction
    endclass
    class Pairs extends Region;
        rand bit [7:0] v, w;
        bit [7:0] vals[];
        Region rs[$];
        constraint c {
            foreach (vals[i])
                foreach (vals[j])
                    (i == 0 && j == 1) -> v == vals[i] + vals[j];
            foreach (rs[i]) w != rs[i].m;
            w < 3;
        }
        function new(); super.new(0); endfunction
    endclass
    class Child;
        rand bit [3:0] m;
    endclass
    class Holder;
        rand Child child;
        Child items[$];
        constraint c { foreach (items[i]) items[i].m == 5; }
    endclass
    Pairs p;
    Region r;
    Holder h;
    initial begin
        p = new;
        p.vals = '{8'd5, 8'd7};
        r = new(0);
        p.rs.push_back(r);
        r = new(2);
        p.rs.push_back(r);
        for (int i = 0; i < 5; i++)
            if (p.randomize() == 0 || p.v != 12 || p.w != 1) bad++;
        // A loop in a class's own inline constraint reads its local member.
        r.kids.push_back(p.rs[0]);
        for (int i = 0; i < 5; i++)
            if (r.choose() == 0 || r.pick != 1) bad++;
        // An element that is also the rand handle's object: its rand m is
        // solved; another element's m is state.
        h = new;
        h.child = new;
        h.items.push_back(h.child);
        begin
            Child other = new;
            other.m = 5;
            h.items.push_back(other);
            for (int i = 0; i < 5; i++)
                if (h.randomize() == 0 || h.child.m != 5 || other.m != 5) bad++;
            other.m = 4;
            if (h.randomize() != 0 || other.m != 4) bad++;
        end
        // The caller's variable in an inline loop body.
        begin
            bit [7:0] limit = 3;
            for (int i = 0; i < 5; i++)
                if ((p.randomize() with { foreach (vals[i]) w != vals[i] - limit; }) == 0
                    || p.w != 1) bad++;
        end
        done = 1;
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("pairs.sv"))], "Top")
        .build()
        .unwrap();
    let (bad, done) = (sim.signal("bad"), sim.signal("done"));
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(bad), BigUint::from(0u8));
}

/// A call whose random set includes the array of an enabled constraint loop
/// (IEEE 1800-2023 18.11), as `randomize(a)` directly, through a base
/// handle to a derived class with the loop, or with an inline loop, solves
/// the elements of the array with the loop (18.5.8.1).
#[test]
fn constraint_loops_over_selected_arrays_solve_their_elements() {
    let source = r#"module Top(output int ok);
    class B;
        rand bit [3:0] x;
        bit [3:0] a[];
    endclass
    class C extends B;
        constraint k { foreach (a[i]) x != a[i]; }
    endclass
    B b;
    C c;
    initial begin
        c = new;
        c.a = new[4];
        c.x = 5;
        ok = 1;
        repeat (8) begin
            ok &= c.randomize(a) && c.x == 5 && c.a.size() == 4;
            foreach (c.a[i]) ok &= c.a[i] != 5;
            b = c;
            ok &= b.randomize(a);
            foreach (c.a[i]) ok &= c.a[i] != 5;
            ok &= c.randomize(a) with { foreach (a[i]) a[i] < 3; };
            foreach (c.a[i]) ok &= c.a[i] < 3;
        end
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("select.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("ok")), BigUint::from(1u8));
}

#[test]
fn rand_arrays_have_their_elements_constrained() {
    // The elements of a rand array, fixed-size or dynamic, are solver
    // variables of loops and sums over it (IEEE 1800-2023 18.4, 18.5.8.1,
    // 18.5.8.2); a state array's sum is its elements' sum.
    let source = r#"module Top(output int ok, output int fixed_sum, output int state_sum);
    class C;
        rand int f[5];
        rand int s[4];
        rand bit [7:0] d[];
        int w[3] = '{1, 2, 3};
        rand int t;
        constraint cf { foreach (f[i]) f[i] == 5; }
        constraint cs { s.sum() == 5; foreach (s[i]) s[i] inside {[0:5]}; }
        constraint cd { foreach (d[i]) d[i] == i + 1; }
        constraint ct { t == w.sum(); }
    endclass
    C c;
    initial begin
        c = new;
        c.d = new[4];
        ok = 1;
        repeat (8) begin
            ok &= c.randomize();
            ok &= c.s.sum() == 5 && c.d.size() == 4 && c.t == 6;
            foreach (c.d[i]) ok &= c.d[i] == i + 1;
        end
        fixed_sum = c.f.sum();
        state_sum = c.w.sum();
    end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("arrays.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("ok")), BigUint::from(1u8));
    assert_eq!(sim.get(sim.signal("fixed_sum")), BigUint::from(25u8));
    assert_eq!(sim.get(sim.signal("state_sum")), BigUint::from(6u8));
    // A loop over a rand array whose size a constraint names would need
    // its elements before its size: the call faults.
    let source = r#"module Top;
    class C; rand int a[]; constraint k { a.size() == 2; foreach (a[i]) a[i] == 1; } endclass
    C c;
    initial begin c = new; void'(c.randomize()); end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("sized.sv"))], "Top")
        .build()
        .unwrap();
    let error = sim.run_until(0).unwrap_err().to_string();
    assert!(
        error.contains(
            "a constraint on the elements of a random array whose size a constraint names"
        ),
        "{error}"
    );
}

#[test]
fn randomizing_four_state_properties_is_rejected() {
    for (body, want) in [
        (
            "class C; rand logic [3:0] x; endclass C c; initial begin c = new; void'(c.randomize()); end",
            "randomization of a 4-state class property",
        ),
        (
            "class C; rand bit [3:0] x; logic [3:0] y; constraint k { x < y; } endclass C c; initial begin c = new; void'(c.randomize()); end",
            "4-state class property in a constraint",
        ),
        (
            "class C; logic [3:0] x; rand bit [3:0] y; constraint k { y < 5; } endclass C c; initial begin c = new; void'(c.randomize(x)); end",
            "randomization of a 4-state class property",
        ),
    ] {
        let source = format!("module Top; {body} endmodule");
        for four_state in [false, true] {
            let Err(error) =
                Simulator::from_sv_sources(vec![(&source, Path::new("class.sv"))], "Top")
                    .four_state(four_state)
                    .build_cranelift()
            else {
                panic!("{want} is unsupported");
            };
            let error = error.to_string();
            assert!(error.contains(want), "{error}");
        }
    }
}

#[test]
fn misplaced_super_new_static_misuse_and_unimplemented_methods_are_rejected() {
    for (body, want) in [
        (
            "class B; endclass class C extends B; int x; function new(); x = 1; super.new(); endfunction endclass",
            "`super.new` that is not the first statement",
        ),
        (
            "class C; int x; static function int f(); return x; endfunction endclass",
            "non-static property `x` in a static method",
        ),
        (
            "class C; function int g(); return 1; endfunction static function int f(); return g(); endfunction endclass",
            "non-static method `g` called from a static method",
        ),
        (
            "virtual class C; pure virtual function int f(); endclass class D extends C; endclass",
            "does not implement pure virtual method `f`",
        ),
        (
            "virtual class B; pure virtual function int f(); endclass class C extends B; function int f(); return super.f(); endfunction endclass",
            "pure virtual method `f` called through `super`",
        ),
        ("virtual class C; endclass", "object of abstract class `C`"),
        (
            "interface class I; pure virtual function int f(); endclass virtual class B implements I; endclass class C extends B; endclass",
            "does not implement interface class method `f`",
        ),
        (
            "interface class I; pure virtual function int f(); endclass interface class J; pure virtual function void f(); endclass class C implements I, J; function int f(); return 1; endfunction endclass",
            "whose return type differs from its interface class",
        ),
    ] {
        let source = format!("module Top; {body} C c; initial c = new; endmodule");
        let Err(error) =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("class.sv"))], "Top")
                .build_cranelift()
        else {
            panic!("`{body}` is rejected");
        };
        let error = error.to_string();
        assert!(error.contains(want), "{error}");
    }
}

#[test]
fn incompatible_class_handle_assignments_are_rejected() {
    for (statement, want) in [
        ("d = b;", ["handle of class `B`", "class `D`"]),
        ("d = u;", ["handle of class `U`", "class `D`"]),
        ("i = b;", ["handle of class `B`", "class `I`"]),
        ("b = U::new;", ["handle of class `U`", "class `B`"]),
        ("d.next = b;", ["handle of class `B`", "class `D`"]),
        ("d = new b;", ["handle of class `B`", "class `D`"]),
        ("u = new d;", ["handle of class `D`", "class `U`"]),
    ] {
        let source = format!(
            "module Top;
                interface class I; endclass
                class B; endclass
                class D extends B implements I; D next; endclass
                class U; endclass
                B b; D d; U u; I i;
                initial begin d = new; {statement} end
            endmodule"
        );
        let Err(error) =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("class.sv"))], "Top")
                .build_cranelift()
        else {
            panic!("`{statement}` is rejected");
        };
        let error = error.to_string();
        assert!(want.iter().all(|want| error.contains(want)), "{error}");
    }
}

/// The SV frontend is recursive, and one case nests class specializations
/// 64 deep, so the cases build on a thread with room on its stack (like
/// the vtest executor runs suite cases).
#[test]
fn misused_parameterized_classes_are_rejected() {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, misused_parameterized_classes)
            .unwrap()
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}

fn misused_parameterized_classes() {
    for (items, want) in [
        (
            "Box #(2) two; Box #(3) three; initial begin three = new; two = three; end",
            ["handle of class `Box#(N=3)`", "class `Box#(N=2)`"],
        ),
        (
            "initial $display(Box::N);",
            ["`Box::` outside class `Box`", "`Box#()::`"],
        ),
        (
            "Box #(.M(1)) other;",
            ["class `Box` has no parameter `M`", ""],
        ),
        (
            "interface class Get #(type T = int); pure virtual function T get(); endclass
             interface class Both extends Get#(int), Get#(bit); endclass",
            ["interface class `Both`", "inherits `T` from both"],
        ),
        (
            "class Self #(int N = 0) extends Self#(N + 1); endclass Self self;",
            ["nested more than 64 deep", "class `Self`"],
        ),
        (
            "class Sized #(parameter V = 1); endclass Sized #(8'h1) a; Sized #(32'h1) b;
             initial begin b = new; a = b; end",
            ["class `Sized#(V=32'd1)`", "class `Sized#(V=8'd1)`"],
        ),
        (
            "Box #(.N(3), 4) mixed;",
            ["positional parameter value after a named one", ""],
        ),
        (
            "class Point; function new(int x = 0); endfunction endclass Point p;
             initial p = new(.x(), .x(1));",
            ["formal `x` is bound twice", "13.5.4"],
        ),
        (
            "Box #(.N(), .N(2)) twice;",
            ["duplicate parameter assignment `N`", ""],
        ),
        (
            "class Plain; typedef int T; endclass Plain#(123)::T value;",
            ["class `Plain`, which has no parameters", ""],
        ),
        (
            "class Typed #(type T = int); endclass Typed#(enum {A, B}) typed;",
            ["enumeration type as a class type parameter value", ""],
        ),
        (
            "class Typed #(type T = int); endclass Typed#(struct packed { int x; }) typed;",
            ["struct or union type as a class type parameter value", ""],
        ),
        (
            // Each iteration would declare a class of its own (#2064).
            "for (genvar i = 0; i < 2; i++) begin : g
                 localparam int K = i + 1;
                 class C #(int N = K); endclass
                 initial $display(\"%0d\", C#()::N);
             end",
            ["class `C` declared in a loop generate block", ""],
        ),
        (
            // A named four-state constant of the module is not yet an
            // actual (#1964); a class passes its own such parameters on.
            "localparam logic [1:0] X = 2'bxx; class Four #(logic [1:0] N = 0); endclass
             Four#(X) four;",
            ["class parameter value that is not constant", ""],
        ),
    ] {
        let source = format!(
            "module Top;
                class Box #(int N = 4); endclass
                {items}
            endmodule"
        );
        let Err(error) =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("class.sv"))], "Top")
                .build_cranelift()
        else {
            panic!("`{items}` is rejected");
        };
        let error = error.to_string();
        assert!(want.iter().all(|want| error.contains(want)), "{error}");
    }
}

/// Calls whose actuals cannot be bound to the formals are compile errors
/// (IEEE 1800-2023 13.5.3, 13.5.4), for methods as for other subroutines.
#[test]
fn unbindable_call_arguments_are_rejected() {
    for (call, want) in [
        ("c.f(.a(1), .a(2))", "formal `a` is bound twice"),
        ("c.f(1, .a(2))", "formal `a` is bound twice"),
        ("c.f(.q(1))", "no formal named `q`"),
        ("c.f(.b(1), 3)", "a positional actual after a named one"),
        ("c.f(1, 2, 3)", "3 actuals for 2 formals"),
        ("c.f(.b(1))", "formal `a` has no default"),
        ("c.f(, 1)", "formal `a` has no default"),
        ("c.f(.a(), .b(1))", "formal `a` has no default"),
        ("C::s(.z(1))", "no formal named `z`"),
        ("f(.a(1), .a(2))", "formal `a` is bound twice"),
        ("f(.z(1))", "no formal named `z`"),
        ("f(.b(1), 2)", "a positional actual after a named one"),
        ("f(.b(1))", "formal `a` has no default"),
        ("f(1, 2, 3)", "3 actuals for 2 formals"),
    ] {
        let source = format!(
            "module Top;
                class C;
                    function int f(int a, int b = 2); return a + b; endfunction
                    static function int s(int a = 1); return a; endfunction
                endclass
                function automatic int f(int a, int b = 2); return a + b; endfunction
                C c = new;
                initial $display(\"%0d\", {call});
            endmodule"
        );
        let Err(error) =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("call.sv"))], "Top")
                .build_cranelift()
        else {
            panic!("`{call}` is accepted");
        };
        let error = error.to_string();
        assert!(error.contains(want), "{call}: {error}");
    }
}

sv_backends! {
    // set_randstate with a string that get_randstate did not return is
    // undefined (IEEE 1800-2023 18.13.5); Celox ends the simulation with an
    // error.
    fn class_set_randstate_rejects_a_foreign_state(sim) {
        @setup {
            let source = r#"module Top(output int started);
                class Item; rand int x; endclass
                Item item = new;
                initial begin
                    started = 1;
                    item.set_randstate("not a state");
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("set_randstate.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("set_randstate with a state that get_randstate did not return"), "{error}");
        assert_eq!(sim.get(sim.signal("started")), 1u8.into());
    }
}

sv_backends! {
    fn class_randomize_null_receiver(sim) {
        @setup {
            let source = r#"module Top(output int hooks, result);
                class Empty;
                    static int calls;
                    function void pre_randomize(); calls++; endfunction
                    function void post_randomize(); calls++; endfunction
                endclass
                Empty object;
                initial begin
                    result = object.randomize();
                    hooks = Empty::calls;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("null_randomize.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("null object handle"), "{error}");
        assert_eq!(sim.get(sim.signal("hooks")), 0u8.into());
        assert_eq!(sim.get(sim.signal("result")), 0u8.into());
    }
}

sv_backends! {
    fn class_randomize_rejects_unsupported_dynamic_constraints(sim) {
        @setup {
            let source = r#"module Top(output int value, hooks, result);
                class Base;
                    rand int x = 77;
                    static int calls;
                    function void pre_randomize(); calls++; endfunction
                    function void post_randomize(); calls++; endfunction
                endclass
                class Derived extends Base;
                    constraint c { (x == 1) <-> (x < 3); }
                endclass
                Base b;
                Derived d;
                initial begin
                    d = new;
                    b = d;
                    result = b.randomize();
                    value = b.x;
                    hooks = Base::calls;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("base_randomize.sv"))], "Top");
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("randomization through a base handle"), "{error}");
        assert_eq!(sim.get(sim.signal("value")), 77u8.into());
        assert_eq!(sim.get(sim.signal("hooks")), 0u8.into());
        assert_eq!(sim.get(sim.signal("result")), 0u8.into());
    }
}

sv_backends! {
    fn class_randomize_checkpoint_four_state(sim) {
        @setup {
            let source = r#"module Top(output bit [63:0] value);
                timeunit 1ps / 1ps;
                class C; rand bit [63:0] x; endclass
                C c;
                initial begin
                    c = new;
                    c.srandom(44);
                    void'(c.randomize());
                    value = c.x;
                    #10;
                    void'(c.randomize());
                    value = c.x;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("random_checkpoint.sv"))], "Top").four_state(true);
        sim.run_until(5).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        let value = sim.signal("value");
        sim.run_until(15).unwrap();
        let expected = sim.get_four_state(value);
        sim.restore(&checkpoint).unwrap();
        sim.run_until(15).unwrap();
        assert_eq!(sim.get_four_state(value), expected);
    }
}

sv_backends! {
    fn class_constructor_checkpoint_restores_process_rng(sim) {
        @setup { let source = r#"
            module Top(input bit clk, output bit [63:0] value);
                class C; rand bit [63:0] x; endclass
                C c;
                always @(posedge clk) begin
                    c = new;
                    void'(c.randomize());
                    value = c.x;
                end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("constructor_rng.sv"))], "Top").four_state(true);
        let clk = sim.event("clk");
        let value = sim.signal("value");
        sim.tick(clk).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        let mut expected = Vec::new();
        for _ in 0..4 {
            sim.tick(clk).unwrap();
            expected.push(sim.get_four_state(value));
        }
        assert!(expected.windows(2).any(|pair| pair[0] != pair[1]));
        sim.restore(&checkpoint).unwrap();
        for expected in expected {
            sim.tick(clk).unwrap();
            assert_eq!(sim.get_four_state(value), expected);
        }
    }
}

sv_backends! {
    fn class_random_modes_checkpoint_four_state(sim) {
        @setup { let source = r#"
            module Top(input bit enable, output int mode, output bit [63:0] value);
                timeunit 1ps / 1ps;
                class C; rand bit [63:0] x = 17; endclass
                C c;
                initial begin
                    c = new;
                    c.x.rand_mode(0);
                    #10 if (enable) c.x.rand_mode(1);
                    #10;
                    void'(c.randomize());
                    value = c.x;
                    mode = c.x.rand_mode();
                end
            endmodule
        "#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("mode_checkpoint.sv"))], "Top").four_state(true);
        sim.run_until(5).unwrap();
        let saved = sim.checkpoint().unwrap();
        let enable = sim.signal("enable");
        sim.modify(|io| io.set(enable, 1u8)).unwrap();
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(sim.signal("mode")), 1u8.into());
        sim.restore(&saved).unwrap();
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(sim.signal("mode")), 0u8.into());
        assert_eq!(sim.get_four_state(sim.signal("value")), (17u8.into(), 0u8.into()));
    }

    fn class_random_selection_spans_multiple_words(sim) {
        @setup {
            let properties = (0..70).map(|i| format!("p{i}")).collect::<Vec<_>>().join(", ");
            let source = format!(r#"
                module Top(output bit done, output int errors);
                    class C;
                        bit [63:0] {properties};
                    endclass
                    C c, reference;
                    initial begin
                        c = new; reference = new;
                        c.srandom(71); reference.srandom(71);
                        if (!c.randomize(p69) || !reference.randomize(p0)) errors++;
                        if (c.p69 != reference.p0 || c.p0 != 0 || c.p68 != 0) errors++;
                        done = 1;
                    end
                endmodule
            "#);
        }
        @build celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("wide_selection.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("done")), 1u8.into());
        assert_eq!(sim.get(sim.signal("errors")), 0u8.into());
    }
}

#[test]
fn classes_of_packages_in_other_sources_are_used_through_their_layouts() {
    let package = "package shapes;
        class Shape;
            int id;
            static int made;
            function new(int id); this.id = id; made++; endfunction
            virtual function int area(); return 0; endfunction
        endclass
        class Square extends Shape;
            int side;
            function new(int side); super.new(side + 1); this.side = side; endfunction
            virtual function int area(); return side * side; endfunction
        endclass
    endpackage";
    let module = "module Top(output int area, output int id, output int made);
        import shapes::*;
        Shape shape;
        initial begin
            shape = Square::new(4);
            area = shape.area();
            id = shape.id;
            made = shapes::Shape::made;
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (package, Path::new("shapes.sv")),
            (module, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("area")), 16u32.into());
    assert_eq!(sim.get(sim.signal("id")), 5u32.into());
    assert_eq!(sim.get(sim.signal("made")), 1u32.into());
}

#[test]
fn compilation_unit_types_of_separate_files_specialize_package_classes_separately() {
    // Celox makes each source file a compilation unit of its own (IEEE
    // 1800-2023 3.12.1 lets a tool choose this), so the same typedef in two
    // files names two types and two specializations of `p::Counter`.
    let package = "`timescale 1ns/1ns
    package p;
        class Counter #(type T = int);
            static int count;
            function new(); count++; endfunction
        endclass
    endpackage";
    let a = "`timescale 1ns/1ns
    typedef enum {A0, A1} E;
    module A(output int n);
        initial begin
            automatic p::Counter#(E) c = new();
            #1 n = p::Counter#(E)::count;
        end
    endmodule";
    let b = "`timescale 1ns/1ns
    typedef enum {B0, B1} E;
    module B(output int n);
        initial begin
            automatic p::Counter#(E) c = new();
            automatic p::Counter#(E) d = new();
            #1 n = p::Counter#(E)::count;
        end
    endmodule";
    let top = "`timescale 1ns/1ns
    module Top(output int a, output int b);
        A ua(.n(a));
        B ub(.n(b));
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (package, Path::new("package.sv")),
            (a, Path::new("a.sv")),
            (b, Path::new("b.sv")),
            (top, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("a")), 1u32.into());
    assert_eq!(sim.get(sim.signal("b")), 2u32.into());
}

#[test]
fn a_specialization_asked_of_a_package_runs_with_the_classes_it_does_not_change() {
    // The package is analyzed again for `p::C#(5)`. The classes that the
    // specialization does not change keep what their first analysis made:
    // `D::g` reaches the new class through the dispatcher of `B::f`, and
    // the static locals of `B::twice` and `K::tick` are the same variables
    // (IEEE 1800-2023 8.20, 8.25).
    let source = "`timescale 1ns/1ns
    package p;
        int count;
        class B;
            int x;
            virtual function int f(); return x; endfunction
            function int twice(); static int calls; calls++; return 2 * f() + calls; endfunction
        endclass
        class C #(int N = 2) extends B;
            virtual function int f(); count++; return N + x; endfunction
        endclass
        class D;
            B b;
            function int g(); int local_value; local_value = b.twice(); return local_value; endfunction
        endclass
        class K;
            static function int tick(); static int ticks; ticks++; return ticks; endfunction
        endclass
    endpackage
    module Top(output int o, output int n, output int t);
        p::C#(5) c;
        p::D d;
        initial begin
            c = new; d = new; d.b = c; c.x = 1;
            o = d.g();
            o = o + d.g();
            n = p::count;
            t = p::K::tick() + p::K::tick();
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("o")), 27u32.into());
    assert_eq!(sim.get(sim.signal("n")), 2u32.into());
    assert_eq!(sim.get(sim.signal("t")), 3u32.into());
}

#[test]
fn a_specialization_asked_of_a_package_keeps_its_static_formals() {
    // The formals of a static task of a package that suspends are package
    // variables (IEEE 1800-2023 13.3.1, 13.3.2): every analysis of the
    // package, including the one made again for `p::C#(5)`, names the
    // same ones.
    let source = "`timescale 1ns/1ns
    package p;
        int done;
        task settle(int value); #1 done = value; endtask
        class B; virtual function int f(); return 1; endfunction endclass
        class C #(int N = 2) extends B;
            virtual function int f(); return N; endfunction
        endclass
    endpackage
    module Top(output int o, output int d);
        p::C#(5) c;
        initial begin c = new; o = c.f(); p::settle(7); d = p::done; end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(2).unwrap();
    assert_eq!(sim.get(sim.signal("o")), 5u32.into());
    assert_eq!(sim.get(sim.signal("d")), 7u32.into());
}

#[test]
fn methods_a_package_specialization_lowers_share_their_static_variables() {
    // `Seq#(Item)` makes the package lower `Item::op` for its dispatcher.
    // The static variable of that method is one variable of the
    // compilation unit, which `Item` declares, whichever scope calls it
    // (IEEE 1800-2023 6.21, 8.25).
    let source = "`timescale 1ns/1ns
    package p;
        class Base; virtual function void op(); endfunction endclass
        class Seq #(type T = Base);
            T item;
            function void run(); item.op(); endfunction
        endclass
    endpackage
    import p::*;
    class Item extends Base;
        int count, calls;
        virtual function void op();
            calls++;
            begin
                static bit warned;
                if (!warned) begin count++; warned = 1; end
            end
        endfunction
    endclass
    module Top(output int count, output int calls, output int other_count, output int other_calls);
        Seq #(Item) s;
        Item other;
        initial begin
            s = new; s.item = new; other = new;
            s.run(); s.run(); other.op();
            count = s.item.count; calls = s.item.calls;
            other_count = other.count; other_calls = other.calls;
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("count")), 1u32.into());
    assert_eq!(sim.get(sim.signal("calls")), 2u32.into());
    assert_eq!(sim.get(sim.signal("other_count")), 0u32.into());
    assert_eq!(sim.get(sim.signal("other_calls")), 1u32.into());
}

#[test]
fn a_package_specialization_runs_methods_of_the_classes_it_takes() {
    // `Registry#(Item)` makes the package run `Item::code` through the
    // dispatcher of `Obj::code` in the initializer of a static property.
    // The locals of the method are its own, not the package's: `keep`
    // holds its `v` across the call, and the method writes its `v`
    // (IEEE 1800-2023 8.25, 13.4.2).
    let source = "`timescale 1ns/1ns
    package p;
        class Obj; virtual function int code(int x); return x; endfunction endclass
        function automatic int keep(Obj o);
            int v;
            v = 100;
            v = v + o.code(1);
            return v;
        endfunction
        class Registry #(type T = Obj);
            static int registered = register();
            static function int register(); T t = new; return keep(t); endfunction
        endclass
    endpackage
    import p::*;
    class Item extends Obj;
        virtual function int code(int x); int v; v = x * 7; return v; endfunction
    endclass
    module Top(output int o);
        typedef Registry#(Item) R;
        initial o = R::registered;
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("o")), 107u32.into());
}

#[test]
fn package_classes_name_the_compilation_unit_classes_of_other_files() {
    // `$typename` of a type parameter of a package class, specialized with
    // a class of the compilation unit of another file, as
    // `uvm_resource_db#(C)` is (IEEE 1800-2023 20.6.1, 3.12.1).
    let package = "package p;
        class R #(type T = int);
            static function string name(); return $typename(T); endfunction
        endclass
    endpackage";
    let top = "class C; endclass
    module Top(output bit named);
        initial named = p::R#(C)::name() == \"$unit::C\";
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(package, Path::new("p.sv")), (top, Path::new("top.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("named")), 1u32.into());
}

#[test]
fn constraints_name_members_of_objects_their_handles_refer_to() {
    // A constraint names a member of the object a handle property refers
    // to (IEEE 1800-2023 18.5.9): through a rand handle, a variable solved
    // with the object's; through another, a state value.
    let source = "class A;
        rand int v;
        constraint r { v inside {[0:9]}; }
    endclass
    class B extends A;
        rand A aObj;
        A s;
        rand int w;
        constraint c { aObj.v < v; w == s.v + 1; }
        function new(); aObj = new; s = new; endfunction
    endclass
    module Top;
        B b;
        int ok;
        initial begin
            b = new;
            b.s.v = 41;
            ok = 1;
            repeat (20) ok &= b.randomize() && b.aObj.v < b.v && b.aObj.v >= 0 && b.w == 42 && b.s.v == 41;
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1"]);
}

#[test]
fn constraints_name_a_member_size_of_the_object_a_handle_refers_to() {
    // `h.size` is a member of the object handle `h` refers to, by the type
    // of `h`, not the size of an array (IEEE 1800-2023 18.5.9).
    let source = "class A; rand int size; endclass
    class B;
        rand A h;
        constraint c { h.size == 3; }
        function new(); h = new; endfunction
    endclass
    module Top;
        B b;
        int ok;
        initial begin
            b = new;
            ok = b.randomize() && b.h.size == 3;
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1"]);
}

#[test]
fn guards_keep_constraints_from_members_of_null_handles() {
    // A guard of state variables is evaluated before the constraints are
    // solved (IEEE 1800-2023 18.5.13): a constraint it leaves out does not
    // read the member of a null handle, under `if` or `->`.
    let source = "class A; int v; endclass
    class B;
        bit enabled;
        A h;
        rand int x, y;
        constraint c { if (enabled) h.v == x; enabled -> h.v == y; }
    endclass
    module Top;
        B b;
        int ok, off, on;
        initial begin
            b = new;
            off = b.randomize();
            b.h = new;
            b.h.v = 7;
            b.enabled = 1;
            on = b.randomize() && b.x == 7 && b.y == 7;
            $display(\"%0d %0d\", off, on);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 1"]);
}

#[test]
fn guards_read_fixed_members_of_objects_the_call_solves() {
    // A guard that reads a state member, or a rand member whose rand_mode is
    // off, through a rand handle is evaluated before solving too (IEEE
    // 1800-2023 18.5.13, 18.8): the call solves that object, but the member
    // keeps its value, so a false guard leaves out the member of a null
    // handle.
    let source = "class H; bit enabled; rand int w; endclass
    class G; rand int v; endclass
    class B;
        rand H h;
        rand G g;
        constraint c { if (h.enabled) g.v == 1; h.w > 0 -> g.v == 2; }
        function new(); h = new; endfunction
    endclass
    module Top;
        B b;
        int off, on;
        initial begin
            b = new;
            b.h.w = 0;
            b.h.w.rand_mode(0);
            off = b.randomize();
            b.g = new;
            b.h.enabled = 1;
            on = b.randomize() && b.g.v == 1;
            $display(\"%0d %0d\", off, on);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 1"]);
}

#[test]
fn guards_of_any_state_expression_are_known_before_solving() {
    // A guard is known before solving whatever operators it applies to
    // state values, a shift or a state array's size among them (IEEE
    // 1800-2023 18.5.13): a false one leaves out the member of a null
    // handle.
    let source = "class A; int v; endclass
    class B;
        bit [3:0] bits;
        int arr[];
        A h;
        rand int x;
        constraint c {
            if (bits << 1) h.v == x;
            if ((bits >> 3) == 1) h.v == x + 1;
            if (arr.size() == 0) h.v == x + 2;
        }
    endclass
    module Top;
        B b;
        int off, on;
        initial begin
            b = new;
            b.arr = new[2];
            off = b.randomize();
            b.h = new;
            b.h.v = 9;
            b.bits = 4'b0100;
            on = b.randomize() && b.x == 9;
            $display(\"%0d %0d\", off, on);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 1"]);
}

#[test]
fn handle_members_named_like_array_methods_are_members() {
    // `h.sum` and `h.size` name the members of the object `h` refers to,
    // as `h` is a class handle, not an array (IEEE 1800-2023 7.12, 18.5.9).
    let source = "class A; rand int sum; rand int size; endclass
    class B;
        rand A h;
        constraint c { h.sum == 3; h.size == h.sum + 1; }
        function new(); h = new; endfunction
    endclass
    module Top;
        B b;
        initial begin
            b = new;
            $display(\"%0d %0d %0d\", b.randomize(), b.h.sum, b.h.size);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 3 4"]);
}

#[test]
fn constraints_name_members_only_through_handles_they_may_access() {
    // The handle of `h.m` is a member the constraint accesses: a `local`
    // one of a base class is not visible in a derived class's constraint
    // (IEEE 1800-2023 8.18, 18.5.9).
    let source = "class A;
        rand int v;
    endclass
    class Base;
        local rand A h;
        function new(); h = new; endfunction
    endclass
    class Derived extends Base;
        constraint c { h.v == 1; }
    endclass
    module Top; Derived d; initial begin d = new; void'(d.randomize()); end endmodule";
    let error =
        match celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
            .build()
        {
            Err(error) => error.to_string(),
            Ok(mut sim) => sim
                .run_until(1)
                .expect_err("a base class's local handle")
                .to_string(),
        };
    assert!(error.contains("local member `h` of class"), "{error}");
}

#[test]
fn uniqueness_constraints_keep_their_members_apart() {
    // `unique` gives its members pairwise different values, with other
    // constraints; a group of fewer than two members has no effect, and a
    // group with no different values left fails the call (IEEE 1800-2023
    // 18.5.4).
    let source = "class C;
        rand bit [1:0] a, b, c;
        rand int x;
        constraint k { unique {a, b, c}; a != 3; b != 3; c != 3; unique {x}; }
    endclass
    module Top;
        C o;
        int ok, failed;
        initial begin
            o = new;
            ok = 1;
            repeat (50) ok &= o.randomize() && o.a != o.b && o.b != o.c && o.a != o.c;
            failed = o.randomize() with { a == b; };
            $display(\"%0d %0d\", ok, failed);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 0"]);
    for (members, constraint, message) in [
        (
            "randc bit [1:0] a; rand bit [1:0] b;",
            "unique {a, b};",
            "a randc variable in a uniqueness constraint",
        ),
        (
            "rand bit [1:0] a; rand int b;",
            "unique {a, b};",
            "members of types that are not equivalent",
        ),
        (
            "rand int a; rand int q[];",
            "unique {a, q};",
            "an array in a uniqueness constraint",
        ),
    ] {
        let error = uniqueness_error(members, constraint);
        assert!(error.contains(message), "{constraint}: {error}");
    }
}

/// The error of randomizing a class with `members` and `constraint`.
fn uniqueness_error(members: &str, constraint: &str) -> String {
    let source = format!(
        "class C; {members} constraint k {{ {constraint} }} endclass
        module Top; C c; initial begin c = new; void'(c.randomize()); end endmodule"
    );
    match celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("class.sv"))], "Top")
        .build()
    {
        Err(error) => error.to_string(),
        Ok(mut sim) => sim.run_until(1).expect_err(constraint).to_string(),
    }
}

#[test]
fn uniqueness_constraint_members_are_variables() {
    // A member is a variable, not a value computed from one (IEEE 1800-2023
    // 18.5.4).
    let error = uniqueness_error("rand int a, b;", "unique {a + 1, b};");
    assert!(
        error.contains("a uniqueness constraint member that is not a variable"),
        "{error}"
    );
}

#[test]
fn uniqueness_constraint_members_are_of_equivalent_types() {
    // Two enumerations of one base type are not equivalent (IEEE 1800-2023
    // 6.22.2, 18.5.4).
    let error = uniqueness_error(
        "typedef enum bit [1:0] {A0, A1} ea; typedef enum bit [1:0] {B0, B1} eb; rand ea a; rand eb b;",
        "unique {a, b};",
    );
    assert!(
        error.contains("members of types that are not equivalent"),
        "{error}"
    );
}

#[test]
fn uniqueness_constraints_of_many_members_stay_solvable() {
    // 33 members make 528 pairwise differences, which the solver takes as
    // a balanced conjunction (IEEE 1800-2023 18.5.4).
    let names: Vec<String> = (0..33).map(|index| format!("m{index}")).collect();
    let source = format!(
        "class C;
        rand bit [7:0] {};
        constraint k {{ unique {{{}}}; }}
    endclass
    module Top;
        C o;
        int ok, apart;
        initial begin
            o = new;
            ok = o.randomize();
            apart = 1;
            {}
            $display(\"%0d %0d\", ok, apart);
        end
    endmodule",
        names.join(", "),
        names.join(", "),
        (0..33)
            .flat_map(|a| ((a + 1)..33).map(move |b| format!("apart &= o.m{a} != o.m{b};")))
            .collect::<Vec<_>>()
            .join(" "),
    );
    assert_eq!(super::hierarchical_references::displayed(&source), ["1 1"]);
}

#[test]
fn distributions_constrain_values_and_weigh_their_items() {
    // `dist` allows the values of its items of nonzero total weight, weighs
    // a range by its values with `:=` and as a whole with `:/`, and its
    // weights never make a call fail; with the class's, an inline one that
    // leaves no value fails the call (IEEE 1800-2023 18.5.3, 18.7).
    let source = "class C;
        rand int b;
        rand bit [3:0] r;
        constraint c { b dist {3 := 0, 10 := 5}; }
        constraint d { r dist {[0:3] :/ 1, [4:15] := 0, 15 := 3}; }
    endclass
    module Top;
        C c;
        int ok, fifteen, low, failed;
        initial begin
            c = new;
            ok = 1;
            repeat (200) begin
                ok &= c.randomize() && c.b == 10 && (c.r <= 3 || c.r == 15);
                if (c.r == 15) fifteen++; else low++;
            end
            $display(\"%0d %0d %0d\", ok, fifteen > 120, low > 20);
            ok = c.randomize() with { b dist {10 := 1, 3 := 9}; };
            failed = c.randomize() with { b dist {3 := 1, 10 := 0}; };
            $display(\"%0d %0d %0d\", ok, c.b, failed);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["1 1 1", "1 10 0"]
    );
    let source = "class C;
        rand int b;
        int w = 2;
        constraint c { b dist {10 := w}; }
    endclass
    module Top; C c; initial begin c = new; void'(c.randomize()); end endmodule";
    let error =
        match celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
            .build()
        {
            Err(error) => error.to_string(),
            Ok(mut sim) => sim
                .run_until(1)
                .expect_err("a weight that is no constant")
                .to_string(),
        };
    assert!(
        error.contains("distribution weight other than a constant"),
        "{error}"
    );
    // However the operand names the randc property.
    for operand in ["b", "(b)"] {
        let source = format!(
            "class C;
                randc bit [3:0] b;
                constraint c {{ {operand} dist {{3 := 0, 10 := 5}}; }}
            endclass
            module Top; C c; initial begin c = new; void'(c.randomize()); end endmodule"
        );
        let error = match celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build()
        {
            Err(error) => error.to_string(),
            Ok(mut sim) => sim.run_until(1).expect_err(operand).to_string(),
        };
        assert!(
            error.contains("distribution of a randc variable"),
            "{operand}: {error}"
        );
    }
    // A soft distribution, the only constraint of its block, keeps the block
    // unsupported rather than taking another expression as its operand.
    let source = "class C;
        rand int b;
        constraint c { soft b dist {10 := 1}; }
    endclass
    module Top; C c; initial begin c = new; void'(c.randomize()); end endmodule";
    let error =
        match celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
            .build()
        {
            Err(error) => error.to_string(),
            Ok(mut sim) => sim
                .run_until(1)
                .expect_err("a soft distribution")
                .to_string(),
        };
    assert!(error.contains("constraint block form"), "{error}");
}

#[test]
fn distribution_ranges_count_their_values_in_their_type() {
    // `:=` counts the values of a range in the type the items compare in:
    // [LOW:2] of a signed `int` with LOW = -2 holds five values, as many as
    // the other item weighs (IEEE 1800-2023 18.5.3).
    let source = "class C;
        localparam int LOW = -2;
        rand int b;
        constraint c { b dist {[LOW:2] := 1, 100 := 5}; }
    endclass
    module Top;
        C c;
        int ok, range, hundred;
        initial begin
            c = new;
            ok = 1;
            repeat (200) begin
                ok &= c.randomize() && (c.b inside {[-2:2], 100});
                if (c.b == 100) hundred++; else range++;
            end
            $display(\"%0d %0d %0d\", ok, range > 60, hundred > 60);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 1 1"]);
}

#[test]
fn distribution_weights_and_bounds_are_constant_expressions() {
    // A weight, and a bound of a range weighted per value, is any constant
    // expression (IEEE 1800-2023 18.5.3).
    let source = "class C;
        rand int e;
        constraint d { e dist {1 := 2 + 3, [1 + 2:5] := 1}; }
    endclass
    module Top;
        C c;
        int ok, one, three;
        initial begin
            c = new;
            ok = 1;
            repeat (200) begin
                ok &= c.randomize() && (c.e inside {1, [3:5]});
                if (c.e == 1) one++; else three++;
            end
            $display(\"%0d %0d %0d\", ok, one > 60, three > 40);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 1 1"]);
}

#[test]
fn distribution_ranges_may_be_open_ended() {
    // `$` bounds a range by the lowest or highest value of the type of the
    // expression (IEEE 1800-2023 11.4.13, 18.5.3): [$:-3] of an `int` is
    // every value up to -3, [250:$] of an 8-bit unsigned one 250 to 255.
    let source = "class C;
        rand int b;
        rand bit [7:0] u;
        constraint c { b dist {[$:-3] :/ 1, [10:12] :/ 1}; }
        constraint d { u dist {[250:$] := 1, [0:5] := 1}; }
    endclass
    module Top;
        C c;
        int ok, low, high;
        initial begin
            c = new;
            ok = 1;
            repeat (200) begin
                ok &= c.randomize() && (c.b <= -3 || c.b inside {[10:12]})
                    && (c.u >= 250 || c.u <= 5);
                if (c.b <= -3) low++;
                if (c.u >= 250) high++;
            end
            $display(\"%0d %0d %0d\", ok, low > 60, high > 60);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 1 1"]);
}

#[test]
fn distribution_weights_keep_their_width() {
    // A weight is the full value of its constant expression: a 64-bit one
    // of 2^32 is not 0 (IEEE 1800-2023 18.5.3), so its item stays allowed,
    // also from a constant function.
    let source = "package P;
        function automatic longint unsigned big(); return 64'h1_0000_0000; endfunction
    endpackage
    class C;
        rand int b;
        constraint c { b dist {7 := 64'h3_0000_0000 / 3, 7 := P::big()}; }
    endclass
    module Top;
        C c;
        int ok;
        initial begin
            c = new;
            ok = c.randomize() && c.b == 7;
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1"]);
}

#[test]
fn distribution_range_totals_keep_their_ratio_beyond_64_bits() {
    // A `:=` range weighs its weight times its size (IEEE 1800-2023
    // 18.5.3): three values of weight 2^63 against one of 2^63 are tried
    // 3:1, a total past 64 bits kept.
    let source = "class C;
        rand bit [1:0] x;
        constraint c { x dist {[0:2] := 64'h8000_0000_0000_0000, 3 := 64'h8000_0000_0000_0000}; }
    endclass
    module Top;
        C c;
        int ok, three;
        initial begin
            c = new;
            ok = 1;
            repeat (1000) begin
                ok &= c.randomize();
                if (c.x == 3) three++;
            end
            $display(\"%0d %0d\", ok, three > 200 && three < 290);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 1"]);
}

#[test]
fn distribution_weights_keep_every_bit() {
    // A weight is an unsigned value of any width (IEEE 1800-2023 18.5.3):
    // a range of three values of weight W = 2^127 - 1 totals past 128 bits
    // and is tried 3:2 against one value of weight 2W, so 3 is picked about
    // 40% of the time, not the 25% of a pick by value; and 2^127 from an
    // unsigned 128-bit constant function stays 2^127, about 1:1 against
    // 2^127 - 1.
    let source = "package P;
        function automatic bit [127:0] top(); return 128'h8000_0000_0000_0000_0000_0000_0000_0000; endfunction
    endpackage
    class C;
        rand bit [1:0] x;
        rand bit y;
        constraint c {
            x dist {[0:2] := 128'h7fff_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 3 := 129'h0_ffff_ffff_ffff_ffff_ffff_ffff_ffff_fffe};
            y dist {0 := P::top(), 1 := 128'h7fff_ffff_ffff_ffff_ffff_ffff_ffff_ffff};
        }
    endclass
    module Top;
        C c;
        int ok, three, zero;
        initial begin
            c = new;
            ok = 1;
            repeat (1000) begin
                ok &= c.randomize();
                if (c.x == 3) three++;
                if (!c.y) zero++;
            end
            $display(\"%0d %0d %0d\", ok, three > 340 && three < 460, zero > 430 && zero < 570);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 1 1"]);
}

#[test]
fn distributions_of_array_sums_solve_the_elements() {
    // The sum of a random array as the operand of a distribution makes its
    // elements solver variables (IEEE 1800-2023 18.5.3, 18.5.8.2), so the
    // sum, of the elements' 4 bits, takes a value of an item.
    let source = "class C;
        rand bit [3:0] a[2];
        constraint c { a.sum() dist {[5:6] :/ 1}; }
    endclass
    module Top;
        C c;
        int ok;
        initial begin
            c = new;
            ok = 1;
            repeat (20) ok &= c.randomize() && c.a.sum() inside {[5:6]};
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1"]);
}

#[test]
fn inline_distributions_take_open_ranges() {
    // `$`, an open bound of a range of an inline distribution (IEEE
    // 1800-2023 11.4.13, 18.7), names no variable of the caller.
    let source = "class C; rand int b; endclass
    module Top;
        C c;
        int ok;
        initial begin
            c = new;
            ok = 1;
            repeat (20) ok &= c.randomize() with { b dist {[$:3] :/ 1}; } && c.b <= 3;
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1"]);
}

#[test]
fn negative_distribution_weights_are_unsigned() {
    // A weight is read as unsigned at the width of its type (IEEE 1800-2023
    // 18.5.3): -1 of an `int` constant function is 2^32 - 1, which outweighs
    // a weight of 1.
    let source = "package P;
        function automatic int neg(); return -1; endfunction
    endpackage
    class C;
        rand bit x;
        constraint c { x dist {0 := P::neg(), 1 := 1}; }
    endclass
    module Top;
        C c;
        int ok, ones;
        initial begin
            c = new;
            ok = 1;
            repeat (100) begin
                ok &= c.randomize();
                if (c.x) ones++;
            end
            $display(\"%0d %0d\", ok, ones);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 0"]);
}

#[test]
fn distributions_under_conditions_and_loops_weigh_their_items() {
    // The weights of a distribution under `if`/`else` or `->`, also in a
    // `foreach`, apply where its condition does (IEEE 1800-2023 18.5.3,
    // 18.5.6, 18.5.7, 18.5.8.1).
    let source = "class C;
        rand int b;
        rand int e;
        bit mode;
        int sel[];
        constraint c { if (mode) b dist {10 := 1, 20 := 9}; else b dist {10 := 9, 20 := 1}; }
        constraint d { foreach (sel[i]) sel[i] == 1 -> e dist {30 := 9, 40 := 1}; }
    endclass
    module Top;
        C c;
        int ok, twenty, ten, thirty;
        initial begin
            c = new;
            c.sel = '{0, 1};
            ok = 1;
            repeat (200) begin
                c.mode = 1;
                ok &= c.randomize() && (c.b inside {10, 20}) && (c.e inside {30, 40});
                if (c.b == 20) twenty++;
                if (c.e == 30) thirty++;
                c.mode = 0;
                ok &= c.randomize();
                if (c.b == 10) ten++;
            end
            $display(\"%0d %0d %0d %0d\", ok, twenty > 150, ten > 150, thirty > 150);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["1 1 1 1"]
    );
}

#[test]
fn a_class_of_another_module_does_not_hide_a_package() {
    let source = "package P; typedef bit [2:0] T; endpackage
        module Other; class P; typedef int T; endclass endmodule
        module Top(output int bits); P::T value; assign bits = $bits(value); endmodule";
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("bits")), 3u32.into());
}

#[test]
fn processes_of_several_instances_share_the_static_properties_of_a_package_class() {
    let source = "`timescale 1ns/1ns
        package p;
            class C;
                static int made;
                function new(); made++; endfunction
            endclass
        endpackage
        module W; p::C c; initial c = new; endmodule
        module Top(output int made);
            W first();
            W second();
            initial #1 made = p::C::made;
        endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("made")), 2u32.into());
}

#[test]
fn type_arguments_keep_their_dimensions_scopes_and_class_scoped_types() {
    let source = "module Top(output int defs, output int narrow, output int wide);
        class Defs; typedef bit [7:0] T; endclass
        class Box #(type T = int);
            T value;
            function int bits(); return $bits(value); endfunction
        endclass
        Box#(Defs::T) box;
        for (genvar i = 2; i < 4; i++) begin : g
            Box#(bit [i:0]) sized;
            initial begin
                sized = new;
                if (i == 2) narrow = sized.bits(); else wide = sized.bits();
            end
        end
        initial begin box = new; defs = box.bits(); end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("defs")), 8u32.into());
    assert_eq!(sim.get(sim.signal("narrow")), 3u32.into());
    assert_eq!(sim.get(sim.signal("wide")), 4u32.into());
    // Arrays of strings of two sizes are two types (IEEE 1800-2023 6.22).
    let source = "module Top;
        class Box #(type T = int); endclass
        typedef string A [2];
        typedef string B [3];
        Box#(A) a;
        Box#(B) b;
        initial begin b = new; a = b; end
    endmodule";
    let Err(error) =
        Simulator::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top").build_cranelift()
    else {
        panic!("handles of two specializations are not assignment compatible");
    };
    assert!(error.to_string().contains("a handle of class"), "{error}");
}

#[test]
fn classes_of_packages_in_other_sources_keep_their_behaviour() {
    let package = r#"package p;
  class B; virtual function int f(); return 1; endfunction endclass
  interface class I; pure virtual function int id(); endclass
  class R; rand int x; int hooked; function void pre_randomize(); hooked = 5; endfunction endclass
  class P #(int N = 1);
    static function int f(); return P::N; endfunction
  endclass
  typedef P#(2) P2;
  class Plain; function int v(); return 9; endfunction endclass
  typedef Plain Alias;
  interface class Getter #(type T = int); pure virtual function T get(); endclass
endpackage
package q;
  class C; extern function int f(); endclass
  function int C::f(); return 2; endfunction
endpackage
package r;
  class C; extern function int f(); endclass
  function int C::f(); return 3; endfunction
endpackage"#;
    let module = r#"package w1; int p; endpackage
package w2; int p; endpackage
module Top(output int cast, output int over, output int iface, output int hooked, output int special, output int alias_v, output int qf, output int rf, output int got);
  import w1::*;
  import w2::*;
  class D extends p::B; virtual function int f(); return 4; endfunction endclass
  class M implements p::I; virtual function int id(); return 6; endfunction endclass
  interface class Getter #(type T = int); pure virtual function T get(); endclass
  class G implements Getter#(int); virtual function int get(); return 8; endfunction endclass
  p::B b;
  D d;
  M m;
  p::R rr;
  p::Alias al;
  q::C qc;
  r::C rc;
  G g;
  initial begin
    d = new;
    m = new;
    rr = new;
    void'(rr.randomize());
    al = p::Alias::new;
    qc = new; rc = new; g = new;
    cast = $cast(b, d);
    over = b.f();
    iface = m.id();
    hooked = rr.hooked;
    special = p::P2::f();
    alias_v = al.v();
    qf = qc.f();
    rf = rc.f();
    got = g.get();
  end
endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (package, Path::new("packages.sv")),
            (module, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(0).unwrap();
    for (signal, value) in [
        ("cast", 1u32),
        ("over", 4),
        ("iface", 6),
        ("hooked", 5),
        ("special", 2),
        ("alias_v", 9),
        ("qf", 2),
        ("rf", 3),
        ("got", 8),
    ] {
        assert_eq!(sim.get(sim.signal(signal)), value.into(), "{signal}");
    }
}

#[test]
fn specializations_tell_types_apart_and_keep_their_bases() {
    let source = r#"module Top(output int packed_made, output int unpacked_made, output int width, output int base_n);
  localparam int K = 2;
  class Box #(type T = int); static int made; function new(); made++; endfunction endclass
  typedef bit [1:0] A;
  typedef bit B [1:0];
  class Inner #(int N = 1); typedef bit [N:0] T; endclass
  class Outer #(int N = 1);
    function int w(); Inner#(N)::T v; return $bits(v); endfunction
  endclass
  class Base #(int N = 0); localparam int K = 1; function int n(); return N; endfunction endclass
  class Derived extends Base#(K); endclass
  Box#(A) a; Box#(B) b;
  Outer#(2) o;
  Derived d;
  initial begin
    a = new; b = new; o = new; d = new;
    packed_made = Box#(A)::made;
    unpacked_made = Box#(B)::made;
    width = o.w();
    base_n = d.n();
  end
endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    // `bit [1:0]` and `bit [1:0]` unpacked are two types (IEEE 1800-2023 6.22).
    assert_eq!(sim.get(sim.signal("packed_made")), 1u32.into());
    assert_eq!(sim.get(sim.signal("unpacked_made")), 1u32.into());
    assert_eq!(sim.get(sim.signal("width")), 3u32.into());
    // `extends Base#(K)` names the module's `K`, not the one `Base` declares.
    assert_eq!(sim.get(sim.signal("base_n")), 2u32.into());
}

#[test]
fn classes_keep_the_identity_and_signatures_of_their_scopes() {
    let first = r#"`timescale 1ns/1ns
class C; bit [7:0] x; endclass
module A(output int a);
  C c;
  initial begin c = new; c.x = 300; a = c.x; end
endmodule"#;
    let second = r#"`timescale 1ns/1ns
package p;
  parameter int N = 8;
  class B; virtual function int f(); return 1; endfunction static function bit [N-1:0] g(); return '1; endfunction endclass
endpackage
package q;
  class D extends p::B; virtual function int f(); return 5; endfunction endclass
endpackage
class C; bit [15:0] x; endclass
module Top(output int narrow, output int wide, output int over, output int bits);
  parameter int N = 16;
  int a;
  A ua(.a(a));
  C c;
  p::B b;
  q::D d;
  initial begin
    c = new; c.x = 300; d = new; b = d;
    #1;
    narrow = a;
    wide = c.x;
    over = b.f();
    bits = $bits(p::B::g());
  end
endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (first, Path::new("first.sv")),
            (second, Path::new("second.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(1).unwrap();
    // Each source's compilation-unit class `C` is its own (3.12.1).
    assert_eq!(sim.get(sim.signal("narrow")), 44u32.into());
    assert_eq!(sim.get(sim.signal("wide")), 300u32.into());
    // A subclass of another package reaches its override.
    assert_eq!(sim.get(sim.signal("over")), 5u32.into());
    // A package method keeps the width of its package's parameter.
    assert_eq!(sim.get(sim.signal("bits")), 8u32.into());
}

// The methods and static properties a module class inherits from a class of
// a package in another source keep what they are there: a source-local
// method identifier does not make a method of the module static, and a
// static property keeps the width the package's constants give it.
#[test]
fn inherited_members_of_package_classes_keep_their_declarations() {
    let package = r#"package p;
  localparam int N = 8;
  class B;
    static bit [N-1:0] x;
    static function int g(); return 1; endfunction
  endclass
endpackage"#;
    let module = r#"module Top(output int value, output int width);
  localparam int N = 16;
  class D extends p::B;
    int y;
    function int f(); y = 3; return y + $bits(x); endfunction
  endclass
  D d;
  initial begin
    d = new;
    value = d.f();
    width = $bits(p::B::x);
  end
endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (package, Path::new("packages.sv")),
            (module, Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("value")), 11u32.into());
    assert_eq!(sim.get(sim.signal("width")), 8u32.into());
}

sv_backends! {
    fn class_randomization_cyclic_checkpoint(sim) {
        @setup { let source = r#"
            module Top(output bit [63:0] value);
                timeunit 1ps / 1ps;
                class C; randc bit [2:0] x; endclass
                C c;
                initial begin
                    c = new;
                    c.srandom(41);
                    void'(c.randomize());
                    #10;
                    repeat (16) begin
                        void'(c.randomize());
                        value = (value << 3) | c.x;
                    end
                end
            endmodule
        "#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("cyclic_checkpoint.sv"))], "Top").four_state(true);
        sim.run_until(5).unwrap();
        let saved = sim.checkpoint().unwrap();
        let value = sim.signal("value");
        sim.run_until(15).unwrap();
        let expected = sim.get_four_state(value);
        sim.restore(&saved).unwrap();
        sim.run_until(15).unwrap();
        assert_eq!(sim.get_four_state(value), expected);
    }
}

sv_backends! {
    fn constraint_mode_checkpoint(sim) {
        @setup { let source = r#"
            module Top(input bit enable, output int mode, output bit [3:0] value);
                timeunit 1ps / 1ps;
                class C;
                    rand bit [3:0] x;
                    constraint c { x == 2; }
                endclass
                C p;
                initial begin
                    p = new;
                    p.constraint_mode(0);
                    #10 if (enable) p.c.constraint_mode(1);
                    #10;
                    mode = p.c.constraint_mode();
                    if (mode) begin
                        if (!p.randomize()) $fatal(1, "enabled solve");
                    end else begin
                        if (!(p.randomize() with { x == 7; })) $fatal(1, "restored solve");
                    end
                    value = p.x;
                end
            endmodule
        "#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("constraint_mode_checkpoint.sv"))], "Top").four_state(true);
        sim.run_until(5).unwrap();
        let saved = sim.checkpoint().unwrap();
        let enable = sim.signal("enable");
        sim.modify(|io| io.set(enable, 1u8)).unwrap();
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(sim.signal("mode")), 1u8.into());
        assert_eq!(sim.get(sim.signal("value")), 2u8.into());
        sim.restore(&saved).unwrap();
        sim.run_until(25).unwrap();
        assert_eq!(sim.get(sim.signal("mode")), 0u8.into());
        assert_eq!(sim.get(sim.signal("value")), 7u8.into());
    }
}

#[test]
fn foreign_generic_native_images_and_checkpoints_preserve_shared_handles() {
    let source = r#"`timescale 1ps/1ps
        package p;
            class Slot #(type T);
                static T saved;
                static function void put(T object); saved=object; endfunction
            endclass
        endpackage
        class Item;
            int value;
            function new(); value=7; endfunction
        endclass
        module Writer;
            Item object;
            initial begin
                object=new(); p::Slot#(Item)::put(object);
                #10; object.value=21;
                #10; object.value++;
            end
        endmodule
        module Reader(output int value);
            initial begin
                #15; value=p::Slot#(Item)::saved.value;
                #10; value=p::Slot#(Item)::saved.value;
            end
        endmodule
        module Top(output int value);
            Writer writer(); Reader reader(value);
        endmodule
    "#;
    for four_state in [false, true] {
        let builder = || {
            celox::Simulation::from_sv_sources(
                vec![(source, Path::new("foreign_checkpoint.sv"))],
                "Top",
            )
            .four_state(four_state)
        };
        let mut interpreted = builder().build_interpreter().unwrap();
        interpreted.run_until(30).unwrap();
        assert_eq!(
            interpreted.get(interpreted.signal("value")),
            BigUint::from(22u8)
        );
        let image = builder().compile_native().unwrap().into_program_image();
        let image =
            celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
                .unwrap();
        let mut sim = celox::Simulation::from_sv_sources(Vec::new(), "Top")
            .build_native_from_image(image)
            .unwrap();
        sim.run_until(15).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(21u8));
        let bytes = sim.checkpoint().unwrap().to_bytes().unwrap();
        // SAFETY: bytes were created by this simulator.
        let checkpoint = unsafe { sim.checkpoint_from_bytes(&bytes) }.unwrap();
        sim.run_until(30).unwrap();
        assert_eq!(sim.get(value), BigUint::from(22u8));
        sim.restore(&checkpoint).unwrap();
        sim.run_until(30).unwrap();
        assert_eq!(sim.get(value), BigUint::from(22u8));
    }
}

// This API chooses one compilation unit per source file (3.12.1).
sv_backends! {
    fn foreign_generic_distinct_unit_classes(sim) {
        @setup { let sources = vec![
            (r#"
package p;
 class Registry #(type T);
  static int count=0;
  static function T create(); T object; object=new(); count++; return object; endfunction
 endclass
endpackage"#, Path::new("registry.sv")),
            (r#"
import p::*;
class User;
 int value;
 function new(); value=42; endfunction
 typedef Registry#(User) type_id;
endclass
module First(output int result, count);
 User object;
 initial begin object=User::type_id::create(); result=object.value; count=User::type_id::count; end
endmodule"#, Path::new("first.sv")),
            (r#"
import p::*;
class User;
 int padding;
 int value;
 function new(); padding=99; value=73; endfunction
 typedef Registry#(User) type_id;
endclass
module Second(output int result, count);
 User object;
 initial begin object=User::type_id::create(); result=object.value; count=User::type_id::count; end
endmodule"#, Path::new("second.sv")),
            (r#"
module Top(output int first, second, first_count, second_count);
 First a(first, first_count); Second b(second, second_count);
endmodule"#, Path::new("top.sv")),
        ]; }
        @build celox::Simulation::from_sv_sources(sources, "Top");
        sim.run_until(0).unwrap();
        for (name, expected) in [("first", 42u32), ("second", 73), ("first_count", 1), ("second_count", 1)] {
            assert_eq!(sim.get(sim.signal(name)), BigUint::from(expected), "{name}");
        }
    }
}

sv_backends! {
    // A random enumeration of 1024 members takes one of them (IEEE 1800-2023
    // 18.3): its domain stays within the solver's limits.
    fn class_randomizes_large_enumerations(sim) {
        @setup {
            let members = (0..1024)
                .map(|index| format!("M{index} = 12'd{}", index * 3))
                .collect::<Vec<_>>()
                .join(", ");
            let source = format!(
                "module Top(output int ok, output int value);
                    typedef enum bit [11:0] {{ {members} }} big_e;
                    class Big; rand big_e e; constraint c {{ e > 12'd30; }} endclass
                    Big b;
                    initial begin
                        b = new;
                        ok = b.randomize() with {{ 1; }};
                        value = b.e;
                    end
                endmodule"
            );
        }
        @build celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("big_enum.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")), 1u32.into());
        let value: u32 = sim.get(sim.signal("value")).try_into().unwrap();
        assert!(value > 30 && value.is_multiple_of(3), "{value}");
    }

    // The members a ranged enumerator declares (6.19.2) are the domain of a
    // random enumeration like any others.
    fn class_randomizes_ranged_enumerators(sim) {
        @setup {
            let source = r#"module Top(output int ok, output int value);
                typedef enum bit [3:0] { A[3] = 4, B[7:6], C } e_t;
                class K; rand e_t e; constraint c { e != A0; e != C; } endclass
                K k;
                initial begin
                    k = new;
                    ok = k.randomize();
                    value = k.e;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ranged_enum_rand.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")), 1u32.into());
        let value: u32 = sim.get(sim.signal("value")).try_into().unwrap();
        assert!((5..=8).contains(&value), "{value}");
    }

    // A class parameter hides the module's constant of its name also in a
    // static call that passes it on by name (8.25).
    fn class_parameter_literals_hide_module_constants_in_static_calls(sim) {
        @setup {
            let source = r#"module Top(output int ok);
                localparam string S = "module";
                class Name #(string S = "x");
                    static function string get(); return S; endfunction
                endclass
                class Shadow #(string S = "x");
                    function string s(); return Name#(S)::get(); endfunction
                endclass
                Shadow #("a_name_longer_than_sixteen") shadow = new;
                initial ok = shadow.s() == "a_name_longer_than_sixteen";
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("shadow.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")), 1u32.into());
    }

    // A method's return type may name a method of a class declared after it,
    // which declaration-time queries see (8.25).
    fn class_signatures_naming_later_classes(sim) {
        @setup {
            let source = r#"package p;
                class A;
                    static function bit [$bits(B::g())-1:0] f(); return 0; endfunction
                endclass
                class B;
                    static function bit [11:0] g(); return 0; endfunction
                endclass
                localparam int W = $bits(A::f());
            endpackage
            module Top(output int width);
                import p::*;
                assign width = W;
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("signatures.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("width")), 12u32.into());
    }
}

/// A signed member of a property keeps its signedness as an inout actual:
/// the copy-in sign-extends it into a wider formal (IEEE 1800-2023 7.2.1,
/// 13.5), and a copy-out through a container element is published to a
/// forked waiter on the element as an assignment of it is (9.4.2).
#[test]
fn place_actuals_keep_signedness_and_publish_copy_outs() {
    let source = r#"`timescale 1ns/1ns
typedef struct packed { bit signed [3:0] s; bit [3:0] u; } sp_t;
class Holder; sp_t sp; endclass
module Top(output int seen, output int widened, output bit done);
  Holder h;
  int mq[$] = '{0, 0};
  function void widen(inout bit signed [7:0] io); widened = io; io = io - 1; endfunction
  task automatic bump(output int o); o = 7; endtask
  initial begin
    h = new;
    h.sp.s = -1;
    widen(h.sp.s);
    if (h.sp.s != -2) widened = 0;
    fork
      begin @(mq[1]); seen = mq[1] == 7 ? 1 : 2; end
      begin #1 bump(mq[1]); end
    join
    done = 1;
  end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("signed.sv"))], "Top")
        .build()
        .unwrap();
    let (seen, widened, done) = (
        sim.signal("seen"),
        sim.signal("widened"),
        sim.signal("done"),
    );
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(done), BigUint::from(1u8));
    assert_eq!(sim.get(widened), BigUint::from(u32::MAX));
    assert_eq!(sim.get(seen), BigUint::from(1u8));
}

#[test]
fn conditional_handles_of_sibling_classes_take_their_common_base() {
    // IEEE 1800-2023 11.4.11 c to e: a derived arm takes the other arm's
    // class, and arms of sibling classes their closest common base class.
    let source = "module Top;
        class A; virtual function int id(); return 1; endfunction endclass
        class B extends A; virtual function int id(); return 2; endfunction endclass
        class C extends B; virtual function int id(); return 3; endfunction endclass
        class D extends B; virtual function int id(); return 4; endfunction endclass
        bit s;
        int sibling, derived, method;
        initial begin
            C c; D d; B b; A a;
            c = new; d = new;
            s = 1;
            b = s ? c : d;
            sibling = b.id();
            a = s ? d : b;
            derived = a.id();
            method = (s ? d : c).id();
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("sibling")), 3u32.into());
    assert_eq!(sim.get(sim.signal("derived")), 4u32.into());
    assert_eq!(sim.get(sim.signal("method")), 4u32.into());
    // The result has the common base class, which a derived class's
    // variable does not take without `$cast` (8.16).
    let source = "module Top;
        class A; endclass
        class B extends A; endclass
        class C extends B; endclass
        class D extends B; endclass
        bit s;
        initial begin C c; D d; c = new; d = new; c = s ? c : d; end
    endmodule";
    let Err(error) =
        Simulator::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top").build_cranelift()
    else {
        panic!("a handle of the common base class was assigned to a derived class's variable");
    };
    assert!(
        error
            .to_string()
            .contains("a handle of class `B` is not assignment compatible"),
        "{error}"
    );
}

#[test]
fn class_handle_actuals_are_assigned_to_their_formals() {
    // IEEE 1800-2023 13.5: an input or inout actual is assigned to its
    // formal, so a handle of a class not deriving from the formal's is
    // rejected as an assignment is (8.16); a derived class's handle passes.
    let calls = [
        (
            "f",
            "function void f(A x); endfunction initial begin b = new; f(b); end",
        ),
        ("t", "task t(A x); endtask initial begin b = new; t(b); end"),
        (
            "t",
            "task t(inout A x); endtask initial begin b = new; t(b); end",
        ),
        (
            "H::f",
            "class H; function void f(A x); endfunction endclass
             H h; initial begin h = new; b = new; h.f(b); end",
        ),
        (
            "H::new",
            "class H; function new(A x); endfunction endclass
             H h; initial begin b = new; h = new(b); end",
        ),
    ];
    for (callee, body) in calls {
        let source =
            format!("module Top; class A; endclass class B; endclass B b; {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build() else {
            panic!("an unrelated class's handle was passed to a formal of class `A`: {body}");
        };
        let error = error.to_string();
        assert!(
            error.contains(&format!("argument 1 of `{callee}`"))
                && error.contains("a handle of class `B` is not"),
            "{error}"
        );
    }
    let source = "module Top;
        class A; virtual function int id(); return 1; endfunction endclass
        class B extends A; virtual function int id(); return 2; endfunction endclass
        int seen;
        function int f(A x); return x.id(); endfunction
        initial begin B b; b = new; seen = f(b); end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("seen")), 2u32.into());
}

#[test]
fn class_handle_results_take_handles_of_their_class() {
    // IEEE 1800-2023 13.4.1: a returned value is assigned to the function's
    // result, as assigning to the function's name is (8.16).
    for body in [
        "function A f(); B x; x = new; return x; endfunction initial begin A y; y = f(); end",
        "function A f(); B x; x = new; f = x; endfunction initial begin A y; y = f(); end",
        "class H; function A f(); B x; x = new; return x; endfunction endclass
         initial begin H h; A y; h = new; y = h.f(); end",
        // Arms of unrelated classes have no common type (11.4.11); each
        // arm is the value, and one is no `A`.
        "bit s; function A f(); A a; B x; a = new; x = new; return s ? a : x; endfunction
         initial begin A y; y = f(); end",
        "bit s; initial begin A a, y; B x; a = new; x = new; y = s ? a : (s ? x : a); end",
    ] {
        let source = format!("module Top; class A; endclass class B; endclass {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build() else {
            panic!("a function of class `A` returned an unrelated class's handle: {body}");
        };
        assert!(
            error.to_string().contains("a handle of class `B` is not"),
            "{error}"
        );
    }
    let source = "module Top;
        class A; virtual function int id(); return 1; endfunction endclass
        class B extends A; virtual function int id(); return 2; endfunction endclass
        int seen;
        function A f(); B x; x = new; return x; endfunction
        initial begin A a; a = f(); seen = a.id(); end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("seen")), 2u32.into());
}

#[test]
fn class_handle_defaults_take_handles_of_their_formals_class() {
    // IEEE 1800-2023 13.5.3: a default is assigned to its formal as an
    // actual is (8.16).
    for body in [
        "B gb; function void f(A x = gb); endfunction initial f();",
        "B gb; class H; function void f(A x = gb); endfunction endclass
         initial begin H h; h = new; h.f(); end",
    ] {
        let source = format!("module Top; class A; endclass class B; endclass {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build() else {
            panic!("a default of an unrelated class's handle was accepted: {body}");
        };
        let error = error.to_string();
        assert!(
            error.contains("default of `x`") && error.contains("a handle of class `B` is not"),
            "{error}"
        );
    }
    let source = "module Top;
        class A; virtual function int id(); return 1; endfunction endclass
        class B extends A; virtual function int id(); return 2; endfunction endclass
        int seen, none;
        B gb;
        function int f(A x = gb); return x == null ? 0 : x.id(); endfunction
        function int g(A x = null); return x == null ? 7 : 0; endfunction
        initial begin gb = new; seen = f(); none = g(); end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("seen")), 2u32.into());
    assert_eq!(sim.get(sim.signal("none")), 7u32.into());
}

#[test]
fn class_handle_outputs_are_assigned_back_to_their_actuals() {
    // IEEE 1800-2023 13.5.1: an output or inout formal is assigned to its
    // actual when the call returns, so the actual's class must take a handle
    // of the formal's (8.16).
    for body in [
        "task t(output A x); x = null; endtask initial begin B y; t(y); end",
        "task t(inout A x); endtask initial begin B y; t(y); end",
        "class H; task t(output A x); x = null; endtask endclass
         initial begin H h; B y; h = new; h.t(y); end",
    ] {
        let source =
            format!("module Top; class A; endclass class B extends A; endclass {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build() else {
            panic!("an output of a base class was copied to a derived class's variable: {body}");
        };
        assert!(
            error.to_string().contains("a handle of class `A` is not"),
            "{error}"
        );
    }
    let source = "module Top;
        class A; virtual function int id(); return 1; endfunction endclass
        class B extends A; virtual function int id(); return 2; endfunction endclass
        int seen;
        task t(output B x); x = new; endtask
        initial begin A a; t(a); seen = a.id(); end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("seen")), 2u32.into());
}
#[test]
fn class_handle_array_patterns_check_their_elements() {
    // IEEE 1800-2023 10.9.1: each item of an array pattern is assigned to
    // its element, so an element of class `A` takes only a handle its class
    // takes (8.16), whether listed, given by `default` or initializing.
    for body in [
        "A arr [2]; initial begin b = new; arr = '{b, null}; end",
        "initial begin A arr [2]; b = new; arr = '{null, b}; end",
        "B gb = new; A arr [2] = '{gb, null};",
        "A arr [2]; initial begin b = new; arr = '{default: b}; end",
        "A arr [2][2]; initial begin b = new; arr = '{'{null, null}, '{b, null}}; end",
    ] {
        let source =
            format!("module Top; class A; endclass class B; endclass B b; {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build() else {
            panic!("an array pattern took an unrelated class's handle: {body}");
        };
        let error = error.to_string();
        assert!(
            error.contains("assignment pattern member")
                && error.contains("a handle of class `B` is not"),
            "{error}"
        );
    }
    // An item of a level that selects subarrays is assigned to a subarray,
    // which takes no single handle (10.9.1, 7.6).
    for body in [
        "A arr [2][2]; initial begin a = new; arr = '{a, '{null, null}}; end",
        "A arr [2][2]; initial begin a = new; arr = '{'{null, null}, a}; end",
        "A arr [2][2]; initial begin a = new; arr = '{1: a, default: '{null, null}}; end",
    ] {
        let source = format!("module Top; class A; endclass A a; {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build() else {
            panic!("a subarray took a single handle: {body}");
        };
        let error = error
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            error.contains("assignment pattern item")
                && error.contains("not an unpacked array is not assignment compatible"),
            "{body}: {error}"
        );
    }
    // A default that is an array of the selected element's type is
    // assigned as an item is: an array of `B` handles is no array of `A`
    // handles (7.6), and one of `A` handles fills each element.
    let source = "module Top; class A; endclass class B; endclass
        A dst [2][2]; B src [2]; initial dst = '{default: src}; endmodule";
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top").build()
    else {
        panic!("an array default of unrelated handles was assigned");
    };
    let error = error
        .to_string()
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        error.contains("assignment pattern item") && error.contains("not assignment compatible"),
        "{error}"
    );
    // A call returning a subarray of the element type fills it.
    let source = "module Top;
        class A; virtual function int id(); return 3; endfunction endclass
        typedef A pair_t [2];
        int listed, defaulted;
        A dst [2][2];
        function pair_t make(); make[1] = new; endfunction
        initial begin
            dst = '{'{null, null}, make()};
            listed = dst[1][1].id();
            dst = '{default: make()};
            defaulted = dst[0][1].id();
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("listed")), 3u32.into());
    assert_eq!(sim.get(sim.signal("defaulted")), 3u32.into());
    // An item of any expression form is typed, a call by its declared
    // result: an array of `B` handles is no subarray of `A` handles.
    for body in [
        "A dst [2][2]; initial dst = '{make(), '{null, null}};",
        "A dst [2][2]; initial dst = '{default: make()};",
        "A dst [2][2]; initial dst = '{1: make(), default: '{null, null}};",
    ] {
        let source = format!(
            "module Top; class A; endclass class B; endclass
            typedef B pair_t [2];
            function pair_t make(); return '{{null, null}}; endfunction
            {body} endmodule"
        );
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build() else {
            panic!("a call's array of unrelated handles was assigned: {body}");
        };
        let error = error
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            error.contains("assignment pattern item")
                && error.contains("not assignment compatible"),
            "{body}: {error}"
        );
    }
    let source = "module Top;
        class A; virtual function int id(); return 1; endfunction endclass
        int corner;
        A dst [2][2]; A src [2];
        initial begin src[1] = new; dst = '{default: src}; corner = dst[1][1].id(); end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("corner")), 1u32.into());
    let source = "module Top;
        class A; virtual function int id(); return 1; endfunction endclass
        class B extends A; virtual function int id(); return 2; endfunction endclass
        int first, second, rest;
        A arr [3];
        initial begin
            A a; B b;
            a = new; b = new;
            arr = '{b, a, null};
            first = arr[0].id();
            second = arr[1].id();
            rest = arr[2] == null;
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("first")), 2u32.into());
    assert_eq!(sim.get(sim.signal("second")), 1u32.into());
    assert_eq!(sim.get(sim.signal("rest")), 1u32.into());
}

#[test]
fn static_class_casts_assign_upcasts_and_reject_downcasts() {
    // IEEE 1800-2023 6.24.1: a static cast of an assignment compatible
    // operand is the assignment; a downcast needs `$cast` (8.16), and a
    // class is no destination of a bit-stream cast (6.24.3).
    let source = "module Top(output int id, output int none);
        class C; virtual function int id(); return 1; endfunction endclass
        class D extends C; virtual function int id(); return 2; endfunction endclass
        initial begin
            D d; C c;
            d = new;
            c = C'(d);
            id = c.id();
            c = C'(null);
            none = c == null;
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("id")), 2u32.into());
    assert_eq!(sim.get(sim.signal("none")), 1u32.into());
    // The target is any class type: a specialization or a class of a
    // package; the cast has the target's type, whose methods it calls.
    let source =
        "package p; class Q; virtual function int id(); return 7; endfunction endclass endpackage
        module Top(output int special, output int called, output int packaged);
        class Box #(int N = 1); virtual function int id(); return N; endfunction endclass
        class Big extends Box #(4); virtual function int id(); return 40; endfunction endclass
        class R extends p::Q; virtual function int id(); return 8; endfunction endclass
        initial begin
            Big b; Box #(4) x; R r;
            b = new; r = new;
            x = Box#(4)'(b);
            special = x.id();
            called = Box#(4)'(b).id();
            packaged = p::Q'(r).id();
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("special")), 40u32.into());
    assert_eq!(sim.get(sim.signal("called")), 40u32.into());
    assert_eq!(sim.get(sim.signal("packaged")), 8u32.into());
    for (cast, expected) in [
        ("d = D'(c);", "a downcast needs"),
        ("c = C'(i);", "no class handle"),
        ("c = C'(o);", "is not assignment compatible"),
        (
            "begin Box #(2) b2; b2 = Box#(2)'(c); end",
            "is not assignment compatible",
        ),
        // The operand's type must be known: arms of a class handle and an
        // int have no common type (11.4.11).
        ("c = C'(i ? c : 1);", "has no common type"),
        // Each arm of a conditional is a value the destination may take,
        // and an int is no handle (8.16).
        ("c = i ? c : 1;", "is no handle of class `C`"),
        ("c = 1;", "is no handle of class `C`"),
    ] {
        let source = format!(
            "module Top; class C; endclass class D extends C; endclass class O; endclass
             class Box #(int N = 1); endclass
             initial begin C c; D d; O o; int i; c = new; o = new; {cast} end endmodule"
        );
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build() else {
            panic!("`{cast}` was accepted");
        };
        let error = error
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(error.contains(expected), "{cast}: {error}");
    }
}

/// The outputs `names` of `source` at time 2, in both state modes, with
/// calls inlined and out of line.
fn static_outputs(source: &str, names: &[&str]) -> Vec<Vec<BigUint>> {
    let mut runs = Vec::new();
    for four_state in [false, true] {
        for forced in [false, true] {
            let builder =
                celox::Simulation::from_sv_sources(vec![(source, Path::new("statics.sv"))], "Top")
                    .four_state(four_state);
            let builder = if forced {
                builder.force_out_of_line_calls()
            } else {
                builder
            };
            let mut sim = builder
                .build()
                .unwrap_or_else(|error| panic!("four-state {four_state} forced {forced}: {error}"));
            sim.run_until(2).unwrap();
            runs.push(names.iter().map(|name| sim.get(sim.signal(name))).collect());
        }
    }
    runs
}

/// A static variable of a method is one per specialization of its class
/// (IEEE 1800-2023 6.21, 8.25): two specializations of `counter` count
/// apart, and each specialization of `registry` keeps its own singleton, as
/// UVM's `uvm_object_registry#(T, Tname)::get` does.
#[test]
fn specializations_have_their_own_static_variables() {
    let source = r#"`timescale 1ns/1ns
package p;
    class counter #(int ID = 0);
        function int next();
            static int count = 0;
            count++;
            return count * 10 + ID;
        endfunction
    endclass
    class registry #(type T = int, string NAME = "");
        typedef registry #(T, NAME) this_type;
        int tag;
        static function this_type get();
            static this_type m_inst;
            if (m_inst == null) begin
                m_inst = new();
                m_inst.tag = $bits(T);
            end
            return m_inst;
        endfunction
    endclass
endpackage
import p::*;
module Top(output int a, output int b, output int c, output int d, output bit same,
           output bit apart);
    counter #(1) one = new;
    counter #(2) two = new;
    initial begin
        a = one.next();
        a = one.next();
        b = two.next();
        c = registry #(byte, "b")::get().tag;
        d = registry #(int, "i")::get().tag;
        same = registry #(byte, "b")::get() == registry #(byte, "b")::get();
        apart = registry #(byte, "b")::get().tag != registry #(int, "i")::get().tag;
    end
endmodule
"#;
    for run in static_outputs(source, &["a", "b", "c", "d", "same", "apart"]) {
        assert_eq!(run, [21u32, 12, 8, 32, 1, 1].map(BigUint::from).to_vec());
    }
}

/// The static variables of subroutines of a package, of a class
/// specialization in it and of `$unit` that have one name and the same
/// position among their analyses' declarations are three variables: each
/// is named by its owner and its declaration, not by a counter of the
/// analysis that declares it.
#[test]
fn statics_of_package_and_unit_subroutines_are_their_own() {
    let source = r#"`timescale 1ns/1ns
function automatic int unit_next();
    static int m_inst = 0;
    m_inst++;
    return m_inst;
endfunction
package p;
    class registry #(type T = int);
        static function int get();
            static int m_inst = 0;
            m_inst += 10;
            return m_inst + $bits(T);
        endfunction
    endclass
    function automatic int pkg_next();
        static int m_inst = 0;
        m_inst += 100;
        return m_inst;
    endfunction
endpackage
import p::*;
module Top(output int a, output int b, output int c, output int d);
    initial begin
        a = unit_next();
        a = unit_next();
        b = registry #(byte)::get();
        b = registry #(byte)::get();
        c = registry #(int)::get();
        d = pkg_next();
    end
endmodule
"#;
    for run in static_outputs(source, &["a", "b", "c", "d"]) {
        assert_eq!(run, [2u32, 28, 42, 100].map(BigUint::from).to_vec());
    }
}

/// A class specialization of a package that both the package and the
/// compilation unit lower, here `registry #(byte)` through `base::f` and
/// through `$unit`'s `env::g`, has one static variable `n` (IEEE 1800-2023
/// 6.21): the compilation unit binds the package's, which the package
/// declares once. UVM's `uvm_component_registry#(T, Tname)::get` stopped the
/// elaboration so ("duplicate port or signal name `m_inst@1192`").
#[test]
fn statics_of_specializations_lowered_by_two_scopes_are_one() {
    let source = r#"`timescale 1ns/1ns
package p;
    int seed = 1;
    class registry #(type T = int);
        static function int get();
            static int n = 0;
            n += p::seed;
            return n;
        endfunction
    endclass
    class base;
        function int f(); return registry #(byte)::get(); endfunction
    endclass
endpackage
import p::*;
class env extends base;
    function int g(); return registry #(byte)::get() + 100; endfunction
endclass
module Top(output int a, output int b);
    env e = new;
    initial begin
        a = e.f();
        b = e.g();
    end
endmodule
"#;
    for run in static_outputs(source, &["a", "b"]) {
        assert_eq!(run, [1u32, 102].map(BigUint::from).to_vec());
    }
}

/// The value of output `a` of `source` at time 1, in both state modes.
fn output_a(source: &str) -> Vec<BigUint> {
    [false, true]
        .map(|four_state| {
            let mut sim =
                celox::Simulation::from_sv_sources(vec![(source, Path::new("statics.sv"))], "Top")
                    .four_state(four_state)
                    .build()
                    .unwrap_or_else(|error| panic!("four-state {four_state}: {error}"));
            sim.run_until(1).unwrap();
            sim.get(sim.signal("a"))
        })
        .to_vec()
}

/// The initializer of a static property is bound in the scope of its class
/// (IEEE 1800-2023 8.9, 8.25), in a package and in a module: a static
/// property it reads is the class's, not the module's port of the same
/// name, and a call of a static method calls that method.
#[test]
fn static_property_initializers_see_their_class() {
    let in_package = |body: &str| {
        format!(
            "`timescale 1ns/1ns\npackage p;\n    class reg_t;\n{body}\n    endclass\nendpackage\n\
             import p::*;\nmodule Top(output int a);\n    initial a = reg_t::get();\nendmodule\n"
        )
    };
    let in_module = |body: &str| {
        format!(
            "`timescale 1ns/1ns\nmodule Top(output int a);\n    class reg_t;\n{body}\n    endclass\n\
             initial a = reg_t::get();\nendmodule\n"
        )
    };
    let property = "static int a = 4;\nstatic int b = a + 1;\n\
                    static function int get(); return b; endfunction";
    let call = "static function int init(); return 7; endfunction\nstatic int a = init();\n\
                static function int get(); return a; endfunction";
    for (source, expected) in [
        (in_package(property), 5u32),
        (in_module(property), 5),
        (in_package(call), 7),
        (in_module(call), 7),
    ] {
        assert_eq!(
            output_a(&source),
            vec![BigUint::from(expected); 2],
            "{source}"
        );
    }
}

/// UVM's `uvm_registry_common`: each specialization registers itself once,
/// through the initializer `local static bit m__initialized =
/// __deferred_init();`, which calls the specialization's own static method.
#[test]
fn specializations_initialize_their_static_properties_through_their_methods() {
    let source = r#"`timescale 1ns/1ns
package p;
    int registered[$];
    class registry #(type T = int);
        static function bit __deferred_init();
            registered.push_back($bits(T));
            return 1;
        endfunction
        local static bit m__initialized = __deferred_init();
        static function bit initialized(); return m__initialized; endfunction
    endclass
endpackage
import p::*;
module Top(output int a);
    initial begin
        a = registry #(byte)::initialized() + registry #(shortint)::initialized();
        a = a * 100 + p::registered.sum();
    end
endmodule
"#;
    assert_eq!(output_a(source), vec![BigUint::from(224u32); 2]);
}

/// The initializers of a package's variables and of the static properties
/// of its classes run before every process (IEEE 1800-2023 6.8, 10.5) and
/// call subroutines as any process does: out of line, recursive ones
/// included. UVM's registries reach the recursive `uvm_coreservice_t::get`
/// so.
#[test]
fn package_initializers_call_recursive_subroutines() {
    let variable = r#"`timescale 1ns/1ns
package p;
    function automatic int fact(int n);
        if (n <= 1) return 1;
        return n * fact(n - 1);
    endfunction
    int value = fact(4);
endpackage
module Top(output int a);
    initial #1 a = p::value;
endmodule
"#;
    let property = r#"`timescale 1ns/1ns
package p;
    class reg_t;
        static function int fact(int n);
            if (n <= 1) return 1;
            return n * fact(n - 1);
        endfunction
        static int value = fact(4);
        static function int get(); return value; endfunction
    endclass
endpackage
import p::*;
module Top(output int a);
    initial a = reg_t::get();
endmodule
"#;
    // A singleton whose `get` calls itself while it makes the instance.
    let singleton = r#"`timescale 1ns/1ns
package p;
    class cs_t;
        static cs_t inst;
        int depth;
        static function cs_t get();
            if (inst == null) begin
                inst = new;
                inst.depth = 1 + (get() == inst);
            end
            return inst;
        endfunction
    endclass
    class user_t;
        static cs_t cs = cs_t::get();
        static function int depth(); return cs.depth; endfunction
    endclass
endpackage
import p::*;
module Top(output int a);
    initial #1 a = user_t::depth();
endmodule
"#;
    for (source, expected) in [(variable, 24u32), (property, 24), (singleton, 2)] {
        assert_eq!(
            output_a(source),
            vec![BigUint::from(expected); 2],
            "{source}"
        );
    }
}

sv_backends! {
    // A class of the compilation unit has the compilation unit's time unit,
    // which a `timescale directive does not set: the default, 1 ns (IEEE
    // 1800-2023 3.14.2.3). Its precision, 1 ps, is finer than the global
    // precision the directive gives the design (3.14.3), so its delays and
    // `$time` round to whole steps of 1 ns.
    fn compilation_unit_classes_count_time_in_the_default_unit(sim) {
        @setup {
            let source = r#"
                `timescale 1ns/1ns
                class C;
                    static function time now(); return $time; endfunction
                    static task wait_units(); #10; endtask
                    static task wait_literal(); #3ns; endtask
                endclass
                module Top(output longint a, output longint b, output longint c, output longint d);
                    initial begin
                        #5 a = C::now();
                        C::wait_units();
                        b = $time;
                        C::wait_literal();
                        c = $time;
                        d = C::now();
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("unit_class_time.sv"))], "Top");
        sim.run_until(100).unwrap();
        assert_eq!(sim.get(sim.signal("a")), 5u32.into());
        assert_eq!(sim.get(sim.signal("b")), 15u32.into());
        assert_eq!(sim.get(sim.signal("c")), 18u32.into());
        assert_eq!(sim.get(sim.signal("d")), 18u32.into());
    }
    // Every form of a delay rounds to the precision of its scope first, then
    // to the global precision (3.14.1): in a compilation-unit class under a
    // coarser `timescale, a real delay and a time literal of the same
    // duration wait the same time.
    fn compilation_unit_classes_round_every_delay_alike(sim) {
        @setup {
            let source = r#"
                `timescale 1ns/1ns
                class C;
                    static task wait_real(real amount); #amount; endtask
                    static task wait_real_literal(); #1.4999; endtask
                    static task wait_time_literal(); #1.4999ns; endtask
                    static task wait_low(); #1.4994ns; endtask
                    static task wait_real_low(); #1.4994; endtask
                endclass
                module Top(output longint a, output longint b, output longint c, output longint d, output longint e);
                    initial begin
                        C::wait_real(1.4999);
                        a = $time;
                        C::wait_real_literal();
                        b = $time;
                        C::wait_time_literal();
                        c = $time;
                        C::wait_low();
                        d = $time;
                        C::wait_real_low();
                        e = $time;
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("unit_class_rounding.sv"))], "Top");
        sim.run_until(100).unwrap();
        // 1.4999 ns is 1.500 ns at 1 ps, then 2 ns; 1.4994 ns is 1.499 ns,
        // then 1 ns.
        assert_eq!(sim.get(sim.signal("a")), 2u32.into());
        assert_eq!(sim.get(sim.signal("b")), 4u32.into());
        assert_eq!(sim.get(sim.signal("c")), 6u32.into());
        assert_eq!(sim.get(sim.signal("d")), 7u32.into());
        assert_eq!(sim.get(sim.signal("e")), 8u32.into());
    }
}

#[test]
fn abstract_classes_of_the_compilation_unit_construct_their_subclasses() {
    // The constructor of an abstract class (IEEE 1800-2023 8.21) runs for
    // each object of a class derived from it, `super.new` implicit or not,
    // and its pure constraint is the subclass's (18.5.2). Declared in the
    // compilation unit, the class was not lowered: "call of `a::new`".
    let source = "virtual class a;
        int x = 3;
        int made;
        function new(); made = x + 1; endfunction
        pure constraint c;
    endclass
    virtual class b extends a;
        int y = 4;
    endclass
    class c extends b;
        rand int z;
        constraint c { z == 5; }
    endclass
    module Top(output int x, output int made, output int y, output int z);
        c obj = new;
        initial begin
            void'(obj.randomize());
            x = obj.x; made = obj.made; y = obj.y; z = obj.z;
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("x")), 3u32.into());
    assert_eq!(sim.get(sim.signal("made")), 4u32.into());
    assert_eq!(sim.get(sim.signal("y")), 4u32.into());
    assert_eq!(sim.get(sim.signal("z")), 5u32.into());
}

#[test]
fn modules_create_classes_they_only_reach_through_a_package_factory() {
    // `simple_test` asks for `Reg#(simple_test)`, whose static initializer
    // registers a `Holder#(simple_test)` with the package; the module names
    // neither class and creates the object through the package, as UVM's
    // `run_test("simple_test")` does. The specializations run the class's
    // constructor, its static initializers and its overrides (IEEE
    // 1800-2023 3.12.1, 8.25).
    let source = "package p;
        class Base;
            string name;
            function new(string name); this.name = name; endfunction
            virtual function string get_type_name(); return \"Base\"; endfunction
            virtual function int run(); return 0; endfunction
        endclass
        virtual class Wrapper;
            pure virtual function Base create(string name);
        endclass
        Wrapper table_[$];
        class Holder #(type T = Base) extends Wrapper;
            virtual function Base create(string name); T obj; obj = new(name); return obj; endfunction
        endclass
        class Reg #(type T = Base);
            static bit registered = add();
            static function bit add(); Holder #(T) me = new; table_.push_back(me); return 1; endfunction
        endclass
        function automatic Base make(int kind, string name); return table_[kind].create(name); endfunction
    endpackage
    import p::*;
    class simple_test extends Base;
        typedef Reg#(simple_test) type_id;
        const static string type_name = \"simple_test\";
        function new(string name); super.new({name, \"!\"}); endfunction
        virtual function string get_type_name(); return type_name; endfunction
        virtual function int run(); return 7; endfunction
    endclass
    module Top(output bit named, output bit typed, output int ran);
        initial begin
            Base b;
            b = make(0, \"t\");
            named = b.name == \"t!\";
            typed = b.get_type_name() == \"simple_test\";
            ran = b.run();
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("named")), 1u32.into());
    assert_eq!(sim.get(sim.signal("typed")), 1u32.into());
    assert_eq!(sim.get(sim.signal("ran")), 7u32.into());

    // A class of the unit declared after the module is no item the module
    // sees (IEEE 1800-2023 3.12.1): the module does not take the unit for
    // it, and so cannot create it.
    let (head, module) = source.split_once("    module Top").unwrap();
    let (package, class) = head.split_once("    class simple_test").unwrap();
    let later = format!("{package}    module Top{module}\n    class simple_test{class}");
    assert!(
        celox::Simulation::from_sv_sources(vec![(later.as_str(), Path::new("class.sv"))], "Top")
            .build()
            .is_err(),
        "{later}"
    );

    // A module takes only the unit of its own source: the module of b.sv
    // sees `Reg#($unit::simple_test)`, a specialization for a class of the
    // unit of a.sv, and leaves its own unit, which it does not use, aside
    // (IEEE 1800-2023 3.12.1).
    let (package, rest) = source.split_once("endpackage").unwrap();
    let (classes, _) = rest.split_once("    module Top").unwrap();
    let package = format!("{package}endpackage");
    let other = "import p::*;
    localparam int W = 3;
    module Top(output int y);
        initial y = 3;
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(
        vec![
            (package.as_str(), Path::new("p.sv")),
            (classes, Path::new("a.sv")),
            (other, Path::new("b.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 3u32.into());
}

#[test]
fn static_rand_properties_are_solved_once_for_every_object() {
    // A static rand property is one variable of the class: a randomize call
    // of any object solves it under the constraints, and every object sees
    // the value (IEEE 1800-2023 18.4). Its rand_mode is shared too (18.8):
    // turned off through one object, it is off for the other, and a call
    // then keeps its value.
    let source = "class A;
        static rand int b;
        constraint c { b > 5; b < 12; }
    endclass
    module Top;
        A o1, o2;
        initial begin
            o1 = new;
            o2 = new;
            void'(o1.randomize());
            $display(\"%0d\", o1.b == o2.b && o1.b > 5 && o1.b < 12);
            void'(o2.randomize());
            $display(\"%0d\", o1.b == o2.b && A::b > 5 && A::b < 12);
            o1.b.rand_mode(0);
            $display(\"%0d %0d\", o1.b.rand_mode(), o2.b.rand_mode());
            A::b = 77;
            void'(o2.randomize());
            $display(\"%0d\", A::b);
            o2.b.rand_mode(1);
            void'(o1.randomize());
            $display(\"%0d %0d\", o1.b.rand_mode(), A::b > 5 && A::b < 12);
            // The object's rand_mode covers the static property too.
            o2.rand_mode(0);
            A::b = 50;
            void'(o1.randomize());
            $display(\"%0d %0d\", o1.b.rand_mode(), A::b);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["1", "1", "0 0", "77", "1 1", "0 50"]
    );
}

#[test]
fn constraints_read_static_properties_when_randomize_runs() {
    // A static property a constraint names is read when the call starts
    // (IEEE 1800-2023 18.5): by the class's constraints, a derived class's
    // and an inline one's, which also reads a variable of the caller. An
    // object that a rand handle reaches is not solved with them: its
    // randomize call reports so.
    let source = "class A;
        rand int y;
        static int lim = 3;
        constraint c { y >= 0; y < lim; }
    endclass
    class D extends A;
        static int low = 1;
        constraint d { y > low; }
    endclass
    module Top;
        A a;
        D d;
        int k, ok, lim;
        initial begin
            lim = 100;
            a = new;
            d = new;
            ok = 1;
            repeat (4) begin
                void'(a.randomize());
                ok &= a.y >= 0 && a.y < 3;
            end
            A::lim = 1;
            void'(a.randomize());
            $display(\"%0d %0d\", ok, a.y);
            A::lim = 6;
            k = 3;
            ok = 1;
            repeat (4) begin
                // `lim` is the class's static property, not the caller's.
                void'(a.randomize() with { y > k; y < lim; });
                ok &= a.y > 3 && a.y < 6;
                void'(d.randomize());
                ok &= d.y > 1 && d.y < 6;
            end
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["1 0", "1"]
    );
    let member = "class A; rand int y; static int lim = 3; constraint c { y < lim; } endclass
    class H; rand A a; function new(); a = new; endfunction endclass
    module Top; H h; initial begin h = new; void'(h.randomize()); end endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(member, Path::new("class.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let error = sim
        .run_until(1)
        .expect_err("the member is not solved")
        .to_string();
    assert!(
        error.contains("constraints name static properties"),
        "{error}"
    );
    let randc = "class A; randc static bit [1:0] z; endclass
    module Top; A a; initial a = new; endmodule";
    let error = Simulator::from_sv_sources(vec![(randc, Path::new("class.sv"))], "Top")
        .build_cranelift()
        .expect_err("a randc static property is rejected")
        .to_string();
    assert!(error.contains("randc static class property"), "{error}");
}

#[test]
fn static_rand_properties_are_named_by_their_declarations() {
    // `randomize(b)` names a static rand property as it names any property
    // of the class (IEEE 1800-2023 18.11), and the bit that turns one off is
    // no name of the class: `p$rand_off`, an identifier (5.6), is a property
    // of its own in either order of the declarations (18.8).
    for order in [
        "static rand int p; static int p$rand_off = 9;",
        "static int p$rand_off = 9; static rand int p;",
    ] {
        let source = format!(
            "class C;
                static rand int b;
                rand int x;
                {order}
                constraint k {{ b inside {{[1:3]}}; }}
            endclass
            module Top;
                C c;
                int ok;
                initial begin
                    c = new;
                    c.x = 5;
                    ok = 1;
                    repeat (4) begin
                        C::b = 0;
                        ok &= c.randomize(b) && C::b >= 1 && C::b <= 3 && c.x == 5;
                    end
                    c.p.rand_mode(0);
                    C::p = 1;
                    void'(c.randomize());
                    $display(\"%0d %0d %0d %0d\", ok, C::p, C::p$rand_off, c.p.rand_mode());
                end
            endmodule"
        );
        assert_eq!(
            super::hierarchical_references::displayed(&source),
            ["1 1 9 0"],
            "{order}"
        );
    }
}

#[test]
fn static_rand_properties_follow_randomize_arguments_and_their_class_methods() {
    // Arguments of `randomize` name the random variables of the call,
    // whatever their modes, so a static rand property they leave out keeps
    // its value (IEEE 1800-2023 18.11); a method of the class sets and reads
    // the mode of its static rand property by its name (18.8).
    let source = "class C;
        static rand int p;
        rand int x;
        constraint k { x inside {[0:5]}; p inside {[10:1000]}; }
        function void freeze(); p.rand_mode(0); endfunction
        function int mode(); return p.rand_mode(); endfunction
    endclass
    module Top;
        C c;
        int ok;
        initial begin
            c = new;
            ok = 1;
            repeat (4) begin
                C::p = 15;
                ok &= c.randomize(x) && C::p == 15 && c.x <= 5;
            end
            $display(\"%0d %0d\", ok, c.mode());
            c.freeze();
            C::p = 7;
            void'(c.randomize());
            $display(\"%0d %0d\", c.mode(), C::p);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["1 1", "0 7"]
    );
}

#[test]
fn static_inputs_are_those_of_the_class_that_declares_them() {
    // A base's constraint reads the base's static property though a derived
    // class redeclares it, and the derived class's constraint reads its own
    // (IEEE 1800-2023 8.14, 18.5). An inline constraint called through a
    // base handle reads the caller's variable, whatever statics the class
    // of the object adds (18.7).
    let source = "class A;
        rand int x;
        static int limit = 3;
        constraint a { x >= 0; x < limit; }
    endclass
    class D extends A;
        rand int y, z;
        static int limit = 100;
        static int extra = 7;
        constraint d { y >= 50; y < limit; z == extra; }
    endclass
    module Top;
        A a;
        D d;
        int k, ok;
        initial begin
            d = new;
            ok = 1;
            repeat (4) begin
                void'(d.randomize());
                ok &= d.x >= 0 && d.x < 3 && d.y >= 50 && d.y < 100 && d.z == 7;
            end
            a = d;
            k = 2;
            repeat (4) begin
                void'(a.randomize() with { x == k; });
                ok &= d.x == 2 && d.z == 7;
            end
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1"]);
}

/// The error of building and running `source` for one time unit.
fn class_error(source: &str, what: &str) -> String {
    match celox::Simulation::from_sv_sources(vec![(source, Path::new("class.sv"))], "Top").build() {
        Err(error) => error.to_string(),
        Ok(mut sim) => sim.run_until(1).expect_err(what).to_string(),
    }
}

#[test]
fn rand_mode_of_a_base_local_static_rand_property_is_not_visible() {
    // A `local` member of a base class is not visible in a derived class
    // (IEEE 1800-2023 8.18), its static rand property's shared mode
    // included (18.8).
    for use_ in [
        "function void f(); p.rand_mode(0); endfunction",
        "function int f(); return p.rand_mode(); endfunction",
    ] {
        let source = format!(
            "class Base; local static rand int p; endclass
            class Derived extends Base; {use_} endclass
            module Top; Derived d; initial begin d = new; void'(d.f()); end endmodule"
        );
        let error = class_error(&source, use_);
        assert!(
            error.contains("local class member") && error.contains("::p` is not"),
            "{use_}: {error}"
        );
    }
}

#[test]
fn randomize_arguments_name_the_member_of_the_most_derived_class() {
    // An instance property of a derived class hides a static rand property
    // of its base of the name (IEEE 1800-2023 8.14): `randomize(p)` makes
    // the instance property random, and leaves the static one alone (18.11).
    let source = "class Base; static rand int p; endclass
    class Derived extends Base;
        rand int p;
        constraint c { p inside {[5:7]}; }
    endclass
    module Top;
        Derived d;
        int ok;
        initial begin
            d = new;
            Base::p = 100;
            ok = d.randomize(p) && d.p >= 5 && d.p <= 7 && Base::p == 100;
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1"]);
}

#[test]
fn constraints_name_the_static_property_that_hides_an_instance_one() {
    // A static property of a derived class hides a rand instance property
    // of its base of the name (IEEE 1800-2023 8.14): its constraint reads
    // the static value (18.5).
    let source = "class Base; rand int p; endclass
    class Derived extends Base;
        static int p = 4;
        rand int x;
        constraint c { x == p; }
    endclass
    module Top;
        Derived d;
        int ok;
        initial begin
            d = new;
            ok = 1;
            repeat (5) ok &= d.randomize() && d.x == 4;
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1"]);
}

#[test]
fn rand_object_members_with_hidden_static_rand_properties_stay_rejected() {
    // A static rand property a derived class hides by a static one of the
    // name is still the base's, which the derived class's randomize calls
    // solve: an object a rand handle reaches does not solve it, as one of a
    // class with a visible static rand property does not (IEEE 1800-2023
    // 18.4).
    let source = "class Base; static rand int p; endclass
    class Derived extends Base; static int p; endclass
    class Holder;
        rand Derived h;
        function new(); h = new; endfunction
    endclass
    module Top; Holder o; initial begin o = new; void'(o.randomize()); end endmodule";
    let error = class_error(source, "a hidden static rand property of a rand member");
    assert!(
        error.contains("a rand object member whose constraints name static properties"),
        "{error}"
    );
}

#[test]
fn constraints_do_not_see_a_base_local_static_rand_property() {
    // A `local` static rand property of a base class is not visible in the
    // constraints of a derived class (IEEE 1800-2023 8.18).
    let source = "class Base; local static rand int p; endclass
    class Derived extends Base;
        rand int x;
        constraint c { x == p; }
    endclass
    module Top; Derived d; initial begin d = new; void'(d.randomize()); end endmodule";
    let error = class_error(source, "a base's local static rand property");
    assert!(error.contains("local member `p`"), "{error}");
}

#[test]
fn static_properties_are_admitted_as_instance_properties_are() {
    // The solver takes the static properties a constraint names, and the
    // static rand ones, as it takes instance properties of the type: a
    // 4-state one in a constraint is rejected with the same diagnostic, and
    // a rand `real` (IEEE 1800-2023 18.4) is built as an instance one is.
    let error = |members: &str| {
        let source = format!(
            "class C; rand int x; {members} endclass
            module Top; C c; initial begin c = new; void'(c.randomize()); end endmodule"
        );
        celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("class.sv"))], "Top")
            .build()
            .err()
            .map(|error| error.to_string())
    };
    let instance = error("logic [3:0] l = 2; constraint c { x < l; }");
    assert!(
        instance
            .as_deref()
            .is_some_and(|error| error.contains("4-state class property in a constraint")),
        "{instance:?}"
    );
    assert_eq!(
        error("static logic [3:0] l = 2; constraint c { x < l; }"),
        instance
    );
    assert_eq!(error("static rand real r;"), error("rand real r;"));
    // With a constraint the solver builds the program, which takes a rand
    // `real` it does not name as it takes an instance one, and rejects a
    // 4-state rand property as it rejects an instance one.
    for (instance, shared) in [
        (
            "rand real r; constraint c { x > 0; }",
            "static rand real r; constraint c { x > 0; }",
        ),
        ("rand logic [3:0] l;", "static rand logic [3:0] l;"),
        (
            "rand logic [3:0] l; constraint c { x > 0; }",
            "static rand logic [3:0] l; constraint c { x > 0; }",
        ),
    ] {
        assert_eq!(error(shared), error(instance), "{shared}");
    }
    assert_eq!(error("static rand real r; constraint c { x > 0; }"), None);
    assert!(
        error("static rand logic [3:0] l;")
            .is_some_and(|error| error.contains("randomization of a 4-state class property"))
    );
}

#[test]
fn randomize_arguments_select_static_rand_properties_through_base_handles() {
    // `b.randomize(s)` through a base handle selects the base's static rand
    // property in the derived object it dispatches to (IEEE 1800-2023 8.20,
    // 18.11), not a property the derived class adds.
    let source = "class Base; static rand bit [7:0] s; rand int a; endclass
    class Derived extends Base; rand int x, y; endclass
    module Top;
        Base b;
        Derived d;
        int ok, changed;
        bit [7:0] previous;
        initial begin
            d = new;
            b = d;
            d.a = 7; d.x = 5; d.y = 6;
            ok = 1;
            repeat (8) begin
                previous = Base::s;
                ok &= b.randomize(s);
                changed += Base::s != previous;
            end
            $display(\"%0d %0d %0d %0d %0d\", ok, changed >= 6, d.a, d.x, d.y);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["1 1 7 5 6"]
    );
}

#[test]
fn locals_hide_static_rand_properties_in_rand_mode_calls() {
    // A formal or local handle `p` of a method hides the class's static
    // rand property `p` (IEEE 1800-2023 6.21): `p.rand_mode(0)` sets the
    // modes of the object it refers to (18.8), every random variable of it.
    let source = "class C;
        static rand int p;
        rand int q;
        function void formal(C p); p.rand_mode(0); endfunction
        function void local_(C other); C p = other; p.rand_mode(0); endfunction
    endclass
    module Top;
        C c, o1, o2;
        initial begin
            c = new; o1 = new; o2 = new;
            c.formal(o1);
            $display(\"%0d %0d\", o1.q.rand_mode(), c.q.rand_mode());
            o1.rand_mode(1);
            c.local_(o2);
            $display(\"%0d %0d\", o2.q.rand_mode(), c.q.rand_mode());
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["0 1", "0 1"]
    );
}

#[test]
fn scalar_locals_hide_static_rand_properties_in_rand_mode_calls() {
    // Any nearer variable `p` of a method, not only a handle, hides the
    // class's static rand property `p` (IEEE 1800-2023 6.21):
    // `p.rand_mode(0)` is then a call on an integral variable, which has no
    // rand_mode, not the static property's.
    for method in [
        "function void f(); int p; p.rand_mode(0); endfunction",
        "function void f(int p); p.rand_mode(0); endfunction",
        "function int f(); int p; return p.rand_mode(); endfunction",
    ] {
        let source = format!(
            "class C; static rand int p; {method} endclass
            module Top; C c; initial begin c = new; void'(c.f()); end endmodule"
        );
        let error = class_error(&source, method);
        assert!(error.contains("rand_mode"), "{method}: {error}");
    }
}

#[test]
fn static_rand_mode_getters_are_ints() {
    // The rand_mode getter of a static rand property is an `int` (IEEE
    // 1800-2023 18.8), compared and added as a signed 32-bit value.
    let source = "class C; static rand int p; endclass
    module Top;
        C c;
        initial begin
            c = new;
            $display(\"%0d %0d %0d\", c.p.rand_mode() < -1, c.p.rand_mode() + -2, $bits(c.p.rand_mode()));
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["0 -1 32"]
    );
}

#[test]
fn object_property_expressions_name_the_static_property_that_hides() {
    // `d.p` names the member `p` denotes in the class of `d` (IEEE 1800-2023
    // 8.14): a derived static rand `p` that hides a base instance `p`, which
    // a base handle still reaches.
    let source = "class Base; int p; endclass
    class Derived extends Base; static rand int p; endclass
    module Top;
        Derived d;
        Base b;
        initial begin
            d = new;
            b = d;
            d.p = 5;
            b.p = 9;
            $display(\"%0d %0d %0d\", Derived::p, d.p, b.p);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["5 5 9"]);
}

#[test]
fn static_properties_and_unpacked_structure_properties_hide_each_other() {
    // One lookup decides the member `p` denotes in the class of `d` (IEEE
    // 1800-2023 8.14), whether one of them is of unpacked structure type: a
    // derived static `p` hides a base structure `p`, which a base handle
    // still reaches, and a derived structure `p` hides a base static `p`.
    let source = "typedef struct { int a; int b; } s_t;
    class Base; s_t p; endclass
    class Derived extends Base; static int p; endclass
    class Root; static int p; endclass
    class Leaf extends Root; s_t p; endclass
    module Top;
        Derived d;
        Base b;
        Leaf l;
        initial begin
            d = new;
            b = d;
            l = new;
            d.p = 5;
            b.p.a = 9;
            l.p.a = 3;
            Root::p = 4;
            $display(\"%0d %0d %0d %0d %0d\", Derived::p, d.p, b.p.a, l.p.a, Root::p);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["5 5 9 3 4"]
    );
}

#[test]
fn wide_static_rand_properties_without_constraints_are_randomized() {
    // A static rand scalar wider than 64 bits no constraint names is drawn
    // as an instance one is, at its width (IEEE 1800-2023 18.4).
    let source = "class C; static rand bit [99:0] w; endclass
    module Top;
        C c;
        int ok, high;
        initial begin
            c = new;
            ok = 1;
            repeat (8) begin
                ok &= c.randomize();
                high += |C::w[99:64];
            end
            $display(\"%0d %0d\", ok, high >= 6);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1 1"]);
}

#[test]
fn distributions_constrain_static_rand_properties() {
    // A distribution over a static rand property constrains the variable
    // the call solves for it (IEEE 1800-2023 18.4, 18.5.3).
    let source = "class C;
        static rand bit [7:0] s;
        constraint c { s dist {[0:3] :/ 1, [10:13] :/ 1}; }
    endclass
    module Top;
        C c;
        int ok;
        initial begin
            c = new;
            ok = 1;
            repeat (20) ok &= c.randomize() && (C::s inside {[0:3], [10:13]});
            $display(\"%0d\", ok);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["1"]);
}

#[test]
fn static_rand_properties_without_constraints_are_randomized() {
    // A class whose only random variables are static rand properties, and
    // that has no constraints, still solves them (IEEE 1800-2023 18.4).
    let source = "class C; static rand bit [15:0] s; endclass
    module Top;
        C c;
        int ok, changed;
        bit [15:0] previous;
        initial begin
            c = new;
            repeat (8) begin
                previous = C::s;
                ok += c.randomize();
                changed += C::s != previous;
            end
            $display(\"%0d %0d\", ok, changed >= 7);
        end
    endmodule";
    assert_eq!(super::hierarchical_references::displayed(source), ["8 1"]);
}

#[test]
fn fixed_size_array_properties_hold_their_elements() {
    // A fixed-size array property (IEEE 1800-2023 7.4, 8.4): initialized,
    // indexed by its range, compared and formatted as an array, iterated
    // from left to right, reordered, and copied by a shallow copy and by an
    // assignment, after which the copies are independent (8.12, 7.6).
    let source = "typedef struct { int a; byte b; } pair_t;
    class C;
        int tab[5] = '{1, 2, 3, 4, 5};
        int down[4:1] = '{40, 30, 20, 10};
        int from[2:6] = '{1, 2, 3, 4, 5};
        pair_t pairs[2];
    endclass
    module Top;
        C c, d;
        int s;
        pair_t p;
        initial begin
            c = new;
            $display(\"%0d %0d %0d %0d\", c.tab[0], c.tab[4], c.down[4], c.down[1]);
            c.tab[2] = 33;
            $display(\"%p\", c.tab);
            d = new c;
            d.tab[0] = 100;
            $display(\"%0d %0d %0d %0d\", c.tab[0], d.tab[0], c.tab == d.tab, c.tab != d.tab);
            d.tab = c.tab;
            d.tab[1] = 7;
            $display(\"%0d %0d %0d\", c.tab[1], d.tab[1], c.tab == d.tab);
            s = 0;
            foreach (c.down[i]) s += i * c.down[i];
            $display(\"%0d\", s);
            s = 0;
            foreach (c.from[i]) s += i * c.from[i];
            $display(\"%0d\", s);
            c.tab.sort();
            $display(\"%p %0d\", c.tab, c.tab.sum());
            c.tab.shuffle();
            $display(\"%0d %0d\", c.tab.sum(), c.tab.max()[0]);
            p.a = 7; p.b = 8;
            c.pairs[1] = p;
            d = new c;
            p.a = 9;
            d.pairs[1] = p;
            $display(\"%0d %0d %0d %0d\", c.pairs[1].a, c.pairs[1].b, c.pairs[0].a, d.pairs[1].a);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        [
            "1 5 40 10",
            "'{1, 2, 33, 4, 5}",
            "1 100 0 1",
            "2 7 0",
            "300",
            "70",
            "'{1, 2, 4, 5, 33} 45",
            "45 33",
            "7 8 0 9",
        ]
    );
}

#[test]
fn queries_of_fixed_size_array_properties_are_constants() {
    // However an object holds them, the properties are of fixed-size array
    // types (IEEE 1800-2023 20.7): their queries are constants, and size
    // a declaration.
    let source = "class C;
        int tab[5];
        bit [3:0] nib[4:1];
    endclass
    module Top;
        C c;
        bit [$bits(c.tab)-1:0] wide;
        bit [$size(c.tab)-1:0] each;
        initial begin
            c = new;
            $display(\"%0d %0d %0d %0d %0d %0d\", $left(c.tab), $right(c.tab), $size(c.tab),
                $bits(c.tab), $dimensions(c.tab), $bits(wide) + $bits(each));
            $display(\"%0d %0d %0d %0d %0d %0d\", $left(c.nib), $right(c.nib), $low(c.nib),
                $high(c.nib), $increment(c.nib), $bits(c.nib));
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["0 4 5 160 2 165", "4 1 1 4 1 16"]
    );
}

#[test]
fn locators_of_fixed_size_array_properties_take_their_indices() {
    // The index methods and `item.index` give the indices of the array's
    // range, from its left bound to its right one (IEEE 1800-2023 7.4.6,
    // 7.12.1, 7.12.4).
    let source = "class C;
        int from[2:5] = '{5, 7, 5, 9};
        int down[4:1] = '{1, 2, 3, 2};
        int tab[4] = '{5, 7, 5, 9};
    endclass
    module Top;
        C c;
        initial begin
            c = new;
            $display(\"%p %p %p\", c.from.find_index with (item == 5),
                c.from.find_first_index with (item.index > 2 && item == 5),
                c.from.unique_index);
            $display(\"%p %p %p\", c.down.find_index with (item == 2),
                c.down.find_last_index with (item == 2), c.down.find with (item.index == 3));
            $display(\"%p %p\", c.tab.find_index with (item == 5), c.from.find with (item.index == 3));
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        [
            "'{2, 4} '{4} '{2, 3, 5}",
            "'{3, 1} '{1} '{2}",
            "'{0, 2} '{7}",
        ]
    );
}

#[test]
fn fixed_size_array_properties_take_dimension_queries_and_class_parameters() {
    // A query names the array's own dimension as 1 (IEEE 1800-2023 20.7),
    // and an element type sized by a class parameter keeps its size outside
    // the class (8.25).
    let source = "package p;
        class C #(int W = 4);
            bit [W-1:0] tab[3];
            bit [3:0] nib[4:1];
        endclass
    endpackage
    module Top;
        p::C#(8) c;
        initial begin
            c = new;
            $display(\"%0d %0d %0d %0d %0d %0d\", $size(c.tab, 1), $left(c.nib, 1), $bits(c.tab),
                $bits(c.tab[0]), $size(c.nib, 2), $left(c.tab, 2));
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["3 4 24 8 4 7"]
    );
}

#[test]
fn const_fixed_size_array_properties_are_written_by_their_constructor_only() {
    // Every write of an element of a const property, and every method that
    // changes it, writes the property (IEEE 1800-2023 8.19): the
    // constructor of its class writes one without an initializer, and
    // nothing else writes either.
    let source = "class C;
        const int k[2] = '{1, 2};
        const int m[2];
        function new(); m[0] = 4; m[1] = 3; endfunction
    endclass
    module Top;
        C c;
        initial begin
            c = new;
            $display(\"%p %p\", c.k, c.m);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["'{1, 2} '{4, 3}"]
    );
    for statement in [
        "c.k[0] = 3;",
        "c.k[1] += 1;",
        "c.m[0]++;",
        "c.k.sort();",
        "c.m.shuffle();",
        "c.k.rsort with (item);",
    ] {
        let source = format!(
            "class C;
                const int k[2] = '{{1, 2}};
                const int m[2];
                function new(); m[0] = 4; endfunction
            endclass
            module Top;
                C c;
                initial begin c = new; {statement} end
            endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build()
        .expect_err(statement);
        assert!(
            error
                .to_string()
                .contains("assignment to const class property"),
            "{statement}: {error}"
        );
    }
}

#[test]
fn fixed_size_array_property_bounds_outside_int_are_rejected() {
    // An index of a property is an `int`, as is a foreach index over it and
    // one a locator method returns (IEEE 1800-2023 12.7.3, 7.12.1), so a
    // bound outside `int` is rejected at the declaration.
    for property in [
        "int a[64'd4294967296:64'd4294967297];",
        "int a[64'd4294967297:64'd4294967296];",
        "int a[0:64'd2147483648];",
    ] {
        let source = format!(
            "class C; {property} endclass
            module Top;
                C c;
                initial begin c = new; $display(\"%p\", c.a.find with (item == 1)); end
            endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build()
        .expect_err(property);
        assert!(
            error
                .to_string()
                .contains("fixed-size array class property with a bound outside int"),
            "{property}: {error}"
        );
    }
}

#[test]
fn fixed_size_array_properties_are_not_written_through_other_places() {
    // An assignment to a member of a structure element, and the formal of
    // an output or inout argument, would write the container that holds
    // the property through a place that does not reach it (see
    // `Property::storage`): each is rejected, never accepted and dropped.
    for (statement, message) in [
        (
            "c.pairs[0].a = 1;",
            "assignment to a member of an element of a dynamic container or a fixed-size array class property",
        ),
        (
            "c.pairs[1].a++;",
            "assignment to a member of an element of a dynamic container or a fixed-size array class property",
        ),
        (
            "c.pairs[0].b += 1;",
            "assignment to a member of an element of a dynamic container or a fixed-size array class property",
        ),
        (
            "out(c.tab);",
            "fixed-size array class property as an output or inout argument",
        ),
        (
            "both(c.tab);",
            "fixed-size array class property as an output or inout argument",
        ),
        (
            "both((c.tab));",
            "fixed-size array class property as an output or inout argument",
        ),
        (
            "x = get(c.tab);",
            "fixed-size array class property as an output or inout argument",
        ),
        (
            "c.fill(c.tab);",
            "fixed-size array class property as an output or inout argument",
        ),
    ] {
        let source = format!(
            "typedef struct {{ int a; byte b; }} pair_t;
            class C;
                int tab[2];
                pair_t pairs[2];
                function void fill(output int a[2]); a = '{{1, 2}}; endfunction
            endclass
            task automatic out(output int a[2]); a = '{{3, 4}}; endtask
            task automatic both(inout int a[2]); a[0] = 5; endtask
            task automatic scalar(output int a); a = 6; endtask
            function automatic int get(output int a[2]); a = '{{7, 8}}; return 1; endfunction
            module Top;
                C c;
                int x;
                initial begin c = new; {statement} end
            endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build()
        .expect_err(statement);
        assert!(error.to_string().contains(message), "{statement}: {error}");
    }
    // The member of an element as the actual of an output argument is
    // written, as an element of a dynamic container is (13.5.1).
    let source = "typedef struct { int a; byte b; } pair_t;
        class C; pair_t pairs[2]; endclass
        task automatic scalar(output int a); a = 6; endtask
        module Top;
            C c;
            initial begin c = new; scalar(c.pairs[1].a); $display(\"%p\", c.pairs); end
        endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["'{'{a:0, b:0}, '{a:6, b:0}}"]
    );
}

#[test]
fn fixed_size_array_properties_meet_ordinary_arrays() {
    // An ordinary array of as many elements of an equivalent type is
    // compared with and assigned to the property element by element, from
    // left to right (IEEE 1800-2023 7.6, 11.4.5), and `$bits` counts the
    // members of structure elements (20.6.2).
    let source = "typedef struct { int a; byte b; } pair_t;
    class C;
        int tab[2] = '{1, 2};
        pair_t pairs[2];
    endclass
    module Top;
        C c;
        int m[1:2] = '{1, 2};
        int d[2:1] = '{5, 6};
        initial begin
            c = new;
            $display(\"%0d %0d %0d %0d\", c.tab == m, m == c.tab, c.tab != m, c.tab == d);
            c.tab = d;
            $display(\"%p %0d %0d\", c.tab, c.tab[0], $bits(c.pairs));
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["1 1 0 0", "'{5, 6} 5 80"]
    );
    for (statement, message) in [
        (
            "m = c.tab[1:0];",
            "slice of a fixed-size array class property",
        ),
        (
            "t(c.tab);",
            "fixed-size array class property as a ref or const ref argument",
        ),
    ] {
        let source = format!(
            "class C; int tab[2]; endclass
            module Top;
                C c;
                int m[2];
                task automatic t(ref int a[2]); endtask
                initial begin c = new; {statement} end
            endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build()
        .expect_err(statement);
        assert!(error.to_string().contains(message), "{statement}: {error}");
    }
}

#[test]
fn fixed_size_array_properties_take_ordinary_array_checks() {
    // `foreach` takes each index of a range up to a bound of `int`, of a
    // property as of an ordinary array (IEEE 1800-2023 12.7.3), and of a
    // range outside `int` of an ordinary array; stepping past the last
    // index does not wrap into another iteration.
    let source = "class C;
        int w[2147483645:2147483647] = '{1, 2, 3};
        int e[2147483647:2147483647] = '{9};
    endclass
    module Top;
        C c;
        int m[64'd4294967297:64'd4294967296] = '{4, 5};
        int x[2147483647:2147483647] = '{7};
        int y[-2147483647:-2147483648] = '{2, 3};
        longint s, n, t, u;
        initial begin
            c = new;
            foreach (c.w[i]) begin s += (i - 2147483645) * c.w[i]; n++; end
            foreach (c.e[i]) begin s += c.e[i]; n++; end
            foreach (m[i]) t += (i - 64'd4294967296) * m[i];
            foreach (x[i]) u += x[i];
            foreach (y[i]) u += y[i] * (i + 2147483647 + 2);
            $display(\"%0d %0d %0d %0d\", s, n, t, u);
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["17 4 4 14"]
    );
    // A count a container cannot hold, and elements whose members have
    // initial values, which a container does not give them, are rejected.
    for (property, message) in [
        (
            "int a[0:16777216];",
            "fixed-size array class property of more than 16777216 elements",
        ),
        (
            "row_t rows[1];",
            "fixed-size array class property of an unpacked structure with member initial values",
        ),
    ] {
        let source = format!(
            "typedef struct {{ bit a = 1; }} row_t;
            class C; {property} endclass
            module Top; C c; initial c = new; endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build()
        .expect_err(property);
        assert!(error.to_string().contains(message), "{property}: {error}");
    }
}

#[test]
fn fixed_size_array_properties_keep_their_size_and_element_type() {
    // `delete` is no method of a fixed-size array (IEEE 1800-2023 7.5.3),
    // and arrays of unrelated enums are not of equivalent types, so neither
    // is assigned or compared to the other (6.22.2, 7.6).
    for (statement, message) in [
        ("c.x.delete();", "delete"),
        ("c.x = c.y;", "fixed-size array class property"),
        (
            "b = c.x == c.y;",
            "comparison of a fixed-size array class property",
        ),
    ] {
        let source = format!(
            "typedef enum {{A, B}} e1;
            typedef enum {{P, Q}} e2;
            class C;
                e1 x[2];
                e2 y[2];
            endclass
            module Top;
                C c;
                bit b;
                initial begin c = new; {statement} end
            endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("class.sv"))],
            "Top",
        )
        .build()
        .expect_err(statement);
        assert!(error.to_string().contains(message), "{statement}: {error}");
    }
}

#[test]
fn constraints_read_fixed_size_array_properties() {
    // A constraint iterates the elements of a state array (IEEE 1800-2023
    // 18.5.8.1); only `x = 0` avoids them all.
    let source = "class C;
        rand int x;
        int tab[3] = '{1, 2, 3};
        constraint c { foreach (tab[i]) x != tab[i]; x inside {[0:3]}; }
    endclass
    module Top;
        C c;
        initial begin
            c = new;
            repeat (4) begin
                void'(c.randomize());
                $display(\"%0d\", c.x);
            end
        end
    endmodule";
    assert_eq!(
        super::hierarchical_references::displayed(source),
        ["0", "0", "0", "0"]
    );
}
