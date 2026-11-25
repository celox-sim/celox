//! Covergroups (IEEE 1800-2023 clause 19): the types, their instances and
//! sampling them, read through the coverage report of the simulation.
use super::*;

fn simulation(source: &str) -> celox::SimulatorBuilder<'_, celox::Simulation> {
    celox::Simulation::from_sv_sources(vec![(source, Path::new("covergroup.sv"))], "Top")
}

/// The covergroup type `name` of `report`.
fn group<'a>(report: &'a celox::CoverageReport, name: &str) -> &'a celox::CovergroupReport {
    report
        .covergroups
        .iter()
        .find(|group| group.name == name)
        .unwrap_or_else(|| panic!("no covergroup `{name}` in {report:?}"))
}

/// Every covergroup type of `report` with the samples of its instances.
fn all_samples(report: &celox::CoverageReport) -> Vec<(String, Vec<u64>)> {
    report
        .covergroups
        .iter()
        .map(|group| {
            (
                group.name.clone(),
                group
                    .instances
                    .iter()
                    .map(|instance| instance.samples)
                    .collect(),
            )
        })
        .collect()
}

fn expected(groups: &[(&str, &[u64])]) -> Vec<(String, Vec<u64>)> {
    groups
        .iter()
        .map(|(name, samples)| (name.to_string(), samples.to_vec()))
        .collect()
}

/// The value of `real` output `name`, which the design assigns with
/// `$realtobits`.
fn real<B: celox::SimBackend>(sim: &mut celox::Simulation<B>, name: &str) -> f64 {
    let signal = sim.signal(name);
    let bits = sim
        .get(signal)
        .to_u64_digits()
        .first()
        .copied()
        .unwrap_or(0);
    f64::from_bits(bits)
}

/// The number of samples of each instance of covergroup type `name`.
fn samples(report: &celox::CoverageReport, name: &str) -> Vec<u64> {
    group(report, name)
        .instances
        .iter()
        .map(|instance| instance.samples)
        .collect()
}

sv_backends! {
    /// 19.8: each call of `sample()` samples the instance once.
    fn sample_counts_once_per_call(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    covergroup cg; endgroup
    cg c = new;
    initial begin
        c.sample();
        c.sample();
        #1 c.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(0).unwrap();
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [2]);
        sim.run_until(2).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [3]);
    }
}

sv_backends! {
    /// 19.3: a covergroup is a type of the scope that declares it, a
    /// module, a program, a package, the compilation unit or a generate
    /// block, and 19.4: one of a class is a variable of the class, of each
    /// specialization its own. `new`, `new()` and `new(arguments)`, by
    /// position, by name and with a default, construct instances.
    fn covergroup_types_construct_in_every_scope(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
covergroup ucg(int a); endgroup
package p;
    covergroup pcg; endgroup
    class holder #(type T = int);
        covergroup hcg(int unsigned n); endgroup
        function new(); hcg = new(3); endfunction
    endclass
endpackage
program prog;
    covergroup prcg; endgroup
    prcg q = new();
    initial q.sample();
endprogram
module Top(output int done);
    import p::*;
    covergroup mcg(bit [3:0] lo, string label = "x"); endgroup
    mcg m0 = new(4'd1);
    mcg m1 = new(.label("b"), .lo(4'd2));
    ucg u = new(7);
    pcg pc = new;
    p::pcg pc2 = new();
    if (1) begin : g
        covergroup gcg; endgroup
        gcg x = new;
        initial begin x.sample(); x.sample(); end
    end
    holder #(int) h = new;
    holder #(bit) hb = new;
    prog pr();
    initial begin
        m0.sample(); m1.sample(); u.sample(); pc.sample(); pc2.sample();
        h.hcg.sample(); hb.hcg.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        // Each specialization of the class has a covergroup type of its
        // own, named with the specialization.
        let (embedded, declared): (Vec<_>, Vec<_>) = all_samples(&sim.coverage())
            .into_iter()
            .partition(|(name, _)| name.starts_with("p::holder#(") && name.ends_with("::hcg"));
        assert_eq!(embedded.len(), 2, "{embedded:?}");
        assert!(embedded.iter().all(|(_, samples)| samples == &[1]), "{embedded:?}");
        assert_eq!(
            declared,
            expected(&[
                ("$unit::ucg", &[1]),
                ("Top.g.gcg", &[2]),
                ("Top.mcg", &[1, 1]),
                ("p::pcg", &[1, 1]),
                ("prog.prcg", &[1]),
            ])
        );
    }

    /// A covergroup of a package that only a module constructs and samples:
    /// the module takes the subroutines the package made for the type.
    fn package_covergroup_constructed_only_in_a_module(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
package p;
    covergroup pcg(int a = 1); endgroup
    int unused;
endpackage
module Top(output int done);
    p::pcg c;
    initial begin
        c = new;
        c.sample();
        #1 c.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(2).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("p::pcg", &[2])]));
    }

    /// 19.4: a covergroup embedded in a class is sampled through a handle
    /// of the class, also from a method of another class.
    fn embedded_covergroup_sampled_from_another_class(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
package p;
    class A;
        covergroup cg; endgroup
        function new(); cg = new; endfunction
        function void own(); cg.sample(); endfunction
    endclass
    class B;
        A a = new;
        function void hit(); a.cg.sample(); endfunction
    endclass
endpackage
module Top(output int done);
    p::B b = new;
    initial begin
        b.hit();
        b.a.own();
        b.a.cg.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("p::A::cg", &[3])]));
    }

    /// 19.4: an embedded covergroup is a property of its class, `null`
    /// until the constructor assigns it, and each object has its own
    /// instance.
    fn embedded_covergroup_is_a_property(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [3:0] q, output int done);
    class C;
        covergroup cg; endgroup
        function new(bit make); if (make) cg = new; endfunction
    endclass
    class D extends C;
        function new(); super.new(1); endfunction
        function bit made(); return cg != null; endfunction
    endclass
    C a = new(0);
    C b = new(1);
    D d = new;
    initial begin
        q[0] = a.cg == null;
        q[1] = b.cg != null;
        q[2] = d.made();
        q[3] = b.cg != d.cg;
        b.cg.sample();
        d.cg.sample();
        d.cg.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let (q, done) = (sim.signal("q"), sim.signal("done"));
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(sim.get(q), BigUint::from(0b1111u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("C::cg", &[1, 2])]));
    }

    /// 19.8: `sample()` from an `initial` procedure, a task, a function, the
    /// branches of a fork and an `always` procedure with a delay.
    fn sample_from_processes_tasks_and_forks(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    covergroup cg; endgroup
    cg c;
    task automatic later(int delay); #delay c.sample(); endtask
    function automatic void now(); c.sample(); endfunction
    always #2 c.sample();
    initial begin
        c = new;
        c.sample();
        now();
        later(1);
        fork
            c.sample();
            #1 c.sample();
            later(2);
        join
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        // The `always` samples at 2 and 4; the rest are the six calls.
        sim.run_until(5).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("Top.cg", &[8])]));
    }

    /// `sample()` from `always_comb` samples once for the evaluation at time
    /// zero and once for each change of what the procedure reads.
    fn sample_in_always_comb_runs_per_change(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output logic [3:0] y);
    covergroup cg; endgroup
    cg c = new;
    logic [3:0] a = 0;
    always_comb begin
        y = a + 4'd1;
        c.sample();
    end
    initial begin
        #1 a = 1;
        #1 a = 2;
        #1 a = 2;
        #1 a = 3;
        #1 done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(6).unwrap();
        let (done, y) = (sim.signal("done"), sim.signal("y"));
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(sim.get(y), BigUint::from(4u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("Top.cg", &[4])]));
    }

    /// `sample()` in the action of a `cover property` samples at each
    /// match.
    fn sample_in_cover_property_action(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    covergroup cg; endgroup
    cg c = new;
    bit clk = 0;
    bit [3:0] n = 0;
    always #1 clk = ~clk;
    always @(posedge clk) n <= n + 4'd1;
    cover property (@(posedge clk) n[0]) c.sample();
    initial #12 done = 1;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        // Edges at 1, 3, ..., 11 sample n = 0..5: odd at three of them.
        sim.run_until(13).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("Top.cg", &[3])]));
    }

    /// 19.4: `sample()` on a covergroup that was not constructed samples
    /// nothing, an embedded one and a variable of a covergroup type alike.
    fn sample_guards_null(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    covergroup cg; endgroup
    class C;
        covergroup ecg; endgroup
        function void hit(); ecg.sample(); endfunction
    endclass
    cg never;
    cg made = new;
    C c = new;
    initial begin
        never.sample();
        c.ecg.sample();
        c.hit();
        made.sample();
        made = null;
        made.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("Top.cg", &[1])]));
    }

    /// 8.12: a shallow copy of an object gets `null` for its embedded
    /// covergroup, and the original keeps its instance.
    fn shallow_copy_nulls_embedded_handle(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [2:0] q, output int done);
    class C;
        int v;
        covergroup cg; endgroup
        function new(); cg = new; endfunction
    endclass
    C a = new;
    C b;
    initial begin
        a.v = 5;
        b = new a;
        q[0] = b.cg == null;
        q[1] = a.cg != null;
        q[2] = b.v == 5;
        b.cg.sample();
        a.cg.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let (q, done) = (sim.signal("q"), sim.signal("done"));
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(sim.get(q), BigUint::from(0b111u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("C::cg", &[1])]));
    }

    /// 19.8, 19.11: a covergroup without coverpoints has no coverage, and
    /// `$get_coverage` of a design whose covergroups have none is 100. The
    /// covergroups are of two source files. The value is 100 whether it is
    /// the constant or the answer of the heap, so that the choice is made
    /// from the whole design and not from the file that reads it is pinned
    /// on the lowered design (`get_coverage_folds_over_every_source_file`
    /// among the tests of the builder).
    fn coverage_of_an_empty_covergroup(sim) {
        @setup {
            let declaring = r#"`timescale 1ns/1ns
module Sub;
    covergroup cg; endgroup
    cg c = new;
    initial c.sample();
endmodule
"#;
            let reading = r#"`timescale 1ns/1ns
module Top(output longint unsigned inst, ty, scoped, all, early, output int done);
    covergroup cg; endgroup
    cg c;
    Sub sub();
    initial begin
        early = $realtobits(cg::get_coverage());
        c = new;
        c.sample();
        inst = $realtobits(c.get_inst_coverage());
        ty = $realtobits(c.get_coverage());
        scoped = $realtobits(cg::get_coverage());
        all = $realtobits($get_coverage());
        done = 1;
    end
endmodule
"#;
        }
        @build celox::Simulation::from_sv_sources(
            vec![
                (reading, Path::new("reading.sv")),
                (declaring, Path::new("declaring.sv")),
            ],
            "Top",
        )
        .four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        for name in ["inst", "ty", "scoped", "early"] {
            assert_eq!(real(&mut sim, name), 0.0, "{name}");
        }
        assert_eq!(real(&mut sim, "all"), 100.0);
        let report = sim.coverage();
        assert_eq!(report.coverage, 100.0);
        assert_eq!(
            all_samples(&report),
            expected(&[("Sub.cg", &[1]), ("Top.cg", &[1])])
        );
    }

    /// Covergroup handles are class handles: elements of fixed arrays,
    /// queues, dynamic and associative arrays, arguments and results.
    fn handle_arrays_and_containers(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output bit [2:0] q, output int done);
    covergroup cg; endgroup
    cg a[3];
    cg queue[$];
    cg dynamic[];
    cg by_name[string];
    function automatic cg pick(cg from[3], int index); return from[index]; endfunction
    initial begin
        foreach (a[i]) a[i] = new();
        a[1].sample();
        queue.push_back(a[0]);
        queue[0].sample();
        dynamic = new[2];
        dynamic[1] = a[2];
        dynamic[1].sample();
        dynamic[1].sample();
        by_name["x"] = pick(a, 0);
        by_name["x"].sample();
        q[0] = queue[0] == a[0];
        q[1] = dynamic[0] == null;
        q[2] = a[0] != a[1];
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let (q, done) = (sim.signal("q"), sim.signal("done"));
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(sim.get(q), BigUint::from(0b111u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("Top.cg", &[2, 1, 2])]));
    }

    /// A method or a function named as a predefined method of a covergroup
    /// is the user's wherever its receiver is no covergroup.
    fn user_methods_named_sample_do_not_collide(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int n, total, done);
    covergroup cg; endgroup
    class C;
        int samples;
        covergroup cg; endgroup
        function new(); cg = new; endfunction
        function void sample(); samples++; cg.sample(); endfunction
        function int get_coverage(int scale); return samples * scale; endfunction
        function void start(); samples += 10; endfunction
    endclass
    function automatic int get_inst_coverage(int x); return x + 1; endfunction
    C c = new;
    cg g = new;
    initial begin
        c.sample();
        c.sample();
        c.start();
        g.sample();
        n = c.get_coverage(2);
        total = get_inst_coverage(c.samples);
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let (n, total, done) = (sim.signal("n"), sim.signal("total"), sim.signal("done"));
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(sim.get(n), BigUint::from(24u8));
        assert_eq!(sim.get(total), BigUint::from(13u8));
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[("C::cg", &[2]), ("Top.cg", &[1])])
        );
    }

    /// The coverage of a covergroup is part of a checkpoint: restoring one
    /// restores the samples it was taken with.
    fn coverage_rolls_back_with_a_checkpoint(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    covergroup cg; endgroup
    cg c = new;
    always #1 c.sample();
    initial #10 done = 1;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(3).unwrap();
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [3]);
        let checkpoint = sim.checkpoint().unwrap();
        sim.run_until(7).unwrap();
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [7]);
        sim.restore(&checkpoint).unwrap();
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [3]);
        sim.run_until(5).unwrap();
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [5]);
    }

    /// 19.11: coverage is cumulative, so the samples of an instance stay in
    /// the report when nothing refers to its object any more and the heap
    /// collects it.
    fn instance_survives_collection(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(input int rounds, output int done);
    covergroup cg; endgroup
    class Junk; int v; endclass
    cg kept;
    Junk junk;
    initial begin
        #1;
        // One `new` and one `sample()`, each run for every instance, in a
        // loop whose bound the host gives: each construction replaces every
        // handle of the instance before it, those in the temporaries of
        // the calls too.
        for (int i = 0; i < rounds; i++) begin
            kept = new;
            for (int k = i; k < rounds; k++) kept.sample();
        end
        // Enough allocation for a collection between time slots, in a
        // loop of the same kind.
        repeat (4) begin
            for (int i = 0; i < rounds * 1000; i++) begin
                junk = new;
                junk.v = i;
            end
            #1;
        end
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        let rounds = sim.signal("rounds");
        sim.modify(|io| io.set(rounds, 3u8)).unwrap();
        sim.run_until(6).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        let instances: Vec<(u64, bool)> = group(&report, "Top.cg")
            .instances
            .iter()
            .map(|instance| (instance.samples, instance.live))
            .collect();
        assert_eq!(instances, [(3, false), (2, false), (1, true)]);
    }
}

sv_backends! {
    /// A property a class declares with the type of a covergroup of a
    /// package or of a module is a handle like any other, where 19.4
    /// restricts the variable of an embedded covergroup alone: methods
    /// assign and sample it, and so does what holds a handle of the object.
    fn handle_properties_of_covergroup_types(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
package p;
    covergroup pcg; endgroup
    class Holder;
        pcg h;
        function void set(pcg x); h = x; endfunction
        function void hit(); h.sample(); endfunction
    endclass
endpackage
module Top(output bit [1:0] q, output int done);
    covergroup cg; endgroup
    class Local;
        cg h;
        function new(); h = new; endfunction
        function void renew(); h = new; h.sample(); endfunction
    endclass
    p::Holder holder = new;
    p::pcg g = new;
    Local l = new;
    initial begin
        holder.set(g);
        holder.hit();
        holder.h.sample();
        q[0] = holder.h == g;
        holder.h = null;
        q[1] = holder.h == null;
        g.sample();
        l.h.sample();
        l.renew();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let (q, done) = (sim.signal("q"), sim.signal("done"));
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(sim.get(q), BigUint::from(0b11u8));
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[("Top.cg", &[1, 1]), ("p::pcg", &[3])])
        );
    }
}

sv_backends! {
    /// The commonest way to sample: from clocked logic, here under the
    /// untimed `Simulator`, which constructs the covergroup in the clocked
    /// procedure. Each tick samples once, at every optimization level.
    fn sample_in_ff_always_runs_once_per_edge(sim) {
        @setup {
            let source = r#"module Top(input bit clk, input logic [7:0] d, output logic [7:0] q);
    covergroup cg; endgroup
    cg c;
    always_ff @(posedge clk) q <= q + d;
    always @(posedge clk) begin
        if (c == null) c = new;
        c.sample();
    end
endmodule
"#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("covergroup_ff.sv"))], "Top")
            .four_state(false);
        let clk = sim.event("clk");
        let (d, q) = (sim.signal("d"), sim.signal("q"));
        sim.modify(|io| io.set(d, 3u8)).unwrap();
        for tick in 1..=5u64 {
            sim.tick(clk).unwrap();
            assert_eq!(samples(&sim.coverage(), "Top.cg"), [tick]);
        }
        assert_eq!(sim.get(q), BigUint::from(15u8));
    }
}

/// The same at each optimization level.
#[test]
fn sample_in_ff_always_runs_once_per_edge_at_every_level() {
    let source = r#"module Top(input bit clk, input logic [7:0] d, output logic [7:0] q);
    covergroup cg; endgroup
    cg c;
    always_ff @(posedge clk) q <= q + d;
    always @(posedge clk) begin
        if (c == null) c = new;
        c.sample();
    end
endmodule
"#;
    for level in [
        celox::OptLevel::O0,
        celox::OptLevel::O1,
        celox::OptLevel::O2,
    ] {
        let mut sim =
            Simulator::from_sv_sources(vec![(source, Path::new("covergroup_ff.sv"))], "Top")
                .four_state(false)
                .opt_level(level)
                .build()
                .unwrap();
        let clk = sim.event("clk");
        for tick in 1..=4u64 {
            sim.tick(clk).unwrap();
            assert_eq!(samples(&sim.coverage(), "Top.cg"), [tick], "{level:?}");
        }
    }
}

/// A covergroup variable declared with an initializer in a module is
/// constructed by a process at time zero, which the untimed `Simulator` does
/// not run: the design builds as a `Simulation` and is rejected as a
/// `Simulator`.
#[test]
fn module_scope_construction_requires_simulation() {
    let source = "`timescale 1ns/1ns\nmodule Top(input bit clk);\n    covergroup cg; endgroup\n    cg c = new;\n    always @(posedge clk) c.sample();\nendmodule\n";
    let sources = || vec![(source, Path::new("covergroup_initializer.sv"))];
    celox::Simulation::from_sv_sources(sources(), "Top")
        .build()
        .unwrap();
    let error = match Simulator::from_sv_sources(sources(), "Top").build() {
        Ok(_) => panic!("a declaration initializer needs a Simulation"),
        Err(error) => format!("{error:?}"),
    };
    assert!(error.contains("Simulation"), "{error}");
}

/// What a covergroup of a package does not support is reported where a
/// module constructs it or calls a method of it; a module that only
/// declares a variable of the type builds.
#[test]
fn unsupported_package_covergroup_reports_where_used() {
    let package = "package p;\n    bit a, b;\n    covergroup pcg(int x); coverpoint x; cross a, b; endgroup\nendpackage\n";
    let build = |body: &str| {
        let source = format!("`timescale 1ns/1ns\n{package}module Top;\n{body}\nendmodule\n");
        celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("unsupported.sv"))],
            "Top",
        )
        .build()
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
    };
    build("    p::pcg c;").unwrap();
    for body in [
        "    p::pcg c = new(1);",
        "    p::pcg c;\n    initial c = new(1);",
        "    p::pcg c;\n    initial c.sample();",
        "    p::pcg c;\n    real r;\n    initial r = c.get_inst_coverage();",
        "    real r;\n    initial r = p::pcg::get_coverage();",
    ] {
        let error = build(body).expect_err(body);
        assert!(error.contains("covergroup cross"), "{body}: {error}");
    }
}

/// Uses of a covergroup that the whole design shows and that are not
/// supported: a covergroup type that a module takes as a type parameter
/// constructs nothing there, where the type is neither declared nor
/// imported, and `sample()` is no action of a deferred assertion, as no
/// subroutine call is. A variable of the parameter type alone builds.
#[test]
fn covergroup_uses_across_the_design_are_rejected_by_name() {
    let build = |source: &str| {
        celox::Simulation::from_sv_sources(vec![(source, Path::new("across.sv"))], "Top")
            .build()
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    };
    let parameterized = |body: &str| {
        format!(
            "`timescale 1ns/1ns
module Sub #(type T = int);
    T v;
{body}
endmodule
module Top;
    covergroup cg; endgroup
    Sub #(.T(cg)) sub();
endmodule
"
        )
    };
    build(&parameterized("")).unwrap();
    for body in ["    initial v = new;", "    initial v.sample();"] {
        let error = build(&parameterized(body)).expect_err(body);
        assert!(
            error.contains("covergroup type used outside the scopes that declare or import it"),
            "{body}: {error}"
        );
    }
    let deferred = "`timescale 1ns/1ns
module Top;
    covergroup cg; endgroup
    cg c = new;
    bit a;
    always_comb assert #0 (a) c.sample();
endmodule
";
    let error = build(deferred).expect_err("a deferred assertion action");
    assert!(error.contains("deferred assertion action"), "{error}");
}

/// A state file does not record the coverage of covergroups: saving one is
/// refused once a covergroup was constructed, and so is loading one, which
/// would keep the instances and samples of the run it replaces.
#[test]
fn state_file_refuses_coverage() {
    let source = "`timescale 1ns/1ns\nmodule Top;\n    covergroup cg; endgroup\n    cg c;\n    initial begin #1 c = new; #1 c.sample(); end\nendmodule\n";
    let mut sim = simulation(source).build().unwrap();
    sim.run_until(0).unwrap();
    let saved = sim.save_state().unwrap();
    sim.load_state(&saved).unwrap();
    sim.run_until(1).unwrap();
    assert!(matches!(sim.save_state(), Err(celox::StateError::Coverage)));
    assert!(matches!(
        sim.load_state(&saved),
        Err(celox::StateError::Coverage)
    ));
    // Also once nothing refers to the instance: its samples remain.
    sim.run_until(2).unwrap();
    assert!(matches!(sim.save_state(), Err(celox::StateError::Coverage)));
    assert!(matches!(
        sim.load_state(&saved),
        Err(celox::StateError::Coverage)
    ));
    // The refused load left the run as it was.
    assert_eq!(samples(&sim.coverage(), "Top.cg"), [1]);
}

/// 18.14.1: constructing a covergroup draws no random number, so adding
/// coverage to a procedure or to a class constructor does not change its
/// random stimulus. (A covergroup variable declared with an initializer in a
/// module is a process of its own, as `C obj = new;` is, and takes a place
/// in the order the processes are seeded in.)
#[test]
fn procedural_construction_does_not_change_random_stimulus() {
    let draws = |construct: bool, in_class: bool| {
        fn when(construct: bool, text: &str) -> &str {
            if construct { text } else { "" }
        }
        let new = |text| when(construct, text);
        let source = format!(
            "`timescale 1ns/1ns
module Top(output int unsigned a, b, c, output int done);
    covergroup cg; endgroup
    class C;
        covergroup ecg; endgroup
        rand int unsigned v;
        function new(); {embedded} endfunction
    endclass
    cg g;
    C o;
    initial begin
        {scope}
        a = $urandom;
        o = new;
        void'(o.randomize());
        b = o.v;
        c = $urandom;
        done = 1;
    end
endmodule
",
            embedded = new(if in_class { "ecg = new;" } else { "" }),
            scope = new(if in_class { "" } else { "g = new; g.sample();" }),
        );
        let mut sim = simulation(&source).four_state(false).build().unwrap();
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        ["a", "b", "c"].map(|name| {
            let signal = sim.signal(name);
            sim.get(signal)
        })
    };
    let plain = draws(false, false);
    assert_ne!(plain[0], plain[2], "the stimulus is random");
    assert_eq!(draws(true, false), plain);
    assert_eq!(draws(true, true), plain);
}

/// A covergroup that a class declares and nothing constructs is a property
/// that stays `null`: the class builds, also one with coverpoints, which
/// are not supported yet, and a local of the name of the covergroup hides
/// it.
#[test]
fn class_covergroups_nothing_constructs_build() {
    let source = |body: &str| {
        format!(
            "`timescale 1ns/1ns
             class Base;
                 int v;
                 covergroup cg; coverpoint v; endgroup
             endclass
             class M extends Base;
                 {body}
             endclass
             module Top(output int seen);
                 initial begin M m = new; m.v = 3; seen = m.v; end
             endmodule"
        )
    };
    for body in ["", "function void s(); int cg; cg = 1; v = cg; endfunction"] {
        let source = source(body);
        let mut sim = simulation(&source)
            .build()
            .unwrap_or_else(|error| panic!("{body:?}: {error}"));
        let seen = sim.signal("seen");
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(seen), BigUint::from(3u8), "{body:?}");
        assert!(sim.coverage().covergroups.is_empty());
    }
}

sv_backends! {
    /// 19.3, 19.8.1: the arguments of a covergroup and of its sample method
    /// are inputs of any type but an unpacked one: packed values of any
    /// width, enumerations, strings and class handles, `null` among them.
    fn arguments_of_every_supported_type(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    typedef enum {A, B} e_t;
    class K; endclass
    covergroup cg(e_t e, K k, string s, bit [70:0] w)
        with function sample(e_t se, K sk, string ss, bit [70:0] sw);
    endgroup
    K k;
    cg c;
    initial begin
        k = new;
        c = new(B, k, "x", 71'd5);
        c.sample(A, k, "y", 71'd9);
        c.sample(.sw(1), .ss("z"), .sk(null), .se(B));
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("Top.cg", &[2])]));
    }

    /// A covergroup of a module is a type of the elaborated module (see the
    /// reference page, "Coverage"): instances with the same parameter values
    /// share the type, an elaboration with other values has a type of its
    /// own under the same name, and the instances of a type are named by
    /// the order they were constructed in.
    fn module_instances_share_the_types_of_their_elaboration(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Sub #(parameter int W = 1);
    covergroup cg; endgroup
    cg c = new;
    initial repeat (W) c.sample();
endmodule
module Top(output int done);
    Sub #(1) a();
    Sub #(1) b();
    Sub #(2) wide();
    initial #1 done = 1;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(2).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        let mut types: Vec<(String, Vec<(String, u64)>)> = report
            .covergroups
            .iter()
            .map(|group| {
                (
                    group.name.clone(),
                    group
                        .instances
                        .iter()
                        .map(|instance| (instance.name.clone(), instance.samples))
                        .collect(),
                )
            })
            .collect();
        types.sort();
        let named = |samples: &[u64]| -> Vec<(String, u64)> {
            samples
                .iter()
                .enumerate()
                .map(|(index, samples)| (format!("Sub.cg#{index}"), *samples))
                .collect()
        };
        assert_eq!(
            types,
            [
                ("Sub.cg".to_string(), named(&[1, 1])),
                ("Sub.cg".to_string(), named(&[2])),
            ]
        );
    }

    /// `sample()` in the action of an immediate assertion, of the pass and
    /// of the fail statement, samples when the statement runs.
    fn sample_in_immediate_assertion_action(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    covergroup cg; endgroup
    cg passed = new, failed = new;
    bit clk = 0;
    initial begin
        #1 assert (clk == 0) passed.sample(); else failed.sample();
        clk = 1;
        assert (clk == 0) passed.sample(); else failed.sample();
        assert (clk == 0) else failed.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(2).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(all_samples(&sim.coverage()), expected(&[("Top.cg", &[1, 2])]));
    }

    /// 19.8: `get_coverage()` is the coverage of the type, which a handle
    /// only names: through `null` it reads nothing. `get_inst_coverage()`
    /// is that of an instance, and through `null` a run-time error.
    fn coverage_queries_through_null(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output longint unsigned ty, output int reached, done);
    covergroup cg; endgroup
    cg never;
    real r;
    initial begin
        ty = $realtobits(never.get_coverage() + 1.0);
        reached = 1;
        #1 r = never.get_inst_coverage();
        #1 done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(0).unwrap();
        let reached = sim.signal("reached");
        assert_eq!(sim.get(reached), BigUint::from(1u8));
        assert_eq!(real(&mut sim, "ty"), 1.0);
        let error = sim.run_until(3).unwrap_err().to_string();
        assert!(error.contains("null"), "{error}");
        assert!(sim.coverage().covergroups.is_empty());
    }
}

/// The bins of item `item` of instance `instance` of covergroup type
/// `name` that have a value, each with its hits.
fn bins(
    report: &celox::CoverageReport,
    name: &str,
    instance: usize,
    item: &str,
) -> Vec<(String, u64)> {
    let instance = &group(report, name).instances[instance];
    instance
        .items
        .iter()
        .find(|found| found.name == item)
        .unwrap_or_else(|| panic!("no item `{item}` in {instance:?}"))
        .bins
        .iter()
        .map(|bin| (bin.name.clone(), bin.hits))
        .collect()
}

fn hits(bins: &[(&str, u64)]) -> Vec<(String, u64)> {
    bins.iter()
        .map(|(name, hits)| (name.to_string(), *hits))
        .collect()
}

/// The bins of `bins` that were hit.
fn hit(bins: Vec<(String, u64)>) -> Vec<(String, u64)> {
    bins.into_iter().filter(|(_, hits)| *hits != 0).collect()
}

sv_backends! {
    /// 19.5, 19.5.1: a coverpoint with bins of values and ranges counts a
    /// sample in every bin that holds its value; `$` is the lowest or the
    /// highest value of the coverpoint. The coverpoint is named by its
    /// label, else by the variable it is of, else by the analysis.
    fn coverpoint_scalar_bins_and_ranges(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst, ty);
    logic [3:0] a;
    bit signed [3:0] s;
    covergroup cg;
        coverpoint a {
            bins zero = {0};
            bins low = {[1:3], 5};
            bins high = {[12:$]};
            bins any = {[$:15]};
            bins none = {[7:6]};
        }
        neg: coverpoint s {
            bins minus = {[$:-1]};
            bins plus = {0, [1:$]};
        }
        coverpoint a + 4'd1 {
            bins wrapped = {0};
            bins two = {2};
        }
    endgroup
    cg c = new;
    initial begin
        a = 0; s = -8; c.sample();
        a = 1; s = -1; c.sample();
        a = 5; s = 0; c.sample();
        a = 15; s = 7; c.sample();
        a = 4'b1x00; s = 3; c.sample();
        inst = $realtobits(c.get_inst_coverage());
        ty = $realtobits(cg::get_coverage());
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [5]);
        // A value with an x bit is in no bin of these (19.5.4).
        assert_eq!(
            bins(&report, "Top.cg", 0, "a"),
            hits(&[("zero", 1), ("low", 2), ("high", 1), ("any", 4)])
        );
        assert_eq!(
            bins(&report, "Top.cg", 0, "neg"),
            hits(&[("minus", 2), ("plus", 3)])
        );
        assert_eq!(
            bins(&report, "Top.cg", 0, "$point3"),
            hits(&[("wrapped", 1), ("two", 1)])
        );
        // `none` has no value and takes no part (19.11.1): 4 of 4 bins of
        // `a`, 2 of 2 of `neg`, 2 of 2 of the third.
        assert_eq!(real(&mut sim, "inst"), 100.0);
        assert_eq!(real(&mut sim, "ty"), 100.0);
        let instance = &group(&report, "Top.cg").instances[0];
        assert_eq!((instance.covered, instance.total), (8, 8));
    }

    /// 19.5.3: a coverpoint without bins has automatic bins: one for each
    /// value up to `option.auto_bin_max` bins, 64 where nothing sets it,
    /// and one for each enumerator of an enumeration.
    fn automatic_bins(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    typedef enum bit [2:0] { RED = 1, GREEN = 4, BLUE = 6 } color_t;
    bit [2:0] three;
    bit [7:0] eight;
    bit signed [3:0] s;
    color_t color;
    covergroup cg;
        coverpoint three;
        coverpoint eight;
        few: coverpoint eight { option.auto_bin_max = 3; }
        coverpoint s { option.auto_bin_max = 4; }
        coverpoint color;
    endgroup
    cg c = new;
    initial begin
        three = 5; eight = 8'd130; s = -8; color = GREEN; c.sample();
        three = 7; eight = 8'd255; s = 7; color = BLUE; c.sample();
        three = 5; eight = 8'd3; s = -1; color = color_t'(3'd2); c.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        let three = bins(&report, "Top.cg", 0, "three");
        assert_eq!(three.len(), 8);
        assert_eq!(hit(three), hits(&[("auto[5]", 2), ("auto[7]", 1)]));
        // 256 values in 64 bins of 4.
        let eight = bins(&report, "Top.cg", 0, "eight");
        assert_eq!(eight.len(), 64);
        assert_eq!(
            hit(eight),
            hits(&[("auto[0:3]", 1), ("auto[128:131]", 1), ("auto[252:255]", 1)])
        );
        // 256 values in 3 bins of 85, the last with what is left over.
        assert_eq!(
            bins(&report, "Top.cg", 0, "few"),
            hits(&[("auto[0:84]", 1), ("auto[85:169]", 1), ("auto[170:255]", 1)])
        );
        assert_eq!(
            bins(&report, "Top.cg", 0, "s"),
            hits(&[
                ("auto[-8:-5]", 1),
                ("auto[-4:-1]", 1),
                ("auto[0:3]", 0),
                ("auto[4:7]", 1)
            ])
        );
        // A value that no enumerator has is in no bin.
        assert_eq!(
            bins(&report, "Top.cg", 0, "color"),
            hits(&[("auto[RED]", 0), ("auto[GREEN]", 1), ("auto[BLUE]", 1)])
        );
    }

    /// 19.5: the guard of a coverpoint is evaluated before its expression,
    /// which is not evaluated where the guard is false: the handle the
    /// expression reads may be `null` then.
    fn coverpoint_iff_guards_its_expression(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
class txn;
    bit [1:0] kind;
endclass
module Top(output int done);
    txn t;
    bit en;
    bit [1:0] plain;
    covergroup cg;
        guarded: coverpoint t.kind iff (t != null);
        coverpoint plain iff (en);
        always_on: coverpoint plain;
    endgroup
    cg c = new;
    initial begin
        plain = 1; c.sample();
        t = new; t.kind = 2; en = 1; plain = 3; c.sample();
        t = null; en = 0; plain = 0; c.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [3]);
        assert_eq!(hit(bins(&report, "Top.cg", 0, "guarded")), hits(&[("auto[2]", 1)]));
        assert_eq!(hit(bins(&report, "Top.cg", 0, "plain")), hits(&[("auto[3]", 1)]));
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "always_on")),
            hits(&[("auto[0]", 1), ("auto[1]", 1), ("auto[3]", 1)])
        );
    }

    /// 19.8.1: the arguments of `with function sample` are what the
    /// coverpoint expressions and guards read, before the names of the
    /// scope.
    fn sample_arguments(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [1:0] x = 3;
    covergroup cg with function sample(bit [1:0] x, bit on, int offset = 1);
        coverpoint x iff (on);
        sum: coverpoint x + offset[1:0];
    endgroup
    cg c = new;
    function automatic void record(bit [1:0] value);
        c.sample(value, 1'b1, 2);
    endfunction
    initial begin
        c.sample(2'd1, 1'b1);
        c.sample(.on(1'b0), .x(2'd2));
        record(2'd0);
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [3]);
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "x")),
            hits(&[("auto[0]", 1), ("auto[1]", 1)])
        );
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "sum")),
            hits(&[("auto[2]", 2), ("auto[3]", 1)])
        );
    }

    /// 19.3, 19.8: the arguments of `new` shape the bins of an instance,
    /// and the coverpoints read them at each sample. The instances of the
    /// 19.8 example have 2 and 3, and 4 and 7 bins.
    fn constructor_arguments_shape_bins(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    typedef enum bit [1:0] { IDLE, BUSY, DONE } state_t;
    covergroup cg(int xb, int yb, bit [2:0] base, state_t wanted)
        with function sample(bit [2:0] x, bit [2:0] y, state_t state);
        coverpoint x { option.auto_bin_max = xb + 1; }
        coverpoint y { option.auto_bin_max = yb + 1; }
        rel: coverpoint x - base {
            bins near = {[0:xb]};
            bins far = {[xb + 1:$]};
        }
        match: coverpoint state == wanted;
    endgroup
    cg cv1 = new(1, 2, 3'd1, BUSY);
    cg cv2 = new(3, 6, 3'd4, DONE);
    initial begin
        cv1.sample(3'd1, 3'd7, BUSY);
        cv1.sample(3'd7, 3'd0, IDLE);
        cv2.sample(3'd5, 3'd3, BUSY);
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        let group = group(&report, "Top.cg");
        let totals = |instance: usize| {
            let instance = &group.instances[instance];
            (
                instance.items[0].total,
                instance.items[1].total,
                instance.items[0].total + instance.items[1].total,
            )
        };
        assert_eq!(totals(0), (2, 3, 5));
        assert_eq!(totals(1), (4, 7, 11));
        assert_eq!(
            bins(&report, "Top.cg", 0, "x"),
            hits(&[("auto[0:3]", 1), ("auto[4:7]", 1)])
        );
        // 1 - 1 and 7 - 1 against `[0:1]` and `[2:$]`; 5 - 4 against
        // `[0:3]` and `[4:$]`.
        assert_eq!(bins(&report, "Top.cg", 0, "rel"), hits(&[("near", 1), ("far", 1)]));
        assert_eq!(bins(&report, "Top.cg", 1, "rel"), hits(&[("near", 1), ("far", 0)]));
        assert_eq!(
            bins(&report, "Top.cg", 0, "match"),
            hits(&[("auto[0]", 1), ("auto[1]", 1)])
        );
        assert_eq!(
            bins(&report, "Top.cg", 1, "match"),
            hits(&[("auto[0]", 1), ("auto[1]", 0)])
        );
    }
}

/// The reports among `events`, each with its severity and text.
fn reports(events: Vec<celox::RuntimeEvent>) -> Vec<(celox::Severity, String)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Report(report) => Some((
                report.severity,
                String::from_utf8_lossy(&report.message).into_owned(),
            )),
            _ => None,
        })
        .collect()
}

/// The item `item` of instance `instance` of covergroup type `name`.
fn item<'a>(
    report: &'a celox::CoverageReport,
    name: &str,
    instance: usize,
    item: &str,
) -> &'a celox::CoverItemReport {
    group(report, name).instances[instance]
        .items
        .iter()
        .find(|found| found.name == item)
        .unwrap_or_else(|| panic!("no item `{item}`"))
}

sv_backends! {
    /// 19.7: the `option` assignments of a definition are those of each
    /// instance, of the covergroup and of a coverpoint: name, comment,
    /// per_instance, weight, goal, at_least and auto_bin_max.
    fn definition_options(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst);
    bit [1:0] a, b;
    covergroup cg;
        option.name = "first";
        option.comment = "of the covergroup";
        option.per_instance = 1;
        option.weight = 3;
        option.goal = 90;
        option.at_least = 2;
        option.auto_bin_max = 2;
        coverpoint a;
        coverpoint b {
            option.weight = 2;
            option.goal = 80;
            option.comment = "of b";
            option.at_least = 1;
            option.auto_bin_max = 4;
        }
    endgroup
    cg c = new;
    initial begin
        a = 0; b = 1; c.sample();
        a = 3; b = 1; c.sample();
        a = 3; b = 2; c.sample();
        inst = $realtobits(c.get_inst_coverage());
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        let instance = &group(&report, "Top.cg").instances[0];
        assert_eq!(instance.name, "first");
        assert_eq!(instance.comment, "of the covergroup");
        assert!(instance.per_instance);
        assert_eq!((instance.weight, instance.goal, instance.at_least), (3, 90, 2));
        // `a` takes the two bins and the two hits of the covergroup; the
        // weight, goal and comment of the covergroup are not its own
        // (19.7, Table 19-2).
        let a = item(&report, "Top.cg", 0, "a");
        assert_eq!((a.weight, a.goal, a.at_least), (1, 100, 2));
        assert_eq!(a.comment, "");
        assert_eq!(
            bins(&report, "Top.cg", 0, "a"),
            hits(&[("auto[0:1]", 1), ("auto[2:3]", 2)])
        );
        assert_eq!((a.covered, a.total), (1, 2));
        let b = item(&report, "Top.cg", 0, "b");
        assert_eq!((b.weight, b.goal, b.at_least), (2, 80, 1));
        assert_eq!(b.comment, "of b");
        assert_eq!((b.covered, b.total), (2, 4));
        // (1 * 50 + 2 * 50) / 3.
        assert_eq!(real(&mut sim, "inst"), 50.0);
    }

    /// 19.5: a coverpoint with a type has the values its expression gives
    /// as assigned to a variable of the type, which is the type its bins
    /// are of.
    fn explicit_coverpoint_type(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    typedef enum bit [1:0] { A, B, C } kind_t;
    bit [7:0] wide;
    kind_t kind;
    covergroup cg;
        bit [1:0] low: coverpoint wide;
        bit signed [3:0] sig: coverpoint wide[3:0] {
            bins negative = {[$:-1]};
            bins others = {[0:$]};
        }
        logic [9:0] grown: coverpoint wide { bins top = {[256:$]}; bins rest = {[0:255]}; }
        kind_t k: coverpoint kind;
    endgroup
    cg c = new;
    initial begin
        wide = 8'hfe; kind = B; c.sample();
        wide = 8'h15; kind = C; c.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "low")),
            hits(&[("auto[1]", 1), ("auto[2]", 1)])
        );
        assert_eq!(
            bins(&report, "Top.cg", 0, "sig"),
            hits(&[("negative", 1), ("others", 1)])
        );
        assert_eq!(bins(&report, "Top.cg", 0, "grown"), hits(&[("top", 0), ("rest", 2)]));
        assert_eq!(
            bins(&report, "Top.cg", 0, "k"),
            hits(&[("auto[A]", 0), ("auto[B]", 1), ("auto[C]", 1)])
        );
    }

    /// 19.5: a coverpoint is of any width. One of more than 64 bits is
    /// sampled in words, with its bins compared as values of its width.
    fn wide_coverpoint_planes(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [64:0] w65;
    logic [127:0] w128;
    bit signed [99:0] s100;
    bit [1:0] narrow;
    covergroup cg(bit [127:0] limit);
        coverpoint w65 {
            bins top = {65'h1_0000_0000_0000_0000};
            bins low = {[0:65'hffff_ffff_ffff_ffff]};
        }
        coverpoint narrow;
        coverpoint w128 iff (narrow != 2) {
            bins big = {[128'h1_0000_0000_0000_0000_0000_0000:$]};
            bins bound = {[0:limit]};
            bins exact = {128'hdead_beef_0000_0000_0000_0000_0000_0001};
        }
        coverpoint s100 {
            bins negative = {[$:-1]};
            bins positive = {[1:$]};
        }
    endgroup
    cg c = new(128'h1234_0000_0000_0000_0000);
    initial begin
        w65 = 65'h1_0000_0000_0000_0000; w128 = 128'hdead_beef_0000_0000_0000_0000_0000_0001;
        s100 = -100'sd5; narrow = 0; c.sample();
        w65 = 65'd7; w128 = 128'h1234_0000_0000_0000_0000; s100 = 100'sh7_ffff_ffff_ffff_ffff_ffff;
        narrow = 1; c.sample();
        w65 = 65'h0_ffff_ffff_ffff_ffff; w128 = 128'd3; s100 = 0; narrow = 2; c.sample();
        w128 = 'x; narrow = 3; c.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [4]);
        assert_eq!(bins(&report, "Top.cg", 0, "w65"), hits(&[("top", 1), ("low", 3)]));
        // The third sample is guarded off, and the fourth has x bits.
        assert_eq!(
            bins(&report, "Top.cg", 0, "w128"),
            hits(&[("big", 1), ("bound", 1), ("exact", 1)])
        );
        assert_eq!(
            bins(&report, "Top.cg", 0, "s100"),
            hits(&[("negative", 1), ("positive", 1)])
        );
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "narrow")),
            hits(&[("auto[0]", 1), ("auto[1]", 1), ("auto[2]", 1), ("auto[3]", 1)])
        );
    }

    /// 19.5: a function a coverpoint expression calls may sample itself,
    /// another instance or the same: the values of a sample reach the
    /// heap only after all of them are evaluated, so the samples do not
    /// mix.
    fn nested_sample_in_a_coverpoint_function(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [1:0] v;
    int depth;
    covergroup inner_cg with function sample(bit [1:0] x);
        coverpoint x;
    endgroup
    inner_cg inner = new;
    covergroup outer_cg;
        first: coverpoint v;
        second: coverpoint note(v);
        third: coverpoint v + 2'd1;
    endgroup
    outer_cg outer = new;
    function automatic bit [1:0] note(bit [1:0] value);
        inner.sample(value);
        if (depth == 0) begin
            depth = 1;
            v = 3;
            outer.sample();
            v = value;
            depth = 0;
        end
        return value;
    endfunction
    initial begin
        v = 1; outer.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.outer_cg"), [2]);
        assert_eq!(samples(&report, "Top.inner_cg"), [2]);
        for (item, expected) in [
            ("first", [("auto[1]", 1), ("auto[3]", 1)]),
            ("second", [("auto[1]", 1), ("auto[3]", 1)]),
            ("third", [("auto[0]", 1), ("auto[2]", 1)]),
        ] {
            assert_eq!(hit(bins(&report, "Top.outer_cg", 0, item)), hits(&expected), "{item}");
        }
        assert_eq!(
            hit(bins(&report, "Top.inner_cg", 0, "x")),
            hits(&[("auto[1]", 1), ("auto[3]", 1)])
        );
    }

    /// 19.5, 26.3: the constants of a bin and the names of a coverpoint
    /// expression resolve in the scope that declares the covergroup, also
    /// through its wildcard imports, in the module that declares it and in
    /// the package another module takes it from.
    fn bin_constants_use_imported_names(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
package limits;
    localparam int LOW = 2;
    parameter bit [3:0] HIGH = 4'd9;
    typedef enum bit [1:0] { READ = 1, WRITE = 2 } op_t;
    function automatic bit [3:0] twice(bit [3:0] value);
        return value * 2;
    endfunction
endpackage
package groups;
    import limits::*;
    covergroup pcg with function sample(bit [3:0] v, op_t op);
        coverpoint v { bins mid = {[LOW:HIGH]}; bins twice_low = {twice(LOW)}; }
        coverpoint op { bins reads = {READ}; bins writes = {WRITE}; }
    endgroup
endpackage
module Top(output int done);
    import limits::*;
    localparam int STEP = 3;
    covergroup cg with function sample(bit [3:0] v);
        coverpoint v {
            bins low = {[0:LOW - 1]};
            bins mid = {[LOW:HIGH]};
            bins step = {STEP, limits::LOW + STEP};
        }
        doubled: coverpoint twice(v) { bins eight = {8}; }
    endgroup
    cg c = new;
    groups::pcg p = new;
    initial begin
        c.sample(4'd1); c.sample(4'd3); c.sample(4'd4); c.sample(4'd5); c.sample(4'd12);
        p.sample(4'd4, READ); p.sample(4'd9, WRITE); p.sample(4'd10, WRITE);
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(
            bins(&report, "Top.cg", 0, "v"),
            hits(&[("low", 1), ("mid", 3), ("step", 2)])
        );
        assert_eq!(bins(&report, "Top.cg", 0, "doubled"), hits(&[("eight", 2)]));
        assert_eq!(
            bins(&report, "groups::pcg", 0, "v"),
            hits(&[("mid", 2), ("twice_low", 1)])
        );
        assert_eq!(
            bins(&report, "groups::pcg", 0, "op"),
            hits(&[("reads", 1), ("writes", 2)])
        );
    }

    /// 19.5.7: what the analysis warns of for the constants of a bin, the
    /// construction warns of for a value that only it knows: the value
    /// takes no part in the bin. A construction whose values are all of
    /// the coverpoint type warns of nothing, also in a type with constants
    /// the build warned of: the construction does not repeat those.
    fn input_dependent_bin_warns_at_run_time(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [2:0] p1;
    covergroup cg(int low, int high);
        coverpoint p1 {
            bins b = {low, [1:high]};
            bins fixed = {15, [6:10]};
            bins none = {-1};
        }
    endgroup
    cg fits, clipped;
    initial begin
        fits = new(0, 7);
        #1 clipped = new(-1, 10);
        p1 = 7; fits.sample(); clipped.sample();
        p1 = 0; fits.sample(); clipped.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(0).unwrap();
        assert_eq!(reports(sim.drain_runtime_events()), []);
        sim.run_until(2).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let warned = reports(sim.drain_runtime_events());
        assert_eq!(warned.len(), 1, "{warned:?}");
        let (severity, text) = &warned[0];
        assert_eq!(*severity, celox::Severity::Warning);
        assert!(
            text.contains("bins `b` of coverpoint `p1` of covergroup `Top.cg`")
                && text.contains("value -1")
                && text.contains("range [1:10] is limited"),
            "{text}"
        );
        // The constants of `fixed` and `none` are what the build warned
        // of.
        assert!(
            !text.contains("bins `fixed`")
                && !text.contains("bins `none`")
                && !text.contains("15")
                && !text.contains("[6:10]"),
            "{text}"
        );
        let report = sim.coverage();
        // `fixed` is `[6:7]`, and `none` has no value.
        assert_eq!(
            bins(&report, "Top.cg", 0, "p1"),
            hits(&[("b", 2), ("fixed", 1)])
        );
        // Without the -1, and with `[1:7]`.
        assert_eq!(
            bins(&report, "Top.cg", 1, "p1"),
            hits(&[("b", 1), ("fixed", 1)])
        );
    }

    /// 19.7, 19.4: the value of an `option` assignment is an ordinary
    /// expression, evaluated where the covergroup is constructed: it reads
    /// the arguments, the variables of the scope and, in an embedded
    /// covergroup, the members of the class, and may call functions.
    fn option_values_are_ordinary_expressions(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
package p;
    int pkg_goal = 70;
    covergroup pcg(string label);
        option.name = {label, "_pkg"};
        option.goal = pkg_goal;
    endgroup
endpackage
class holder;
    int m_w;
    string m_name;
    bit [1:0] v;
    function int doubled(); return m_w * 2; endfunction
    covergroup cg(int c);
        option.weight = m_w;
        option.comment = $sformatf("%s has weight %0d", m_name, m_w);
        option.at_least = doubled();
        coverpoint v { option.weight = c; }
    endgroup
    function new(string name, int w);
        m_name = name;
        m_w = w;
        cg = new(w + 1);
    endfunction
endclass
module Top(output int done);
    int goal = 55;
    string module_name = "top";
    bit [1:0] x;
    covergroup cg(int c, string instComment);
        option.per_instance = c > 0;
        option.comment = instComment;
        option.goal = goal;
        option.name = module_name;
        b: coverpoint x { option.weight = c; }
    endgroup
    cg first, second;
    holder h;
    p::pcg pc;
    initial begin
        first = new(2, "one");
        goal = 60; module_name = "later";
        second = new(0, "two");
        h = new("held", 4);
        pc = new("from");
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        let instances = &group(&report, "Top.cg").instances;
        assert_eq!(
            instances
                .iter()
                .map(|instance| (
                    instance.name.as_str(),
                    instance.comment.as_str(),
                    instance.goal,
                    instance.per_instance,
                    instance.items[0].weight
                ))
                .collect::<Vec<_>>(),
            [("top", "one", 55, true, 2), ("later", "two", 60, false, 0)]
        );
        let held = &group(&report, "$unit::holder::cg").instances[0];
        assert_eq!((held.weight, held.at_least), (4, 8));
        assert_eq!(held.comment, "held has weight 4");
        assert_eq!(held.items[0].weight, 5);
        let package = &group(&report, "p::pcg").instances[0];
        assert_eq!((package.name.as_str(), package.goal), ("from_pkg", 70));
    }

    /// 19.5: a bin may call functions, with the arguments of the
    /// covergroup: the construction calls them, a method of the class for
    /// an embedded covergroup too.
    fn function_calls_in_bin_bounds(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
class scaled;
    const int factor;
    bit [5:0] v;
    function int times(int value); return value * factor; endfunction
    covergroup cg(int low, int high);
        coverpoint v {
            bins scaled_range = {[times(low):times(high)]};
            bins exact = {factor};
        }
    endgroup
    function new(int f);
        factor = f;
        cg = new(1, 3);
    endfunction
endclass
module Top(output int done);
    bit [5:0] v;
    function automatic int f(int value); return value + 10; endfunction
    covergroup cg(int low, int high);
        coverpoint v {
            bins upto = {[low:f(high)]};
            bins folded = {$clog2(64) + 1};
        }
    endgroup
    cg a, b;
    scaled s;
    initial begin
        a = new(0, 5);
        b = new(20, 15);
        s = new(4);
        v = 15; a.sample(); b.sample();
        v = 22; a.sample(); b.sample();
        v = 7; a.sample(); b.sample();
        s.v = 12; s.cg.sample();
        s.v = 4; s.cg.sample();
        s.v = 13; s.cg.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        // `[0:15]` and `[20:25]`.
        assert_eq!(bins(&report, "Top.cg", 0, "v"), hits(&[("upto", 2), ("folded", 1)]));
        assert_eq!(bins(&report, "Top.cg", 1, "v"), hits(&[("upto", 1), ("folded", 1)]));
        // `[4:12]` and 4.
        assert_eq!(
            bins(&report, "$unit::scaled::cg", 0, "v"),
            hits(&[("scaled_range", 2), ("exact", 1)])
        );
    }

    /// 19.8.1: an argument of `sample` may be an `inout`: only an output
    /// is excluded. The covergroup reads it and leaves the actual as it
    /// was.
    fn inout_sample_formal(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output int kept);
    covergroup cg with function sample(inout int v);
        coverpoint v[1:0];
    endgroup
    cg c = new;
    int value;
    initial begin
        value = 6; c.sample(value);
        value = 9; c.sample(value);
        kept = value;
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let kept = sim.signal("kept");
        assert_eq!(sim.get(kept), BigUint::from(9u8));
        let report = sim.coverage();
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "$point1")),
            hits(&[("auto[1]", 1), ("auto[2]", 1)])
        );
    }
}

/// 19.5.3, 19.5.4: a sample with an x or z bit is in no automatic bin. In
/// two-state simulation no value has such a bit, and the same design counts
/// every sample.
#[test]
fn four_state_samples_miss_automatic_bins() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    logic [1:0] v;
    logic [69:0] wide;
    covergroup cg;
        coverpoint v;
        coverpoint wide { bins zero = {0}; bins some = {[1:$]}; }
    endgroup
    cg c = new;
    initial begin
        c.sample();
        v = 2'b1x; wide = 70'd5; c.sample();
        v = 2'b10; wide[69] = 1'bz; c.sample();
        done = 1;
    end
endmodule
"#;
    for (four_state, v, wide) in [
        (true, vec![("auto[2]", 1)], vec![("zero", 0), ("some", 1)]),
        (
            false,
            vec![("auto[0]", 1), ("auto[2]", 2)],
            vec![("zero", 1), ("some", 2)],
        ),
    ] {
        let mut sim = simulation(source)
            .four_state(four_state)
            .build_native()
            .unwrap();
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [3], "four_state={four_state}");
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "v")),
            hits(&v),
            "four_state={four_state}"
        );
        assert_eq!(
            bins(&report, "Top.cg", 0, "wide"),
            hits(&wide),
            "four_state={four_state}"
        );
    }
}

/// 19.5.3, 19.7: `option.auto_bin_max` below 1 gives a coverpoint one
/// automatic bin with all its values, and the `new` that constructs the
/// instance warns of it, for a constant too.
#[test]
fn auto_bin_max_below_one_makes_one_bin_and_warns() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [1:0] v;
    covergroup cg(int max);
        coverpoint v { option.auto_bin_max = max; }
    endgroup
    covergroup zero;
        coverpoint v { option.auto_bin_max = 0; }
    endgroup
    cg two, none;
    zero z;
    initial begin
        two = new(2);
        #1 none = new(-3);
        #1 z = new;
        v = 3; two.sample(); none.sample(); z.sample();
        done = 1;
    end
endmodule
"#;
    let mut sim = simulation(source).four_state(false).build_native().unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(reports(sim.drain_runtime_events()), []);
    for (time, maximum) in [(1, "-3"), (2, "0")] {
        sim.run_until(time).unwrap();
        let warned = reports(sim.drain_runtime_events());
        assert_eq!(warned.len(), 1, "{warned:?}");
        let (severity, text) = &warned[0];
        assert_eq!(*severity, celox::Severity::Warning);
        assert!(
            text.contains(&format!("option.auto_bin_max is {maximum}"))
                && text.contains("one automatic bin"),
            "{text}"
        );
    }
    let done = sim.signal("done");
    assert_eq!(sim.get(done), BigUint::from(1u8));
    let report = sim.coverage();
    assert_eq!(
        bins(&report, "Top.cg", 0, "v"),
        hits(&[("auto[0:1]", 0), ("auto[2:3]", 1)])
    );
    assert_eq!(bins(&report, "Top.cg", 1, "v"), hits(&[("auto[0:3]", 1)]));
    assert_eq!(bins(&report, "Top.zero", 0, "v"), hits(&[("auto[0:3]", 1)]));
}

/// 19.5.4, 19.5.7: a bin value written with x or z bits keeps them in both
/// state modes, as a constant of the descriptor of the type. On a coverpoint
/// of a 4-state type it holds the samples with exactly those bits, of which
/// two-state simulation has none; on one of a 2-state type it takes no part
/// in its bin, which the build warns of.
#[test]
fn bin_values_with_x_or_z_bits() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    logic [3:0] v;
    bit [3:0] w;
    covergroup cg;
        coverpoint v { bins xz = {4'b1x0z}; bins plain = {4'b1000}; }
        coverpoint w { bins xz = {4'b1x0z}; bins plain = {4'b1000}; }
    endgroup
    cg c = new;
    initial begin
        v = 4'b1x0z; w = 4'b1x0z; c.sample();
        v = 4'b1000; w = 4'b1000; c.sample();
        v = 4'b1x00; c.sample();
        done = 1;
    end
endmodule
"#;
    for (four_state, v) in [
        (true, [("xz", 1), ("plain", 1)]),
        (false, [("xz", 0), ("plain", 3)]),
    ] {
        let mut sim = simulation(source)
            .four_state(four_state)
            .build_native()
            .unwrap();
        let warned: Vec<_> = sim
            .warnings()
            .iter()
            .filter_map(|warning| match warning {
                celox::CompilationWarning::SystemVerilog(celox::SvFrontendWarning {
                    kind: celox::SvFrontendWarningKind::CovergroupBinValue { item, bin, .. },
                    ..
                }) => Some((item.clone(), bin.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            warned,
            [("w".to_string(), "xz".to_string())],
            "four_state={four_state}"
        );
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(
            bins(&report, "Top.cg", 0, "v"),
            hits(&v),
            "four_state={four_state}"
        );
        // `xz` of `w` has no value, and `w` holds no x or z bit.
        assert_eq!(
            bins(&report, "Top.cg", 0, "w"),
            hits(&[("plain", 3)]),
            "four_state={four_state}"
        );
    }
}

sv_backends! {
    /// 19.5: coverpoints that share a call of the heap, 96 of one bit with
    /// guards on some, count as the same coverpoints do one by one, each
    /// as a value of 65 bits with a call of its own.
    fn packed_points_match_unpacked(sim) {
        @setup {
            let points = |wide: bool| {
                (0..96)
                    .map(|index| {
                        let guard = if index % 5 == 0 { " iff (en)" } else { "" };
                        let value = if wide {
                            format!("{{64'd0, data[{index}]}}")
                        } else {
                            format!("data[{index}]")
                        };
                        format!(
                            "        p{index}: coverpoint {value}{guard} {{ bins zero = {{0}}; bins one = {{1}}; }}\n"
                        )
                    })
                    .collect::<String>()
            };
            let source = format!(
                "`timescale 1ns/1ns
module Top(output int done);
    bit [95:0] data = 96'h0123_4567_89ab_cdef_0f1e_2d3c;
    bit en;
    covergroup packed_cg;
{}    endgroup
    covergroup wide_cg;
{}    endgroup
    packed_cg narrow = new;
    wide_cg wide = new;
    initial begin
        repeat (40) begin
            data = {{data[94:0], data[95] ^ data[61] ^ data[7] ^ 1'b1}};
            en = data[11] ^ data[2];
            narrow.sample();
            wide.sample();
        end
        done = 1;
    end
endmodule
",
                points(false),
                points(true)
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.packed_cg"), [40]);
        let items = |name: &str| {
            group(&report, name).instances[0]
                .items
                .iter()
                .map(|item| {
                    (
                        item.name.clone(),
                        item.bins
                            .iter()
                            .map(|bin| (bin.name.clone(), bin.hits))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let narrow = items("Top.packed_cg");
        assert_eq!(narrow.len(), 96);
        assert_eq!(narrow, items("Top.wide_cg"));
        // A coverpoint without a guard counts every sample, and a guarded
        // one only some.
        let total = |item: usize| narrow[item].1.iter().map(|(_, hits)| hits).sum::<u64>();
        assert_eq!(total(1), 40);
        assert!(total(0) > 0 && total(0) < 40, "{:?}", narrow[0]);
        assert!(narrow.iter().any(|(_, bins)| bins.iter().all(|(_, hits)| *hits > 0)));
    }

    /// A covergroup of a package and one embedded in a class of a package
    /// have the bins of their declaration in every module that uses them,
    /// also where the package is analyzed again for a specialization that
    /// only one of the modules asks for.
    fn package_covergroups_from_two_modules(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
package p;
    localparam int TOP = 5;
    covergroup pcg(int low) with function sample(bit [3:0] v);
        coverpoint v { bins range = {[low:TOP]}; bins rest = {[TOP + 1:$]}; }
    endgroup
    class K;
        bit [2:0] kind;
        covergroup cg;
            coverpoint kind { bins lower = {[0:3]}; bins upper = {[4:7]}; }
        endgroup
        function new(); cg = new; endfunction
    endclass
    class P #(type T = int);
        T v;
    endclass
endpackage
module Sub;
    p::pcg c = new(2);
    p::K k = new;
    p::P #(logic [3:0]) asked = new;
    initial begin
        c.sample(4'd1); c.sample(4'd3); c.sample(4'd9);
        k.kind = 6; k.cg.sample();
    end
endmodule
module Top(output int done);
    p::pcg c = new(0);
    p::K k = new;
    Sub sub();
    initial begin
        c.sample(4'd1); c.sample(4'd6);
        k.kind = 1; k.cg.sample(); k.kind = 2; k.cg.sample();
        #1 done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(2).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        let mut scope: Vec<_> = (0..2).map(|instance| bins(&report, "p::pcg", instance, "v")).collect();
        scope.sort();
        assert_eq!(
            scope,
            [
                hits(&[("range", 1), ("rest", 1)]),
                hits(&[("range", 1), ("rest", 1)]),
            ]
        );
        let mut embedded: Vec<_> =
            (0..2).map(|instance| bins(&report, "p::K::cg", instance, "kind")).collect();
        embedded.sort();
        assert_eq!(
            embedded,
            [
                hits(&[("lower", 0), ("upper", 1)]),
                hits(&[("lower", 2), ("upper", 0)]),
            ]
        );
    }
}

/// 19.5.7: the build warns of the constants of a bin that the type of the
/// coverpoint has not, once for each element of the range list and at its
/// line, and the bins hold what the standard says: the examples of 19.5.7.
#[test]
fn bin_values_outside_the_coverpoint_type_warn_at_build() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [2:0] p1;
    bit signed [2:0] p2;
    covergroup g1;
        coverpoint p1 {
            bins b1 = { 1, [2:5], [6:10]};
            bins b2 = {-1,
                       [1:10],
                       15};
        }
        coverpoint p2 {
            bins b3 = { 1, [2:5], [6:10]};
            bins b4 = {-1, [1:10], 15};
        }
    endgroup
    g1 g = new;
    initial begin
        p1 = 7; p2 = -1; g.sample();
        p1 = 0; p2 = 3; g.sample();
        p1 = 6; p2 = -4; g.sample();
        done = 1;
    end
endmodule
"#;
    let mut sim = simulation(source).four_state(false).build_native().unwrap();
    let warned: Vec<_> = sim
        .warnings()
        .iter()
        .map(|warning| match warning {
            celox::CompilationWarning::SystemVerilog(celox::SvFrontendWarning {
                kind: celox::SvFrontendWarningKind::CovergroupBinValue { item, bin, .. },
                location: Some(location),
            }) => {
                assert!(
                    warning.to_string().contains("IEEE 1800-2023 19.5.7"),
                    "{warning}"
                );
                (location.line, item.as_str(), bin.as_str())
            }
            warning => panic!("unexpected warning: {warning}"),
        })
        .collect();
    assert_eq!(
        warned,
        [
            (7, "p1", "b1"),
            (8, "p1", "b2"),
            (9, "p1", "b2"),
            (10, "p1", "b2"),
            (13, "p2", "b3"),
            (13, "p2", "b3"),
            (14, "p2", "b4"),
            (14, "p2", "b4"),
        ]
    );
    sim.run_until(1).unwrap();
    assert_eq!(reports(sim.drain_runtime_events()), []);
    let report = sim.coverage();
    // b1 is {1, [2:5], [6:7]} and b2 is {[1:7]}; b3 is {1, [2:3]} and b4
    // is {-1, [1:3]}.
    assert_eq!(
        bins(&report, "Top.g1", 0, "p1"),
        hits(&[("b1", 2), ("b2", 2)])
    );
    assert_eq!(
        bins(&report, "Top.g1", 0, "p2"),
        hits(&[("b3", 1), ("b4", 2)])
    );
}

sv_backends! {
    /// 19.3: a string or a class handle argument that a coverpoint reads
    /// is kept by the instance, which alone keeps the object alive when
    /// nothing else refers to it, also over a collection.
    fn handle_arguments_live_with_the_instance(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(input int rounds, output int done);
    class K;
        bit [2:0] v;
        function new(bit [2:0] value); v = value; endfunction
    endclass
    class Junk; int v; endclass
    covergroup cg(string label, K held, int bias);
        length: coverpoint label.len() + bias { bins three = {3}; bins five = {5}; }
        coverpoint held.v iff (held != null);
    endgroup
    cg a, b;
    Junk junk;
    function automatic cg make(string label, bit [2:0] value, int bias);
        K k = new(value);
        cg made = new(label, k, bias);
        return made;
    endfunction
    initial begin
        a = make("ab", 3'd6, 1);
        b = make("four", 3'd2, 1);
        a.sample();
        #1;
        // Enough allocation for a collection between time slots.
        repeat (4) begin
            for (int i = 0; i < rounds * 1000; i++) begin
                junk = new;
                junk.v = i;
            end
            #1;
        end
        a.sample(); b.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        let rounds = sim.signal("rounds");
        sim.modify(|io| io.set(rounds, 3u8)).unwrap();
        sim.run_until(6).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [2, 1]);
        assert_eq!(bins(&report, "Top.cg", 0, "length"), hits(&[("three", 2), ("five", 0)]));
        assert_eq!(bins(&report, "Top.cg", 1, "length"), hits(&[("three", 0), ("five", 1)]));
        // `held.v` is no single variable, so the second coverpoint has a
        // name of the analysis (19.5).
        assert_eq!(hit(bins(&report, "Top.cg", 0, "$point2")), hits(&[("auto[6]", 2)]));
        assert_eq!(hit(bins(&report, "Top.cg", 1, "$point2")), hits(&[("auto[2]", 1)]));
    }
}

/// 19.3: an argument of a 4-state type that a coverpoint reads keeps its x
/// and z bits in the instance, of any width. In two-state simulation it has
/// none.
#[test]
fn four_state_arguments_keep_their_bits() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    covergroup cg(logic [3:0] narrow, logic [69:0] wide, int plain);
        coverpoint narrow;
        hi: coverpoint wide[69:66];
        same: coverpoint narrow === 4'b1x0z;
        wide_same: coverpoint wide === {4'b10xz, 66'd5};
        low: coverpoint plain[1:0];
    endgroup
    cg a, b;
    initial begin
        a = new(4'b1x0z, {4'b10xz, 66'd5}, 2);
        b = new(4'd9, 70'd7, -1);
        a.sample(); b.sample();
        done = 1;
    end
endmodule
"#;
    for four_state in [true, false] {
        let mut sim = simulation(source)
            .four_state(four_state)
            .build_native()
            .unwrap();
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        let of = |instance: usize, item: &str| hit(bins(&report, "Top.cg", instance, item));
        // With its x and z bits the value is in no automatic bin, and is
        // the value `===` compares it with. Without them it is 4'b1000 and
        // 4'b1000 of the high bits, which no value with an x or z bit is
        // the same as.
        let (narrow, hi, same) = if four_state {
            (vec![], vec![], "auto[1]")
        } else {
            (vec![("auto[8]", 1)], vec![("auto[8]", 1)], "auto[0]")
        };
        assert_eq!(of(0, "narrow"), hits(&narrow), "four_state={four_state}");
        assert_eq!(of(0, "hi"), hits(&hi), "four_state={four_state}");
        assert_eq!(of(0, "same"), hits(&[(same, 1)]), "four_state={four_state}");
        assert_eq!(
            of(0, "wide_same"),
            hits(&[(same, 1)]),
            "four_state={four_state}"
        );
        assert_eq!(of(0, "low"), hits(&[("auto[2]", 1)]));
        assert_eq!(of(1, "narrow"), hits(&[("auto[9]", 1)]));
        assert_eq!(of(1, "hi"), hits(&[("auto[0]", 1)]));
        assert_eq!(of(1, "same"), hits(&[("auto[0]", 1)]));
        assert_eq!(of(1, "wide_same"), hits(&[("auto[0]", 1)]));
        assert_eq!(of(1, "low"), hits(&[("auto[3]", 1)]));
    }
}
