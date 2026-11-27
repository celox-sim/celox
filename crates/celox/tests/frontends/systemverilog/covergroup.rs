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
    let package = "package p;\n    bit a, b;\n    covergroup pcg(int x); coverpoint x; \
         ab: cross a, b { bins w = ab with (a > b); } endgroup\nendpackage\n";
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
        assert!(
            error.contains("covergroup cross with or matches selection"),
            "{body}: {error}"
        );
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

/// 18.14.1: the sampler of a clocking event is no process of the source, so
/// giving a covergroup a clocking event does not change the random stimulus
/// of the processes declared after it, whichever shape the sampler has: the
/// edge-sensitive procedure of `@(posedge clk)` or the process of `@(clk)`.
/// A procedure the source writes takes a seed, and so changes it.
#[test]
fn clocking_events_do_not_change_random_stimulus() {
    let draws_with = |event: &str, declaration: &str, written: &str| {
        let source = format!(
            "`timescale 1ns/1ns
module Top(output int unsigned a, b, output int done);
    bit clk = 0;
    bit [1:0] d;
    covergroup cg {event}; coverpoint d; endgroup
    {declaration}
    {written}
    always #1 clk = ~clk;
    initial begin
        a = $urandom;
        @(posedge clk);
        b = $urandom;
        done = 1;
    end
endmodule
"
        );
        let mut sim = simulation(&source).four_state(false).build().unwrap();
        sim.run_until(3).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        ["a", "b"].map(|name| {
            let signal = sim.signal(name);
            sim.get(signal)
        })
    };
    let draws = |event: &str, written: &str| draws_with(event, "cg c = new;", written);
    let plain = draws("", "");
    assert_ne!(plain[0], plain[1], "the stimulus is random");
    assert_eq!(draws("@(posedge clk)", ""), plain);
    assert_eq!(draws("@(negedge clk iff d == 0)", ""), plain);
    assert_eq!(draws("@(clk)", ""), plain);
    assert_ne!(draws("", "always @(posedge clk) c.sample();"), plain);
    // An instance that a procedure declared before the drawing process
    // constructs, of a type sampled by the procedure of the type or by a
    // process of the type, and more instances than the variables of its
    // list hold: the procedure that constructs is one of the source, the
    // samplers draw no seed.
    let constructed =
        "cg c, more[6];\n    initial begin c = new; foreach (more[i]) more[i] = new; end";
    let plain = draws_with("", constructed, "");
    assert_eq!(draws_with("@(posedge clk)", constructed, ""), plain);
    assert_eq!(
        draws_with("@(negedge clk iff d == 0)", constructed, ""),
        plain
    );
    assert_eq!(draws_with("@(clk)", constructed, ""), plain);
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

/// The bins of item `item` of instance `instance` of covergroup type `name`
/// that are no `bins` bins of values, each with its kind and its hits.
fn other_bins(
    report: &celox::CoverageReport,
    name: &str,
    instance: usize,
    item_name: &str,
) -> Vec<(String, celox::BinReportKind, u64)> {
    item(report, name, instance, item_name)
        .bins
        .iter()
        .filter(|bin| bin.kind != celox::BinReportKind::Bins)
        .map(|bin| (bin.name.clone(), bin.kind, bin.hits))
        .collect()
}

/// The covered and all bins of item `item` of instance `instance` of
/// covergroup type `name`, as its coverage counts them (19.11.1).
fn counted(
    report: &celox::CoverageReport,
    name: &str,
    instance: usize,
    item_name: &str,
) -> (u64, u64) {
    let item = item(report, name, instance, item_name);
    (item.covered, item.total)
}

/// Whether `value` is `expected` but for the rounding of the arithmetic
/// that gives it.
fn close(value: f64, expected: f64) -> bool {
    (value - expected).abs() < 1e-9
}

sv_backends! {
    /// 19.5.1: `name[]` is a bin for each value and `name[N]` distributes
    /// the values among N bins; these are the examples of the clause. N may
    /// be an argument of the covergroup.
    fn array_bins(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst);
    bit [9:0] v_a;
    bit [3:0] f;
    covergroup cg(int n);
        coverpoint v_a {
            bins a   = { [0:63],65 };
            bins b[] = { [127:150],[148:191] };
            bins c[] = { 200,201,202 };
            bins d   = { [1000:$] };
            bins others[] = default;
        }
        coverpoint f { bins fixed [4] = {[1:10], 1, 4, 7}; }
        g: coverpoint f { bins fixed [5] = {1, 4, 7}; }
        h: coverpoint f { bins sized [n] = {[0:7]}; }
    endgroup
    cg c = new(3);
    initial begin
        v_a = 65; f = 1; c.sample();
        v_a = 148; f = 4; c.sample();
        v_a = 201; f = 10; c.sample();
        v_a = 1023; f = 7; c.sample();
        v_a = 500; f = 0; c.sample();
        c.sample();
        v_a = 64; c.sample();
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
        // `a`, the 65 bins `b[127]` to `b[191]`, the three of `c` and `d`:
        // 70 bins, and the bins of `others` are none of the coverage.
        let v_a = item(&report, "Top.cg", 0, "v_a");
        assert_eq!(
            v_a.bins
                .iter()
                .filter(|bin| bin.kind == celox::BinReportKind::Bins)
                .count(),
            70
        );
        assert_eq!(v_a.bins[1].name, "b[127]");
        assert_eq!(v_a.bins[65].name, "b[191]");
        assert_eq!(counted(&report, "Top.cg", 0, "v_a"), (4, 70));
        let mut others = other_bins(&report, "Top.cg", 0, "v_a");
        others.sort_by(|first, second| first.0.cmp(&second.0));
        assert_eq!(
            others,
            [
                ("others[500]".to_string(), celox::BinReportKind::Default, 2),
                ("others[64]".to_string(), celox::BinReportKind::Default, 1),
            ]
        );
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "v_a"))
                .into_iter()
                .filter(|(name, _)| !name.starts_with("others"))
                .collect::<Vec<_>>(),
            hits(&[("a", 1), ("b[148]", 1), ("c[201]", 1), ("d", 1)])
        );
        // <1,2,3>, <4,5,6>, <7,8,9>, <10,1,4,7>.
        assert_eq!(
            bins(&report, "Top.cg", 0, "f"),
            hits(&[("fixed[0]", 1), ("fixed[1]", 1), ("fixed[2]", 1), ("fixed[3]", 4)])
        );
        // <1>, <4>, <7>, < >, < >: the two bins without a value are none.
        assert_eq!(
            bins(&report, "Top.cg", 0, "g"),
            hits(&[("fixed[0]", 1), ("fixed[1]", 1), ("fixed[2]", 1)])
        );
        // Three bins of eight values: <0,1>, <2,3>, <4,5,6,7>.
        assert_eq!(
            bins(&report, "Top.cg", 0, "h"),
            hits(&[("sized[0]", 4), ("sized[1]", 0), ("sized[2]", 2)])
        );
        assert!(close(
            real(&mut sim, "inst"),
            (4.0 / 70.0 + 1.0 + 1.0 + 2.0 / 3.0) / 4.0 * 100.0
        ));
    }

    /// 19.5: a `default` bin counts the samples that are in no other bin of
    /// its coverpoint, `default[]` in a bin for each value. It is no bin of
    /// the coverage, and a coverpoint with it has no automatic bins.
    fn default_bins(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst);
    bit [2:0] v;
    bit en;
    covergroup cg;
        coverpoint v { bins low = {[0:1]}; bins rest = default; }
        each: coverpoint v { bins low = {[0:1]}; bins rest[] = default; }
        guarded: coverpoint v { bins low = {[0:1]}; bins rest = default iff (en); }
        only: coverpoint v { bins rest = default; }
    endgroup
    cg c = new;
    initial begin
        v = 0; en = 1; c.sample();
        v = 5; c.sample();
        en = 0; c.sample();
        v = 7; c.sample();
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
        let default = celox::BinReportKind::Default;
        assert_eq!(bins(&report, "Top.cg", 0, "v"), hits(&[("low", 1), ("rest", 3)]));
        assert_eq!(
            other_bins(&report, "Top.cg", 0, "v"),
            [("rest".to_string(), default, 3)]
        );
        assert_eq!(counted(&report, "Top.cg", 0, "v"), (1, 1));
        assert_eq!(
            other_bins(&report, "Top.cg", 0, "each"),
            [
                ("rest[5]".to_string(), default, 2),
                ("rest[7]".to_string(), default, 1),
            ]
        );
        // The guard of the bin held for one of the three samples.
        assert_eq!(
            other_bins(&report, "Top.cg", 0, "guarded"),
            [("rest".to_string(), default, 1)]
        );
        // No bin of `only` counts for its coverage (19.11.1): it has none
        // to cover and takes no part in that of the covergroup.
        assert_eq!(
            other_bins(&report, "Top.cg", 0, "only"),
            [("rest".to_string(), default, 4)]
        );
        assert_eq!(counted(&report, "Top.cg", 0, "only"), (0, 0));
        assert_eq!(real(&mut sim, "inst"), 100.0);
    }

    /// 19.5.5, 19.5.6, 19.11.1: the values of `ignore_bins` and
    /// `illegal_bins` are values of no other bin, taken once the values
    /// are distributed, and a bin that is left without a value counts on
    /// neither side of the coverage. The first coverpoint is the example
    /// of 19.11.1: of four automatic bins three are left.
    fn ignore_and_illegal_bins(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst);
    bit [2:0] a, b;
    bit [3:0] w;
    covergroup ct;
        coverpoint b {
            option.auto_bin_max = 4;
            ignore_bins ig = {[0:1], [5:6]};
        }
        coverpoint a {
            bins low[] = {[0:3]};
            bins high = {[4:7]};
            ignore_bins skip = {1, [6:7]};
            illegal_bins bad = {3};
        }
        coverpoint w {
            bins all[4] = {[0:15]};
            ignore_bins quarter = {[4:7]};
            ignore_bins part = {0};
        }
    endgroup
    ct c = new;
    initial begin
        a = 0; b = 2; w = 0; c.sample();
        a = 1; b = 0; w = 5; c.sample();
        a = 4; b = 4; w = 9; c.sample();
        a = 7; b = 7; w = 1; c.sample();
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
        // No sample is of an illegal value.
        assert_eq!(reports(sim.drain_runtime_events()), []);
        let report = sim.coverage();
        let (ignore, illegal) = (celox::BinReportKind::Ignore, celox::BinReportKind::Illegal);
        let counting = |item: &str| -> Vec<(String, u64)> {
            let other = other_bins(&report, "Top.ct", 0, item);
            bins(&report, "Top.ct", 0, item)
                .into_iter()
                .filter(|(name, _)| other.iter().all(|(other, ..)| other != name))
                .collect()
        };
        // auto[0:1] has no value left; auto[4:5] is 4 and auto[6:7] is 7.
        assert_eq!(
            counting("b"),
            hits(&[("auto[2:3]", 1), ("auto[4:5]", 1), ("auto[6:7]", 1)])
        );
        assert_eq!(
            other_bins(&report, "Top.ct", 0, "b"),
            [("ig".to_string(), ignore, 1)]
        );
        assert_eq!(counted(&report, "Top.ct", 0, "b"), (3, 3));
        // low[1] and low[3] have no value left, and `high` is 4 and 5.
        assert_eq!(counting("a"), hits(&[("low[0]", 1), ("low[2]", 0), ("high", 1)]));
        assert_eq!(
            other_bins(&report, "Top.ct", 0, "a"),
            [
                ("skip".to_string(), ignore, 2),
                ("bad".to_string(), illegal, 0),
            ]
        );
        assert_eq!(counted(&report, "Top.ct", 0, "a"), (2, 3));
        // The values are distributed first: <0..3>, <4..7>, <8..11>,
        // <12..15>, and then all[1] has none left.
        assert_eq!(counting("w"), hits(&[("all[0]", 1), ("all[2]", 1), ("all[3]", 0)]));
        assert_eq!(counted(&report, "Top.ct", 0, "w"), (2, 3));
        assert!(close(
            real(&mut sim, "inst"),
            (1.0 + 2.0 / 3.0 + 2.0 / 3.0) / 3.0 * 100.0
        ));
    }

    /// 19.5.6: a sample of an illegal value is a run-time error, which
    /// names the bin and the value, and counts in no `bins` bin that lists
    /// the value too. The simulation goes on. An illegal bin whose guard is
    /// false is no error.
    fn illegal_bin_hit_reports_an_error(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] v;
    bit en;
    covergroup cg;
        cp_a: coverpoint v {
            bins ok = {[0:9]};
            bins all = {[0:15]};
            illegal_bins ill = {15, [10:11]};
            illegal_bins off = {12} iff (en);
        }
    endgroup
    cg c = new;
    initial begin
        v = 3; c.sample();
        v = 15; c.sample();
        v = 12; en = 0; c.sample();
        v = 12; en = 1; c.sample();
        #1 v = 10; c.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(0).unwrap();
        let errors = reports(sim.drain_runtime_events());
        let text = |bin: &str, value: u32| {
            format!(
                "illegal_bins `{bin}` of coverpoint `cp_a` of covergroup `Top.cg` (instance \
                 `Top.cg#0`): value {value}"
            )
        };
        assert_eq!(
            errors,
            [
                (celox::Severity::Error, text("ill", 15)),
                (celox::Severity::Error, text("off", 12)),
            ]
        );
        sim.run_until(2).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(
            reports(sim.drain_runtime_events()),
            [(celox::Severity::Error, text("ill", 10))]
        );
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [5]);
        // `all` has the values 0 to 9, 13 and 14: the illegal values are
        // none of it, 12 neither while the guard of `off` is false.
        assert_eq!(
            bins(&report, "Top.cg", 0, "cp_a"),
            hits(&[("ok", 1), ("all", 1), ("ill", 2), ("off", 1)])
        );
    }

    /// 19.5.4: in a `wildcard` bin an x, z or `?` bit of a value stands for
    /// 0 and for 1; the examples of the clause. A sample with an x or z bit
    /// is in no wildcard bin.
    fn wildcard_bins(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    logic [3:0] v;
    covergroup cg;
        coverpoint v {
            wildcard bins g12_15 = { 4'b11?? };
            wildcard bins g12_15_array[] = { 4'b11?? };
            wildcard bins odd = { 4'bxxx1 };
            wildcard ignore_bins seven = { 4'b0111 };
            wildcard bins low_or_top = { 4'b0z0z, [4'b111x:4'b1111] };
            bins exact = { 4'b0000 };
        }
    endgroup
    cg c = new;
    initial begin
        v = 12; c.sample();
        v = 13; c.sample();
        v = 15; c.sample();
        v = 7; c.sample();
        v = 4'b11x1; c.sample();
        v = 0; c.sample();
        v = 5; c.sample();
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
        // `odd` is 1, 3, 5, 9, 11, 13 and 15 without the ignored 7;
        // `low_or_top` is 0, 1, 4, 5 and the range 14 to 15.
        assert_eq!(
            bins(&report, "Top.cg", 0, "v"),
            hits(&[
                ("g12_15", 3),
                ("g12_15_array[12]", 1),
                ("g12_15_array[13]", 1),
                ("g12_15_array[14]", 0),
                ("g12_15_array[15]", 1),
                ("odd", 3),
                ("seven", 1),
                ("low_or_top", 3),
                ("exact", 1),
            ])
        );
        assert_eq!(counted(&report, "Top.cg", 0, "v"), (7, 8));
    }

    /// 19.5.1: a bin with a guard counts a sample only where the guard
    /// holds. The guard is evaluated at the sample, and reads the
    /// arguments of the covergroup and of its sample method.
    fn bins_iff(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [1:0] v;
    bit en;
    int unsigned gate;
    covergroup cg(int unsigned lim) with function sample(bit ok);
        coverpoint v {
            bins zero = {0} iff (en);
            bins one[] = {1, 2} iff (gate < lim);
            bins three = {3} iff (ok);
            bins any = {[0:3]};
        }
        guarded: coverpoint v iff (ok) {
            bins zero = {0} iff (en);
        }
    endgroup
    cg c = new(3);
    initial begin
        v = 0; c.sample(1);
        en = 1; c.sample(1);
        c.sample(0);
        v = 1; gate = 5; c.sample(1);
        v = 2; gate = 1; c.sample(1);
        v = 3; c.sample(0);
        c.sample(1);
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
            hits(&[("zero", 2), ("one[1]", 0), ("one[2]", 1), ("three", 1), ("any", 7)])
        );
        // The guard of the coverpoint and that of the bin both hold once.
        assert_eq!(bins(&report, "Top.cg", 0, "guarded"), hits(&[("zero", 1)]));
    }

    /// 19.5.1.1: a bin has the values of its range list for which its
    /// `with` expression holds, where `item` is the value; the name of the
    /// coverpoint stands for all its values. These are the two examples of
    /// the clause, then filters on one bin, on `ignore_bins` and on
    /// `illegal_bins`, one that reads an argument of the covergroup, and
    /// one over the values of a 64-bit coverpoint, which is evaluated for
    /// the sampled value.
    fn with_filters(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst);
    bit [7:0] x, b;
    bit [3:0] s;
    longint unsigned big;
    function automatic bit myfunc(bit [7:0] v);
        return v[7] && v[0];
    endfunction
    covergroup cg(int lim);
        a: coverpoint x {
            bins mod3[] = {[0:255]} with (item % 3 == 0);
        }
        coverpoint b {
            bins func[] = b with (myfunc(item));
        }
        coverpoint s {
            bins low = {[0:15]} with (item < lim);
            bins ev[] = {[0:7]};
            ignore_bins odd = {[0:7]} with (item % 2);
            illegal_bins top = {[8:15]} with (item > 13);
        }
        under: coverpoint s {
            bins u[] = {[0:15]} with (item < lim);
        }
        coverpoint big {
            bins even = big with (item % 2 == 0 && item > lim);
            bins few = {[0:9]};
        }
    endgroup
    cg c, d, e;
    initial begin
        c = new(4);
        d = new(1);
        e = new(4);
        x = 9; b = 129; s = 2; big = 6; c.sample();
        x = 10; b = 128; s = 3; big = 7; c.sample();
        x = 255; b = 255; s = 6; big = 64'h100_0000_0000; c.sample();
        s = 14; big = 2; c.sample();
        s = 0; d.sample(); e.sample();
        s = 2; d.sample(); e.sample();
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
        assert_eq!(
            reports(sim.drain_runtime_events()),
            [(
                celox::Severity::Error,
                "illegal_bins `top` of coverpoint `s` of covergroup `Top.cg` (instance \
                 `Top.cg#0`): value 14"
                    .to_string()
            )]
        );
        let report = sim.coverage();
        // The 86 multiples of 3 below 256, and the 64 odd values from 129.
        assert_eq!(counted(&report, "Top.cg", 0, "a"), (2, 86));
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "a")),
            hits(&[("mod3[9]", 1), ("mod3[255]", 2)])
        );
        assert_eq!(counted(&report, "Top.cg", 0, "b"), (2, 64));
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "b")),
            hits(&[("func[129]", 1), ("func[255]", 2)])
        );
        // `low` is 0 and 2 of the values below 4, the odd ones being
        // ignored; `ev` has the four even values; `top` is 14 and 15.
        assert_eq!(
            bins(&report, "Top.cg", 0, "s"),
            hits(&[
                ("low", 1),
                ("ev[0]", 0),
                ("ev[2]", 1),
                ("ev[4]", 0),
                ("ev[6]", 1),
                ("odd", 1),
                ("top", 1),
            ])
        );
        assert_eq!(counted(&report, "Top.cg", 0, "s"), (3, 5));
        // 6 and 2^40 are even and above 4; 2 is not above 4.
        assert_eq!(
            bins(&report, "Top.cg", 0, "big"),
            hits(&[("even", 2), ("few", 3)])
        );
        // The values below the limit 4, of which 2 and 3 were sampled.
        assert_eq!(
            bins(&report, "Top.cg", 0, "under"),
            hits(&[("u[0]", 0), ("u[1]", 0), ("u[2]", 1), ("u[3]", 1)])
        );
        assert!(close(
            real(&mut sim, "inst"),
            (2.0 / 86.0 + 2.0 / 64.0 + 3.0 / 5.0 + 2.0 / 4.0 + 1.0) / 5.0 * 100.0
        ));
        // The second instance has the bins of the limit 1: `u` is the bin
        // of 0, and `low` is 0 alone, so the sample of 2 is not in it. The
        // third, constructed with the arguments of the first after the
        // second, has the bins of the limit 4 again, where `low` has 2.
        assert_eq!(bins(&report, "Top.cg", 1, "under"), hits(&[("u[0]", 1)]));
        assert_eq!(
            bins(&report, "Top.cg", 2, "under"),
            hits(&[("u[0]", 1), ("u[1]", 0), ("u[2]", 1), ("u[3]", 0)])
        );
        let s = |low: u64| {
            hits(&[
                ("low", low),
                ("ev[0]", 1),
                ("ev[2]", 1),
                ("ev[4]", 0),
                ("ev[6]", 0),
                ("odd", 0),
                ("top", 0),
            ])
        };
        assert_eq!(bins(&report, "Top.cg", 1, "s"), s(1));
        assert_eq!(bins(&report, "Top.cg", 2, "s"), s(2));
        assert_eq!(counted(&report, "Top.cg", 1, "s"), (3, 5));
    }

    /// 19.5.1.1 beside 19.8.1: in a `with` expression `item` is the value
    /// the filter is asked about, also where the sample method has an
    /// argument `item`, which the coverpoint expression reads.
    fn with_filter_item_beside_a_sample_argument_named_item(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    covergroup cg with function sample(int item);
        cp: coverpoint item { bins b[] = {[0:3]} with (item > 1); }
    endgroup
    cg c;
    initial begin
        c = new;
        c.sample(0);
        c.sample(2);
        c.sample(3);
        c.sample(3);
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
            bins(&report, "Top.cg", 0, "cp"),
            hits(&[("b[2]", 1), ("b[3]", 2)])
        );
    }

    /// 19.5.1.1: what a `with` expression reads decides the bins of each
    /// instance, a string argument as an integral one. Instances that are
    /// constructed with the same arguments have the same bins, in whatever
    /// order they are constructed.
    fn filters_read_string_arguments(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [2:0] v;
    covergroup cg(string s, string mode);
        option.name = {mode, "/", s};
        a: coverpoint v {
            bins b[] = {[0:5]} with (s == "x" ? item < 2 : item < 4);
        }
        i: coverpoint v {
            bins e[] = {[0:3]};
            ignore_bins skip = {[0:3]} with (mode == "odd" ? item % 2 : 0);
        }
        l: coverpoint v {
            bins n[] = {[0:7]} with (s.len() > item);
        }
    endgroup
    cg c1, c2, c3, c4;
    initial begin
        c1 = new("x", "odd");
        c2 = new("yy", "all");
        c3 = new("x", "odd");
        c4 = new("yy", "odd");
        v = 1; c1.sample(); c2.sample(); c3.sample(); c4.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(reports(sim.drain_runtime_events()), []);
        let report = sim.coverage();
        let of = |instance: usize, item: &str| bins(&report, "Top.cg", instance, item);
        // "x": the values below 2, and one character; else those below 4,
        // and two. "odd": 1 and 3 are ignored, which the sample of 1 hits.
        let (two, four) = (
            hits(&[("b[0]", 0), ("b[1]", 1)]),
            hits(&[("b[0]", 0), ("b[1]", 1), ("b[2]", 0), ("b[3]", 0)]),
        );
        let (odd, all) = (
            hits(&[("e[0]", 0), ("e[2]", 0), ("skip", 1)]),
            hits(&[("e[0]", 0), ("e[1]", 1), ("e[2]", 0), ("e[3]", 0)]),
        );
        let (one, both) = (hits(&[("n[0]", 0)]), hits(&[("n[0]", 0), ("n[1]", 1)]));
        for (instance, a, i, l) in [
            (0, &two, &odd, &one),
            (1, &four, &all, &both),
            (2, &two, &odd, &one),
            (3, &four, &odd, &both),
        ] {
            assert_eq!(&of(instance, "a"), a, "instance {instance}");
            assert_eq!(&of(instance, "i"), i, "instance {instance}");
            assert_eq!(&of(instance, "l"), l, "instance {instance}");
        }
        let names: Vec<_> = group(&report, "Top.cg")
            .instances
            .iter()
            .map(|instance| instance.name.as_str())
            .collect();
        assert_eq!(names, ["odd/x", "all/yy", "odd/x", "odd/yy"]);
    }

    /// 19.5, 19.5.1.1: a function of a `with` expression may read the
    /// constants of the object it is a method of, so the filter of an
    /// embedded covergroup that calls a method of its class keeps what the
    /// object gives: each instance is asked, also one that is constructed
    /// as an earlier one was.
    fn filters_that_call_methods_of_the_class_are_asked_by_each_object(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    class C;
        const int limit;
        int v;
        covergroup cg;
            coverpoint v { bins b[] = {[0:7]} with (ok(item)); }
        endgroup
        function bit ok(int x);
            return x < limit;
        endfunction
        function new(int l);
            limit = l;
            cg = new;
        endfunction
    endclass
    C a, b, c;
    initial begin
        a = new(2);
        b = new(6);
        c = new(2);
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
            [0, 1, 2].map(|instance| bins(&report, "C::cg", instance, "v").len()),
            [2, 6, 2]
        );
    }

    /// 19.5.1.1: a filter with a handle among its operands, the object a
    /// method is called of or one whose member is read, keeps what the
    /// object gives when the instance is constructed: an instance that is
    /// constructed with the handle of an earlier one is asked again.
    fn filters_that_read_objects_are_asked_by_each_construction(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    class K;
        int top;
        function new(int t);
            top = t;
        endfunction
        function bit below(int x);
            return x < top;
        endfunction
    endclass
    bit [2:0] w;
    covergroup by_handle(K h);
        coverpoint w { bins b[] = {[0:7]} with (h.below(item)); }
        m: coverpoint w { bins b[] = {[0:7]} with (item < h.top); }
    endgroup
    K k2, k5;
    by_handle g2, g5, again;
    initial begin
        k2 = new(2);
        k5 = new(5);
        g2 = new(k2);
        g5 = new(k5);
        k2.top = 3;
        again = new(k2);
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
        // The third is constructed with the handle of the first, whose
        // object has changed.
        for item in ["w", "m"] {
            assert_eq!(
                [0, 1, 2].map(|instance| bins(&report, "Top.by_handle", instance, item).len()),
                [2, 5, 3],
                "{item}"
            );
        }
    }

    /// 19.5.1.1, 19.5.3: the name of a coverpoint before `with` stands for
    /// all its values, which for a coverpoint of an enumeration are its
    /// named values, as its automatic bins are.
    fn coverpoint_name_of_an_enumeration_is_its_named_values(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    typedef enum { RED, GREEN = 5, BLUE } color_t;
    color_t color;
    covergroup cg;
        coverpoint color { bins not_green[] = color with (item != GREEN); }
        one: coverpoint color { bins late = one with (item > RED); }
        ig: coverpoint color { ignore_bins first = ig with (item == RED); }
    endgroup
    cg c = new;
    initial begin
        color = BLUE; c.sample();
        color = GREEN; c.sample();
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
            bins(&report, "Top.cg", 0, "color"),
            hits(&[("not_green[0]", 0), ("not_green[6]", 1)])
        );
        assert_eq!(bins(&report, "Top.cg", 0, "one"), hits(&[("late", 2)]));
        // The automatic bins of the two enumerators that are not ignored.
        assert_eq!(
            other_bins(&report, "Top.cg", 0, "ig"),
            [("first".to_string(), celox::BinReportKind::Ignore, 0)]
        );
        assert_eq!(
            bins(&report, "Top.cg", 0, "ig")
                .into_iter()
                .filter(|(name, _)| name != "first")
                .collect::<Vec<_>>(),
            hits(&[("auto[GREEN]", 1), ("auto[BLUE]", 1)])
        );
    }

    /// 11.4.13: the operands of a tolerance range keep their own
    /// signedness. `[A +/- B]` is `[A - type(A)'(B) : A + type(A)'(B)]` and
    /// `[A +%- B]` has `type(A)'(A*B/100.0)` in place of `type(A)'(B)`: an
    /// unsigned center above the middle of its type, an unsigned tolerance
    /// with its top bit set and a negative center, as arguments of the
    /// covergroup and as constants.
    fn tolerance_ranges_keep_the_signedness_of_their_operands(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [7:0] v;
    int w;
    bit signed [7:0] s;
    covergroup byarg(bit [7:0] p, bit [7:0] ub, int a, int na, int nb, bit signed [7:0] m);
        u: coverpoint v {
            bins rel[] = {[p +%- 10]};
            bins both[] = {[p +/- 2], [p +%- 1]};
        }
        i: coverpoint w {
            bins rel[] = {[a +%- ub]};
            bins abs[] = {[a +/- ub]};
            bins neg[] = {[na +%- nb]};
            bins nabs[] = {[na +/- 3]};
        }
        s8: coverpoint s { bins rel[] = {[m +%- 20]}; }
    endgroup
    covergroup consts;
        u: coverpoint v {
            bins rel[] = {[8'd129 +%- 10]};
            bins high[] = {[8'd200 +%- 10]};
            bins both[] = {[8'd129 +/- 2], [8'd129 +%- 1]};
        }
        i: coverpoint w {
            bins rel[] = {[50 +%- 8'd200]};
            bins abs[] = {[50 +/- 8'd200]};
            bins neg[] = {[-40 +%- 25]};
            bins nabs[] = {[-40 +/- 3]};
        }
        s8: coverpoint s { bins rel[] = {[-8'sd100 +%- 20]}; }
    endgroup
    byarg x = new(8'd129, 8'd200, 50, -40, 25, -8'sd100);
    byarg y = new(8'd200, 8'd200, 50, -40, 25, -8'sd100);
    consts k = new;
    initial done = 1;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(reports(sim.drain_runtime_events()), []);
        let report = sim.coverage();
        // The bins of array `bin` of `item`: how many, the first and the
        // last, which are named by their values in rising order.
        let span = |name: &str, instance: usize, item: &str, bin: &str| {
            let prefix = format!("{bin}[");
            let listed: Vec<String> = bins(&report, name, instance, item)
                .into_iter()
                .map(|(name, _)| name)
                .filter(|name| name.starts_with(&prefix))
                .collect();
            (
                listed.len(),
                listed.first().cloned().unwrap_or_default(),
                listed.last().cloned().unwrap_or_default(),
            )
        };
        let expect = |count: usize, bin: &str, low: i32, high: i32| {
            (count, format!("{bin}[{low}]"), format!("{bin}[{high}]"))
        };
        for (name, instance) in [("Top.byarg", 0), ("Top.consts", 0)] {
            // 10 percent of 129 is 12.9, cut to 12; 1 percent is 1.
            assert_eq!(span(name, instance, "u", "rel"), expect(25, "rel", 117, 141), "{name}");
            assert_eq!(span(name, instance, "u", "both"), expect(5, "both", 127, 131), "{name}");
            // 200 percent of 50 is 100, and int'(8'd200) is 200.
            assert_eq!(span(name, instance, "i", "rel"), expect(201, "rel", -50, 150), "{name}");
            assert_eq!(span(name, instance, "i", "abs"), expect(401, "abs", -150, 250), "{name}");
            // 25 percent of -40 is -10: -30 and -50, put in order.
            assert_eq!(span(name, instance, "i", "neg"), expect(21, "neg", -50, -30), "{name}");
            assert_eq!(span(name, instance, "i", "nabs"), expect(7, "nabs", -43, -37), "{name}");
            // 20 percent of -100 is -20: -80 and -120.
            assert_eq!(span(name, instance, "s8", "rel"), expect(41, "rel", -120, -80), "{name}");
        }
        // 10 percent of 200 is 20, and 1 percent is 2.
        assert_eq!(span("Top.byarg", 1, "u", "rel"), expect(41, "rel", 180, 220));
        assert_eq!(span("Top.byarg", 1, "u", "both"), expect(5, "both", 198, 202));
        assert_eq!(span("Top.consts", 0, "u", "high"), expect(41, "high", 180, 220));
    }

    /// 19.5.1.2: the values of a bin may be the elements of an array: a
    /// fixed-size array of constants, also of a package, and a queue that a
    /// function makes of an argument of the covergroup. The elements are
    /// taken in their order.
    fn set_expression_bins(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
package p;
    localparam int unsigned PS [4] = '{5, 1, 9, 3};
endpackage
module Top(output int done);
    typedef int q_t[$];
    localparam bit [3:0] S [3] = '{2, 4, 6};
    bit [3:0] v;
    function automatic q_t doubles(int n);
        q_t q;
        for (int i = 0; i < n; i++) q.push_back(i * 2);
        return q;
    endfunction
    covergroup cg(int n);
        coverpoint v {
            bins s[] = S;
            bins two[2] = p::PS;
            bins q[] = doubles(n);
            ignore_bins none = S iff (0);
        }
    endgroup
    cg c = new(3);
    initial begin
        v = 2; c.sample();
        v = 5; c.sample();
        v = 1; c.sample();
        v = 4; c.sample();
        v = 0; c.sample();
        v = 7; c.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(reports(sim.drain_runtime_events()), []);
        let report = sim.coverage();
        // The ignored 2, 4 and 6 leave `s` without a bin and `q` with the
        // bin of 0. The two bins of `two` are <5,1> and <9,3>, the first
        // element first: 5 and 1 are both in the first.
        assert_eq!(
            bins(&report, "Top.cg", 0, "v"),
            hits(&[("two[0]", 2), ("two[1]", 0), ("q[0]", 1), ("none", 0)])
        );
        assert_eq!(counted(&report, "Top.cg", 0, "v"), (2, 3));
    }

    /// 11.4.13, 19.5.1: `[A +/- B]` is the values from A - B to A + B and
    /// `[A +%- B]` those within B percent of A, the tolerance cut to an
    /// integer: 25 percent of 7 is 1, so the range is 6 to 8, and of -7 it
    /// is -8 to -6, as the clause says. The bounds are put in order, and
    /// may be arguments of the covergroup.
    fn tolerance_ranges(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit signed [7:0] v;
    covergroup cg(int center, int tol);
        coverpoint v {
            bins near = {[10 +/- 2]};
            bins pct = {[7 +%- 25]};
            bins neg = {[-7 +%- 25]};
            bins swapped = {[-20 +/- -3]};
            bins arg[] = {[center +/- tol]};
            bins rel = {[center +%- tol]};
        }
    endgroup
    cg c = new(40, 5);
    initial begin
        take(8); take(12); take(13); take(5);
        take(6); take(-6); take(-8); take(-9);
        take(-23); take(-17); take(-16);
        take(35); take(38); take(42); take(45);
        take(46);
        done = 1;
    end
    task automatic take(int value);
        v = value[7:0];
        c.sample();
    endtask
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(reports(sim.drain_runtime_events()), []);
        let report = sim.coverage();
        // 5 percent of 40 is 2: `rel` is 38 to 42, and `arg` the eleven
        // values 35 to 45.
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "v")),
            hits(&[
                ("near", 2),
                ("pct", 2),
                ("neg", 2),
                ("swapped", 2),
                ("arg[35]", 1),
                ("arg[38]", 1),
                ("arg[42]", 1),
                ("arg[45]", 1),
                ("rel", 2),
            ])
        );
        assert_eq!(counted(&report, "Top.cg", 0, "v"), (9, 16));
    }

    /// 19.5, 19.5.4: a sample with an x or z bit that no bin lists with
    /// those bits is in no bin of values, so a `default` bin counts it, a
    /// `default[]` in a bin of that value, and an `illegal_bins` that is
    /// `default` reports it.
    fn unknown_samples_reach_default_bins(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    logic [3:0] v;
    covergroup cg;
        coverpoint v {
            bins xz = {4'b1x0z};
            bins known = {[0:15]};
            bins other = default;
        }
        each: coverpoint v {
            bins known = {[0:15]};
            bins other[] = default;
        }
        ill: coverpoint v {
            bins known = {[0:15]};
            illegal_bins unknown = default;
        }
    endgroup
    cg c = new;
    initial begin
        v = 4'b1x0z; c.sample();
        v = 4'b1000; c.sample();
        #1 v = 4'bxxxx; c.sample();
        c.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(true);
        sim.run_until(0).unwrap();
        let unknown = |value: &str| {
            (
                celox::Severity::Error,
                format!(
                    "illegal_bins `unknown` of coverpoint `ill` of covergroup `Top.cg` \
                     (instance `Top.cg#0`): value {value}"
                ),
            )
        };
        assert_eq!(reports(sim.drain_runtime_events()), [unknown("4'b1x0z")]);
        sim.run_until(2).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(
            reports(sim.drain_runtime_events()),
            [unknown("4'bxxxx"), unknown("4'bxxxx")]
        );
        let report = sim.coverage();
        assert_eq!(
            bins(&report, "Top.cg", 0, "v"),
            hits(&[("xz", 1), ("known", 1), ("other", 2)])
        );
        assert_eq!(
            bins(&report, "Top.cg", 0, "each"),
            hits(&[("known", 1), ("other[4'b1x0z]", 1), ("other[4'bxxxx]", 2)])
        );
        assert_eq!(bins(&report, "Top.cg", 0, "ill"), hits(&[("known", 1), ("unknown", 3)]));
    }
}

/// 19.5.4: the x and z bits of a bin value are told apart, and in a bin
/// that is not `wildcard` a sample is in the bin only with exactly those
/// bits. Two-state simulation has no such sample: the value is then one of
/// the bins of known values, and of the `default` bin if there is none.
#[test]
fn four_state_planes_reach_bins() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    logic [3:0] v;
    covergroup cg;
        coverpoint v {
            bins xz = {4'b1x0z};
            bins zx = {4'b1z0x};
            wildcard bins wild = {4'b1x0z};
            bins nine = {9};
            bins other = default;
        }
    endgroup
    cg c = new;
    initial begin
        v = 4'b1x0z; c.sample();
        v = 4'b1z0x; c.sample();
        c.sample();
        v = 4'b1100; c.sample();
        v = 4'bz00z; c.sample();
        done = 1;
    end
endmodule
"#;
    for (four_state, expected) in [
        (
            true,
            [("xz", 1), ("zx", 2), ("wild", 1), ("nine", 0), ("other", 1)],
        ),
        // 4'b1000 three times, 4'b1100 and 4'b0000: `wild` is 8, 9, 12
        // and 13.
        (
            false,
            [("xz", 0), ("zx", 0), ("wild", 4), ("nine", 0), ("other", 1)],
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
        assert_eq!(
            bins(&report, "Top.cg", 0, "v"),
            hits(&expected),
            "four_state={four_state}"
        );
    }
}

sv_backends! {
    /// 19.4, 19.5: the bins of an embedded covergroup read the constants of
    /// the object that holds it, and call the methods of its class: two
    /// objects with different constants have different bins. A guard of a
    /// bin reads the members of the object at each sample.
    fn embedded_covergroup_bin_kinds(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    class C;
        const int limit;
        const int allowed [3] = '{7, 1, 12};
        bit [3:0] v;
        bit en;
        covergroup cg;
            coverpoint v {
                bins low[] = {[0:7]} with (item < limit && keep(item));
                bins high = {[8:14]} iff (en);
                bins part[2] = {[limit +/- 1]};
                illegal_bins bad = {15};
            }
            listed: coverpoint v { bins of[2] = allowed; }
        endgroup
        function new(int l);
            limit = l;
            cg = new;
        endfunction
        function bit keep(bit [3:0] x);
            return !x[0];
        endfunction
        function void take(bit [3:0] value, bit on);
            v = value;
            en = on;
            cg.sample();
        endfunction
    endclass
    C a, b;
    initial begin
        a = new(5);
        b = new(3);
        a.take(2, 0); a.take(9, 0); a.take(9, 1); a.take(6, 1); a.take(12, 0);
        b.take(2, 1); b.take(4, 0); b.take(7, 0);
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(reports(sim.drain_runtime_events()), []);
        let report = sim.coverage();
        // The even values below 5, and <4>, <5,6> of 4 to 6.
        assert_eq!(
            bins(&report, "C::cg", 0, "v"),
            hits(&[
                ("low[0]", 0),
                ("low[2]", 1),
                ("low[4]", 0),
                ("high", 1),
                ("part[0]", 0),
                ("part[1]", 1),
                ("bad", 0),
            ])
        );
        // <7>, <1,12> of the array of the object.
        assert_eq!(
            bins(&report, "C::cg", 0, "listed"),
            hits(&[("of[0]", 0), ("of[1]", 1)])
        );
        assert_eq!(
            bins(&report, "C::cg", 1, "listed"),
            hits(&[("of[0]", 1), ("of[1]", 0)])
        );
        // The even values below 3, and <2>, <3,4> of 2 to 4.
        assert_eq!(
            bins(&report, "C::cg", 1, "v"),
            hits(&[
                ("low[0]", 0),
                ("low[2]", 1),
                ("high", 0),
                ("part[0]", 1),
                ("part[1]", 1),
                ("bad", 0),
            ])
        );
    }
}

/// The bins of 19.5.1 to 19.5.6 under the untimed `Simulator`, which
/// constructs the covergroup in the clocked procedure that samples it: the
/// constructor answers the filter in a loop there. At every optimization
/// level and in both state modes.
#[test]
fn bin_kinds_in_clocked_logic_under_simulator() {
    let source = r#"module Top(input bit clk, input logic [3:0] d, output bit [7:0] q);
    covergroup cg;
        coverpoint d {
            bins even[] = {[0:15]} with (item % 2 == 0);
            bins low = {[0:7]} iff (q[0]);
            ignore_bins ig = {2};
            illegal_bins bad = {15};
            bins rest = default;
        }
    endgroup
    cg c;
    always_ff @(posedge clk) q <= q + 8'd1;
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
        for four_state in [false, true] {
            let mut sim =
                Simulator::from_sv_sources(vec![(source, Path::new("covergroup_ff.sv"))], "Top")
                    .four_state(four_state)
                    .opt_level(level)
                    .build()
                    .unwrap();
            let clk = sim.event("clk");
            let d = sim.signal("d");
            // `q` is the number of edges before the one that samples.
            for value in [4u8, 4, 2, 3, 9, 15] {
                sim.modify(|io| io.set(d, value)).unwrap();
                sim.tick(clk).unwrap();
            }
            let context = format!("{level:?} four_state={four_state}");
            assert_eq!(
                reports(sim.drain_runtime_events()),
                [(
                    celox::Severity::Error,
                    "illegal_bins `bad` of coverpoint `d` of covergroup `Top.cg` (instance \
                     `Top.cg#0`): value 15"
                        .to_string()
                )],
                "{context}"
            );
            let report = sim.coverage();
            assert_eq!(samples(&report, "Top.cg"), [6], "{context}");
            // 2 is ignored, so `even[2]` has no value; 4 is sampled with
            // `q` 0 and 1, 3 with `q` 3.
            assert_eq!(
                bins(&report, "Top.cg", 0, "d"),
                hits(&[
                    ("even[0]", 0),
                    ("even[4]", 2),
                    ("even[6]", 0),
                    ("even[8]", 0),
                    ("even[10]", 0),
                    ("even[12]", 0),
                    ("even[14]", 0),
                    ("low", 2),
                    ("ig", 1),
                    ("bad", 1),
                    ("rest", 1),
                ]),
                "{context}"
            );
        }
    }
}

/// The rows of item `item` of instance `instance` of covergroup type
/// `name`: each bin with its kind and hits, and the number of automatic
/// cross bins that are not listed.
fn rows(
    report: &celox::CoverageReport,
    name: &str,
    instance: usize,
    item_name: &str,
) -> (Vec<(String, celox::BinReportKind, u64)>, u64) {
    let item = item(report, name, instance, item_name);
    (
        item.bins
            .iter()
            .map(|bin| (bin.name.clone(), bin.kind, bin.hits))
            .collect(),
        item.omitted_bins,
    )
}

/// `rows` of bins of kind `bins` alone.
fn counting(bins: &[(&str, u64)]) -> Vec<(String, celox::BinReportKind, u64)> {
    bins.iter()
        .map(|(name, hits)| (name.to_string(), celox::BinReportKind::Bins, *hits))
        .collect()
}

sv_backends! {
    /// 19.6: a cross of coverpoints, or of variables, which get a coverpoint
    /// "as if it had been created by the statement `coverpoint V;`", has a
    /// bin for each combination of their bins. These are the examples of
    /// the clause, `cov`, `cov2` and `cov3`, and that of 19.8.1, which
    /// crosses an argument of `sample`.
    fn automatic_crosses_and_implicit_coverpoints(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] a, b, c;
    bit [31:0] a_var;
    bit [3:0] b_var;
    covergroup cov;
        aXb : cross a, b;
    endgroup
    covergroup cov2;
        BC: coverpoint b+c;
        aXb : cross a, BC;
    endgroup
    covergroup cov3;
        A: coverpoint a_var { bins yy[] = {[0:9]}; }
        CC: cross b_var, A;
    endgroup
    covergroup p_cg with function sample(bit s, int x);
        coverpoint x { bins low[] = {[0:3]}; }
        cross x, s;
    endgroup
    cov c1 = new;
    cov2 c2 = new;
    cov3 c3 = new;
    p_cg c4 = new;
    initial begin
        a = 1; b = 2; c = 3; a_var = 4; b_var = 5;
        c1.sample(); c2.sample(); c3.sample();
        a = 15; b = 0; c = 0; a_var = 20; b_var = 1;
        c1.sample(); c2.sample(); c3.sample();
        c4.sample(1, 2); c4.sample(0, 2); c4.sample(1, 9);
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
        // The coverpoints of `a` and `b`, 16 automatic bins each, and 256
        // cross products.
        let names = |name: &str| {
            group(&report, name).instances[0]
                .items
                .iter()
                .map(|item| (item.name.clone(), item.kind, item.coverpoints.clone()))
                .collect::<Vec<_>>()
        };
        let (point, cross) = (celox::CoverItemKind::Coverpoint, celox::CoverItemKind::Cross);
        let crossed = |names: &[&str]| names.iter().map(|name| name.to_string()).collect();
        assert_eq!(
            names("Top.cov"),
            [
                ("a".to_string(), point, vec![]),
                ("b".to_string(), point, vec![]),
                ("aXb".to_string(), cross, crossed(&["a", "b"])),
            ]
        );
        assert_eq!(counted(&report, "Top.cov", 0, "a"), (2, 16));
        assert_eq!(counted(&report, "Top.cov", 0, "aXb"), (2, 256));
        assert_eq!(
            rows(&report, "Top.cov", 0, "aXb"),
            (counting(&[("<auto[1],auto[2]>", 1), ("<auto[15],auto[0]>", 1)]), 254)
        );
        // `BC` is the coverpoint of `b+c`, of four bits: the same number of
        // cross products. The implicit coverpoint follows those declared.
        assert_eq!(
            names("Top.cov2"),
            [
                ("BC".to_string(), point, vec![]),
                ("a".to_string(), point, vec![]),
                ("aXb".to_string(), cross, crossed(&["a", "BC"])),
            ]
        );
        assert_eq!(
            rows(&report, "Top.cov2", 0, "aXb"),
            (counting(&[("<auto[1],auto[5]>", 1), ("<auto[15],auto[0]>", 1)]), 254)
        );
        // 16 × 10 = 160 cross products; a value of `a_var` in no bin of `A`
        // is no sample of the cross.
        assert_eq!(counted(&report, "Top.cov3", 0, "CC"), (1, 160));
        assert_eq!(counted(&report, "Top.cov3", 0, "b_var"), (2, 16));
        assert_eq!(
            rows(&report, "Top.cov3", 0, "CC"),
            (counting(&[("<auto[5],yy[4]>", 1)]), 159)
        );
        // The argument `s` of `sample` gets a coverpoint of its own; the
        // cross without a label is named for reports only.
        assert_eq!(
            names("Top.p_cg"),
            [
                ("x".to_string(), point, vec![]),
                ("s".to_string(), point, vec![]),
                ("$cross1".to_string(), cross, crossed(&["x", "s"])),
            ]
        );
        assert_eq!(counted(&report, "Top.p_cg", 0, "s"), (2, 2));
        assert_eq!(
            rows(&report, "Top.p_cg", 0, "$cross1"),
            (counting(&[("<low[2],auto[0]>", 1), ("<low[2],auto[1]>", 1)]), 6)
        );
    }

    /// 19.6.1: the bins of a cross take the cross products their select
    /// expressions select, and the others stay automatic bins. These are
    /// `x1` and `x2` of the clause and the cross `c` of 19.6.1.1: a sample
    /// of each of its 16 cross products counts 4 in `c1`, 7 in `c2` and 1
    /// in `c3`, and leaves 6 automatic bins (19.11.2: `Bc` = 16 - 10).
    fn cross_bins_select_cross_products(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst);
    int i, j;
    bit [7:0] v_a, v_b;
    covergroup ct;
        coverpoint i { bins i[] = { [0:1] }; }
        coverpoint j { bins j[] = { [0:1] }; }
        x1: cross i,j;
        x2: cross i,j {
            bins i_zero = binsof(i) intersect { 0 };
        }
    endgroup
    covergroup cg;
        a: coverpoint v_a
        {
            bins a1 = { [0:63] };
            bins a2 = { [64:127] };
            bins a3 = { [128:191] };
            bins a4 = { [192:255] };
        }
        b: coverpoint v_b
        {
            bins b1 = {0};
            bins b2 = { [1:84] };
            bins b3 = { [85:169] };
            bins b4 = { [170:255] };
        }
        c : cross a, b
        {
            bins c1 = ! binsof(a) intersect {[100:200]};
            bins c2 = binsof(a.a2) || binsof(b.b2);
            bins c3 = binsof(a.a1) && binsof(b.b4);
        }
    endgroup
    ct t = new;
    cg g = new;
    initial begin
        i = 0; j = 1; t.sample();
        i = 1; j = 0; t.sample();
        for (int x = 0; x < 4; x++)
            for (int y = 0; y < 4; y++) begin
                v_a = x * 64;
                v_b = y == 0 ? 0 : y == 1 ? 50 : y == 2 ? 100 : 200;
                g.sample();
            end
        inst = $realtobits(g.get_inst_coverage());
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
            rows(&report, "Top.ct", 0, "x1"),
            (counting(&[("<i[0],j[1]>", 1), ("<i[1],j[0]>", 1)]), 2)
        );
        assert_eq!(counted(&report, "Top.ct", 0, "x1"), (2, 4));
        // `i_zero`, and the two automatic bins of i == 1.
        assert_eq!(
            rows(&report, "Top.ct", 0, "x2"),
            (counting(&[("i_zero", 1), ("<i[1],j[0]>", 1)]), 1)
        );
        assert_eq!(counted(&report, "Top.ct", 0, "x2"), (2, 3));
        assert_eq!(
            rows(&report, "Top.cg", 0, "c"),
            (
                counting(&[
                    ("c1", 4),
                    ("c2", 7),
                    ("c3", 1),
                    ("<a3,b1>", 1),
                    ("<a3,b3>", 1),
                    ("<a3,b4>", 1),
                    ("<a4,b1>", 1),
                    ("<a4,b3>", 1),
                    ("<a4,b4>", 1),
                ]),
                0
            )
        );
        assert_eq!(counted(&report, "Top.cg", 0, "c"), (9, 9));
        assert_eq!(real(&mut sim, "inst"), 100.0);
    }

    /// 19.6.2: the cross products of `ignore_bins` are excluded from
    /// coverage, also where another bin of the cross selects them; the
    /// example `yy` of the clause keeps 192 of 256. 19.6.3: those of
    /// `illegal_bins` too, and a sample of one is a run-time error, which
    /// names the bin, the cross and the cross product; the example `zz`
    /// takes the illegal value from an argument.
    fn ignore_and_illegal_cross_products(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] a, b;
    bit [1:0] x, y;
    covergroup yy;
        cross a, b
        {
            ignore_bins ignore = binsof(a) intersect { 5, [1:3] };
        }
        ov: cross a, b {
            bins some = binsof(a) intersect {[0:7]};
            ignore_bins ignore = binsof(a) intersect { 5, [1:3] };
        }
    endgroup
    covergroup zz(int bad);
        cross x, y
        {
            illegal_bins illegal = binsof(y) intersect {bad};
        }
    endgroup
    yy cy = new;
    zz cz = new(2);
    initial begin
        a = 2; b = 7; cy.sample();
        a = 4; b = 7; cy.sample();
        x = 1; y = 2; cz.sample();
        #1 x = 1; y = 3; cz.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(0).unwrap();
        assert_eq!(
            reports(sim.drain_runtime_events()),
            [(
                celox::Severity::Error,
                "illegal_bins `illegal` of cross `$cross1` of covergroup `Top.zz` (instance \
                 `Top.zz#0`): <auto[1],auto[2]>"
                    .to_string()
            )]
        );
        sim.run_until(2).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(reports(sim.drain_runtime_events()), []);
        let report = sim.coverage();
        let (ignore, illegal) = (celox::BinReportKind::Ignore, celox::BinReportKind::Illegal);
        assert_eq!(counted(&report, "Top.yy", 0, "$cross1"), (1, 192));
        assert_eq!(
            rows(&report, "Top.yy", 0, "$cross1"),
            (
                vec![
                    ("ignore".to_string(), ignore, 1),
                    ("<auto[4],auto[7]>".to_string(), celox::BinReportKind::Bins, 1),
                ],
                191
            )
        );
        // `some` keeps the values 0, 4, 6 and 7 of `a`, and the ignored
        // sample counts in it no more.
        assert_eq!(counted(&report, "Top.yy", 0, "ov"), (1, 129));
        assert_eq!(
            rows(&report, "Top.yy", 0, "ov"),
            (
                vec![
                    ("some".to_string(), celox::BinReportKind::Bins, 1),
                    ("ignore".to_string(), ignore, 1),
                ],
                128
            )
        );
        // 16 cross products, 4 of them illegal.
        assert_eq!(counted(&report, "Top.zz", 0, "$cross1"), (1, 12));
        assert_eq!(
            rows(&report, "Top.zz", 0, "$cross1"),
            (
                vec![
                    ("illegal".to_string(), illegal, 1),
                    ("<auto[1],auto[3]>".to_string(), celox::BinReportKind::Bins, 1),
                ],
                11
            )
        );
    }

    /// 19.6: a cross whose guard is false at a sample is not sampled, and a
    /// bin of a cross whose guard is false counts nothing; its cross
    /// products stay those of no other bin. A cross of a coverpoint that is
    /// not sampled, as its guard is false, is not sampled either.
    fn cross_iff_and_bin_iff(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [1:0] p, q;
    bit en, on;
    covergroup cg;
        cp: coverpoint p { bins lo = {[0:1]}; bins hi = {[2:3]}; }
        cq: coverpoint q { bins lo = {[0:1]}; bins hi = {[2:3]}; }
        gp: coverpoint p iff (on) { bins lo = {[0:1]}; bins hi = {[2:3]}; }
        g: cross cp, cq iff (en) {
            bins both_lo = binsof(cp.lo) && binsof(cq.lo) iff (on);
        }
        u: cross cp, cq;
        x: cross gp, cq;
    endgroup
    cg c = new;
    initial begin
        en = 1; on = 1; p = 0; q = 0; c.sample();
        en = 1; on = 0; p = 0; q = 1; c.sample();
        en = 0; on = 1; p = 3; q = 3; c.sample();
        en = 1; on = 1; p = 2; q = 0; c.sample();
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
            rows(&report, "Top.cg", 0, "g"),
            (counting(&[("both_lo", 1), ("<hi,lo>", 1)]), 2)
        );
        assert_eq!(counted(&report, "Top.cg", 0, "g"), (2, 4));
        assert_eq!(
            rows(&report, "Top.cg", 0, "u"),
            (counting(&[("<lo,lo>", 2), ("<hi,lo>", 1), ("<hi,hi>", 1)]), 1)
        );
        // The second sample is none of `gp`, so none of `x`.
        assert_eq!(
            rows(&report, "Top.cg", 0, "x"),
            (counting(&[("<lo,lo>", 1), ("<hi,lo>", 1), ("<hi,hi>", 1)]), 1)
        );
    }

    /// Table 19-1: with `cross_retain_auto_bins` clear, the bins a cross
    /// declares remove every automatic bin; with only an `ignore_bins`, the
    /// cross has no bin and takes no part in the coverage of the covergroup
    /// (19.11.2). Set for the covergroup it is the default of its crosses
    /// (Table 19-2), which a cross may set again.
    fn cross_retain_auto_bins_off(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst, output longint unsigned none);
    bit [1:0] p, q;
    covergroup cg;
        option.cross_retain_auto_bins = 0;
        cp: coverpoint p { bins lo = {[0:1]}; bins hi = {[2:3]}; }
        cq: coverpoint q { bins lo = {[0:1]}; bins hi = {[2:3]}; }
        kept: cross cp, cq { bins b = binsof(cp.lo); }
        nothing: cross cp, cq { ignore_bins i = binsof(cp.hi); }
        retained: cross cp, cq { option.cross_retain_auto_bins = 1; bins b = binsof(cp.lo); }
    endgroup
    cg c = new;
    initial begin
        p = 0; q = 0; c.sample();
        p = 3; q = 3; c.sample();
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
        assert_eq!(rows(&report, "Top.cg", 0, "kept"), (counting(&[("b", 1)]), 0));
        assert_eq!(counted(&report, "Top.cg", 0, "kept"), (1, 1));
        assert_eq!(
            rows(&report, "Top.cg", 0, "nothing"),
            (vec![("i".to_string(), celox::BinReportKind::Ignore, 1)], 0)
        );
        assert_eq!(counted(&report, "Top.cg", 0, "nothing"), (0, 0));
        assert_eq!(item(&report, "Top.cg", 0, "nothing").coverage, 0.0);
        assert_eq!(
            rows(&report, "Top.cg", 0, "retained"),
            (counting(&[("b", 1), ("<hi,hi>", 1)]), 1)
        );
        assert_eq!(counted(&report, "Top.cg", 0, "retained"), (2, 3));
        // `cp`, `cq` and `kept` are covered, `retained` is to 2 of 3.
        assert!(close(
            real(&mut sim, "inst"),
            (100.0 + 100.0 + 100.0 + 200.0 / 3.0) / 4.0
        ));
    }

    /// 19.7, Table 19-2: a cross has `weight`, `goal`, `comment`,
    /// `at_least` and `cross_num_print_missing`; `at_least` and
    /// `cross_num_print_missing` of the covergroup are the defaults of its
    /// crosses. The values are expressions of the scope of `new`.
    fn cross_options(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned inst);
    bit [1:0] p, q;
    covergroup cg(int w);
        option.at_least = 2;
        option.cross_num_print_missing = 2;
        cp: coverpoint p { bins lo = {[0:1]}; bins hi = {[2:3]}; }
        cq: coverpoint q { bins lo = {[0:1]}; bins hi = {[2:3]}; }
        x: cross cp, cq {
            option.weight = w;
            option.goal = 90;
            option.comment = "pairs";
            option.at_least = 1;
            option.cross_num_print_missing = 1;
        }
        y: cross cp, cq;
    endgroup
    cg c = new(3);
    initial begin
        p = 0; q = 0; c.sample();
        c.sample();
        p = 2; c.sample();
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
        let x = item(&report, "Top.cg", 0, "x");
        assert_eq!(
            (x.weight, x.goal, x.comment.as_str(), x.at_least),
            (3, 90, "pairs", 1)
        );
        let y = item(&report, "Top.cg", 0, "y");
        assert_eq!((y.weight, y.goal, y.comment.as_str(), y.at_least), (1, 100, "", 2));
        // One missing bin of `x` is listed, two of `y`.
        assert_eq!(
            rows(&report, "Top.cg", 0, "x"),
            (counting(&[("<lo,lo>", 2), ("<hi,lo>", 1), ("<lo,hi>", 0)]), 1)
        );
        assert_eq!(counted(&report, "Top.cg", 0, "x"), (2, 4));
        assert_eq!(
            rows(&report, "Top.cg", 0, "y"),
            (
                counting(&[("<lo,lo>", 2), ("<hi,lo>", 1), ("<lo,hi>", 0), ("<hi,hi>", 0)]),
                0
            )
        );
        assert_eq!(counted(&report, "Top.cg", 0, "y"), (1, 4));
        assert!(close(
            real(&mut sim, "inst"),
            (50.0 + 50.0 + 3.0 * 50.0 + 25.0) / 6.0
        ));
    }

    /// The covergroup of a monitor class with an overridden `sample`, of
    /// the shape a protocol coverage collector has: a `default` bin, an
    /// illegal value, a bin array, and a cross of three coverpoints that
    /// ignores combinations by the values of two of them. Bins that are
    /// `default`, ignored or illegal have no cross products (19.6): 3 × 4 ×
    /// 5 = 60 cross products, of which 4 + 10 are ignored.
    fn cross_of_a_monitor_class(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    class txn;
        bit [1:0] burst;
        bit [2:0] size;
        bit [3:0] len;
        function new(bit [1:0] b, bit [2:0] s, bit [3:0] l);
            burst = b; size = s; len = l;
        endfunction
    endclass
    class monitor;
        covergroup cg with function sample(txn t);
            BURST: coverpoint t.burst {
                bins fixed = {0};
                bins incr = {1};
                bins wrap = {2};
                illegal_bins reserved = {3};
            }
            SIZE: coverpoint t.size {
                bins sizes[] = {[0:3]};
                bins other = default;
            }
            LEN: coverpoint t.len {
                bins one = {0};
                bins many[] = {1, 3, 7, 15};
                bins rest = default;
            }
            BXSXL: cross BURST, SIZE, LEN {
                ignore_bins wrap_one = binsof(BURST) intersect {2} && binsof(LEN) intersect {0};
                ignore_bins fixed_big = binsof(BURST) intersect {0} && binsof(SIZE) intersect {[2:3]};
            }
        endgroup
        function new();
            cg = new;
        endfunction
        function void write(txn t);
            cg.sample(t);
        endfunction
    endclass
    monitor m;
    task automatic put(bit [1:0] b, bit [2:0] s, bit [3:0] l);
        txn t;
        t = new(b, s, l);
        m.write(t);
    endtask
    initial begin
        m = new;
        put(1, 0, 0);
        put(2, 1, 0);
        put(0, 3, 7);
        put(1, 5, 1);
        #1 put(3, 0, 0);
        #1 put(0, 1, 15);
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(0).unwrap();
        assert_eq!(reports(sim.drain_runtime_events()), []);
        sim.run_until(3).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let errors = reports(sim.drain_runtime_events());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].0, celox::Severity::Error);
        assert!(
            errors[0].1.starts_with("illegal_bins `reserved` of coverpoint `BURST` of covergroup `monitor::cg`")
                && errors[0].1.ends_with("value 3"),
            "{errors:?}"
        );
        let report = sim.coverage();
        assert_eq!(samples(&report, "monitor::cg"), [6]);
        assert_eq!(counted(&report, "monitor::cg", 0, "BXSXL"), (2, 46));
        let ignore = celox::BinReportKind::Ignore;
        assert_eq!(
            rows(&report, "monitor::cg", 0, "BXSXL"),
            (
                vec![
                    ("wrap_one".to_string(), ignore, 1),
                    ("fixed_big".to_string(), ignore, 1),
                    ("<fixed,sizes[1],many[15]>".to_string(), celox::BinReportKind::Bins, 1),
                    ("<incr,sizes[0],one>".to_string(), celox::BinReportKind::Bins, 1),
                ],
                44
            )
        );
    }

    /// 19.6.1: `!binsof`, `&&`, `||` and parentheses combine the bins that
    /// `binsof` conditions select, of a coverpoint or of one bin
    /// declaration of it; the values of `intersect` may be arguments of the
    /// covergroup and calls of functions (19.5). The identifier of the
    /// cross selects every bin tuple (19.6.1.2).
    fn select_expressions(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] a;
    bit [1:0] b;
    function automatic int twice(int v);
        return 2 * v;
    endfunction
    covergroup cg(int lo, int hi);
        ca: coverpoint a { bins low[] = {[0:3]}; bins mid = {[4:7]}; bins top = {[8:15]}; }
        cb: coverpoint b;
        S: cross ca, cb {
            bins neg = !binsof(ca.low) && binsof(cb) intersect {[lo:hi]};
            bins par = (binsof(ca.mid) || binsof(ca.top)) && !binsof(cb) intersect {twice(1)};
            bins arr = binsof(ca.low) intersect {[1:2]};
        }
        T: cross ca, cb { bins all = T; }
    endgroup
    cg one = new(1, 2);
    cg two = new(3, 3);
    initial begin
        a = 5; b = 1; one.sample();
        a = 9; b = 2; one.sample();
        a = 0; b = 0; one.sample();
        a = 2; b = 3; one.sample();
        a = 5; b = 3; two.sample();
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
        // `neg` is <mid|top, 1|2>, `par` <mid|top, 0|1|3>, `arr` <low[1]|low[2],
        // any>: 16 cross products, so 8 are automatic.
        assert_eq!(
            rows(&report, "Top.cg", 0, "S"),
            (
                counting(&[("neg", 2), ("par", 1), ("arr", 1), ("<low[0],auto[0]>", 1)]),
                7
            )
        );
        assert_eq!(counted(&report, "Top.cg", 0, "S"), (4, 11));
        assert_eq!(rows(&report, "Top.cg", 0, "T"), (counting(&[("all", 4)]), 0));
        assert_eq!(counted(&report, "Top.cg", 0, "T"), (1, 1));
        // With `lo` = `hi` = 3, `neg` is <mid|top, 3>: 14 cross products in
        // bins, 10 automatic.
        assert_eq!(
            rows(&report, "Top.cg", 1, "S"),
            (counting(&[("neg", 1), ("par", 1), ("arr", 0)]), 10)
        );
        assert_eq!(counted(&report, "Top.cg", 1, "S"), (2, 13));
    }

    /// The report of a cross lists the automatic bins that were sampled and
    /// as many others as `option.cross_num_print_missing` asks for, whatever
    /// the number of cross products: a cross of seven of nine one-bit
    /// coverpoints has 128, one of all nine 512.
    fn large_cross_report_is_bounded(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [8:0] d;
    covergroup cg;
        p0: coverpoint d[0]; p1: coverpoint d[1]; p2: coverpoint d[2];
        p3: coverpoint d[3]; p4: coverpoint d[4]; p5: coverpoint d[5];
        p6: coverpoint d[6]; p7: coverpoint d[7]; p8: coverpoint d[8];
        seven: cross p0, p1, p2, p3, p4, p5, p6;
        nine: cross p0, p1, p2, p3, p4, p5, p6, p7, p8 { option.cross_num_print_missing = 5; }
    endgroup
    cg c = new;
    initial begin
        d = 0; c.sample();
        d = 9'h1ff; c.sample();
        d = 9'h0aa; c.sample();
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
        let (listed, omitted) = rows(&report, "Top.cg", 0, "seven");
        assert_eq!(listed.len(), 3);
        assert_eq!(omitted, 125);
        assert_eq!(counted(&report, "Top.cg", 0, "seven"), (3, 128));
        let (listed, omitted) = rows(&report, "Top.cg", 0, "nine");
        assert_eq!(listed.len(), 8);
        assert_eq!(omitted, 504);
        assert_eq!(counted(&report, "Top.cg", 0, "nine"), (3, 512));
        assert_eq!(
            listed[0].0,
            "<auto[0],auto[0],auto[0],auto[0],auto[0],auto[0],auto[0],auto[0],auto[0]>"
        );
    }
}

sv_backends! {
    /// 19.4, 19.6: a cross in a class crosses the properties of the object
    /// as variables, each with a coverpoint of its own.
    fn embedded_cross_of_class_properties(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    class C;
        bit [1:0] a, b;
        covergroup cg;
            ab: cross a, b {
                ignore_bins same = binsof(a) intersect {0} && binsof(b) intersect {0};
            }
        endgroup
        function new();
            cg = new;
        endfunction
        function void take(bit [1:0] x, bit [1:0] y);
            a = x;
            b = y;
            cg.sample();
        endfunction
    endclass
    C o;
    initial begin
        o = new;
        o.take(0, 0);
        o.take(1, 2);
        o.take(3, 3);
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
        assert_eq!(counted(&report, "C::cg", 0, "a"), (3, 4));
        assert_eq!(counted(&report, "C::cg", 0, "ab"), (2, 15));
        assert_eq!(
            rows(&report, "C::cg", 0, "ab"),
            (
                vec![
                    ("same".to_string(), celox::BinReportKind::Ignore, 1),
                    ("<auto[1],auto[2]>".to_string(), celox::BinReportKind::Bins, 1),
                    ("<auto[3],auto[3]>".to_string(), celox::BinReportKind::Bins, 1),
                ],
                13
            )
        );
    }

    /// 19.6.1, 19.5.7: the values of an `intersect` list are resolved as
    /// those of a bin of the coverpoint of its `binsof`. One that an
    /// argument gives and that the coverpoint has not is warned of by the
    /// `new` that constructs the instance, and selects nothing.
    fn intersect_values_of_arguments_warn_at_new(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [1:0] x, y;
    covergroup cg(int v);
        c: cross x, y { bins b = binsof(x) intersect {v}; }
    endgroup
    cg fits, outside;
    initial begin
        fits = new(1);
        #1 outside = new(9);
        x = 1; y = 0; fits.sample(); outside.sample();
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
        assert_eq!(warned[0].0, celox::Severity::Warning);
        assert!(
            warned[0].1.contains("bins `b` of cross `c`") && warned[0].1.contains('9'),
            "{warned:?}"
        );
        let report = sim.coverage();
        assert_eq!(rows(&report, "Top.cg", 0, "c"), (counting(&[("b", 1)]), 12));
        assert_eq!(counted(&report, "Top.cg", 0, "c"), (1, 13));
        assert_eq!(
            rows(&report, "Top.cg", 1, "c"),
            (counting(&[("<auto[1],auto[0]>", 1)]), 15)
        );
        assert_eq!(counted(&report, "Top.cg", 1, "c"), (1, 16));
    }
}

/// An `intersect` list that meets values of a bin whose `with` filter a
/// sample evaluates, which no construction sorts, is not supported: the
/// construction ends the simulation with an error that says so.
#[test]
fn intersect_with_values_only_a_sample_sorts_ends_the_simulation() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done);
    int x;
    bit y;
    covergroup cg;
        cx: coverpoint x { bins even = cx with (item % 2 == 0); }
        c: cross cx, y { bins b = binsof(cx) intersect {4}; }
    endgroup
    cg h;
    initial begin
        h = new;
        done = 1;
    end
endmodule
"#;
    let mut sim = simulation(source).four_state(false).build().unwrap();
    let error = format!(
        "{:?}",
        sim.run_until(1).expect_err("the construction faults")
    );
    assert!(
        error.contains("covergroup cross `intersect` of bins `b` of cross `c`")
            && error.contains("only a sample sorts into bins"),
        "{error}"
    );
}

/// Crosses under the untimed `Simulator`, in the clocked procedure that
/// constructs and samples the covergroup: the guard of the cross reads the
/// state of the design, a variable crossed is an input, and a sample of an
/// illegal cross product is reported. At every optimization level and in
/// both state modes.
#[test]
fn crosses_in_clocked_logic_under_simulator() {
    let source = r#"module Top(input bit clk, input logic [3:0] d, input logic e, output bit [7:0] q);
    covergroup cg;
        cd: coverpoint d { bins low[] = {[0:3]}; bins high = {[4:14]}; illegal_bins bad = {15}; }
        x: cross cd, e iff (q[0] || q == 8'd4) {
            bins hi_on = binsof(cd.high) && binsof(e) intersect {1};
            illegal_bins no = binsof(cd.low) intersect {3} && binsof(e) intersect {1};
        }
    endgroup
    cg c;
    always_ff @(posedge clk) q <= q + 8'd1;
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
        for four_state in [false, true] {
            let mut sim =
                Simulator::from_sv_sources(vec![(source, Path::new("covergroup_ff.sv"))], "Top")
                    .four_state(four_state)
                    .opt_level(level)
                    .build()
                    .unwrap();
            let clk = sim.event("clk");
            let d = sim.signal("d");
            let e = sim.signal("e");
            // `q` is the number of edges before the one that samples.
            for (value, on) in [(4u8, 1u8), (4, 1), (3, 1), (3, 1), (15, 0), (0, 0)] {
                sim.modify(|io| {
                    io.set(d, value);
                    io.set(e, on);
                })
                .unwrap();
                sim.tick(clk).unwrap();
            }
            let context = format!("{level:?} four_state={four_state}");
            assert_eq!(
                reports(sim.drain_runtime_events()),
                [
                    (
                        celox::Severity::Error,
                        "illegal_bins `no` of cross `x` of covergroup `Top.cg` (instance \
                         `Top.cg#0`): <low[3],auto[1]>"
                            .to_string()
                    ),
                    (
                        celox::Severity::Error,
                        "illegal_bins `bad` of coverpoint `cd` of covergroup `Top.cg` (instance \
                         `Top.cg#0`): value 15"
                            .to_string()
                    ),
                ],
                "{context}"
            );
            let report = sim.coverage();
            assert_eq!(samples(&report, "Top.cg"), [6], "{context}");
            assert_eq!(
                rows(&report, "Top.cg", 0, "x"),
                (
                    vec![
                        ("hi_on".to_string(), celox::BinReportKind::Bins, 1),
                        ("no".to_string(), celox::BinReportKind::Illegal, 1),
                        (
                            "<low[0],auto[0]>".to_string(),
                            celox::BinReportKind::Bins,
                            1
                        ),
                    ],
                    7
                ),
                "{context}"
            );
            assert_eq!(counted(&report, "Top.cg", 0, "x"), (2, 9), "{context}");
        }
    }
}

sv_backends! {
    /// 19.7: the options of an instance and of its items assigned after
    /// the instance was constructed change its coverage from then on:
    /// `at_least` of the covergroup is that of every item that does not
    /// assign its own (Table 19-2), `weight` of an item weighs it in the
    /// instance; `goal`, `comment` and `name` change only what the report
    /// shows. A procedure reads them back.
    fn procedural_options(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned r0, r1, r2, r3,
           output int weight, at_least, inherited, goal, named);
    bit [1:0] a, b;
    covergroup cg;
        ca: coverpoint a { bins lo = {0}; bins mid = {1}; bins hi = {[2:3]}; }
        cb: coverpoint b;
    endgroup
    cg c = new;
    initial begin
        a = 0; b = 0; c.sample();
        a = 0; b = 1; c.sample();
        a = 1; b = 1; c.sample();
        r0 = $realtobits(c.get_inst_coverage());
        c.option.at_least = 2;
        r1 = $realtobits(c.get_inst_coverage());
        c.ca.option.weight = 3;
        r2 = $realtobits(c.get_inst_coverage());
        c.cb.option.at_least = 1;
        r3 = $realtobits(c.get_inst_coverage());
        c.option.goal = 75;
        c.option.comment = "late";
        c.option.name = "renamed";
        c.ca.option.comment = $sformatf("of %s", "ca");
        weight = c.ca.option.weight;
        at_least = c.cb.option.at_least;
        inherited = c.ca.option.at_least;
        goal = c.option.goal;
        named = c.option.name == "renamed" && c.ca.option.comment == "of ca";
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        // `ca` 2 of 3 bins, `cb` 2 of 4.
        assert!(close(real(&mut sim, "r0"), (200.0 / 3.0 + 50.0) / 2.0));
        // At least two hits: `ca` 1 of 3, `cb` 1 of 4.
        assert!(close(real(&mut sim, "r1"), (100.0 / 3.0 + 25.0) / 2.0));
        assert!(close(real(&mut sim, "r2"), (100.0 + 25.0) / 4.0));
        assert!(close(real(&mut sim, "r3"), (100.0 + 50.0) / 4.0));
        for (name, value) in [
            ("weight", 3u8),
            ("at_least", 1),
            ("inherited", 2),
            ("goal", 75),
            ("named", 1),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
        let report = sim.coverage();
        let instance = &group(&report, "Top.cg").instances[0];
        assert_eq!(
            (instance.name.as_str(), instance.comment.as_str()),
            ("renamed", "late")
        );
        assert_eq!((instance.goal, instance.at_least), (75, 2));
        let ca = item(&report, "Top.cg", 0, "ca");
        assert_eq!(
            (ca.weight, ca.at_least, ca.comment.as_str()),
            (3, 2, "of ca")
        );
        assert_eq!(counted(&report, "Top.cg", 0, "cb"), (2, 4));
    }

    /// 19.7.1: `type_option` of the type and of its items, through `::`
    /// before any instance exists, as in the example of the clause, and in
    /// the definition. With `merge_instances` the coverage of the type is
    /// "((merge of coverage of a from all instances) × 3) + ((merge of
    /// coverage of b from all instances) × 5) ) / (3 + 5)".
    fn type_options_through_scope(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned ty, ta, tb, output int covered, total,
           output int weight, merged);
    bit [1:0] a_var, b_var;
    covergroup gc;
        type_option.goal = 90;
        a: coverpoint a_var;
        b: coverpoint b_var { type_option.weight = 2; }
    endgroup
    gc g1, g2;
    initial begin
        gc::type_option.comment = "Here is a comment for all covergroups of type gc";
        gc::a::type_option.weight = 3;
        g1 = new;
        gc::b::type_option.weight = 5;
        gc::type_option.merge_instances = 1;
        g2 = new;
        a_var = 0; b_var = 0; g1.sample();
        a_var = 1; g1.sample();
        b_var = 3; g2.sample();
        a_var = 2; g2.sample();
        ty = $realtobits(gc::get_coverage());
        ta = $realtobits(gc::a::get_coverage());
        tb = $realtobits(gc::b::get_coverage(covered, total));
        weight = gc::a::type_option.weight + 10 * gc::b::type_option.weight;
        merged = g1.type_option.merge_instances;
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        // `a` has 3 of its 4 bins in the union, `b` 2.
        assert!(close(real(&mut sim, "ty"), (75.0 * 3.0 + 50.0 * 5.0) / 8.0));
        assert_eq!(real(&mut sim, "ta"), 75.0);
        assert_eq!(real(&mut sim, "tb"), 50.0);
        for (name, value) in [
            ("covered", 2u8),
            ("total", 4),
            ("weight", 53),
            ("merged", 1),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
        let report = sim.coverage();
        let gc = group(&report, "Top.gc");
        assert_eq!(
            gc.comment,
            "Here is a comment for all covergroups of type gc"
        );
        assert!(gc.merge_instances);
        assert_eq!((gc.weight, gc.goal), (1, 90));
        let weights: Vec<_> = gc.items.iter().map(|item| (item.name.as_str(), item.weight)).collect();
        assert_eq!(weights, [("a", 3), ("b", 5)]);
    }

    /// 19.11.3: with `merge_instances`, the bins of the instances are
    /// united by name, and `get_inst_coverage` is the coverage of the
    /// instance only with `option.get_inst_coverage`, else that of the type
    /// (Table 19-1). The two examples of the clause: `gt`, where `b[1]` is
    /// a bin of both instances, and `ga`, whose instances have bins of no
    /// common name, 96 in all.
    fn merge_instances(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned gt_type, gv1_inst, gv2_inst,
           gu_type, gu1_inst, ga_type, ga_avg, output int ga_covered, ga_total);
    bit [7:0] a;
    covergroup gt (int l, h);
        type_option.merge_instances = 1;
        option.get_inst_coverage = 1;
        coverpoint a {bins b[] = { [l:h] };}
    endgroup
    covergroup gu (int l, h);
        type_option.merge_instances = 1;
        coverpoint a {bins b[] = { [l:h] };}
    endgroup
    covergroup ga (int abm);
        option.auto_bin_max = abm;
        coverpoint a {ignore_bins i = {3};}
    endgroup
    gt gv1 = new (0,1);
    gt gv2 = new (1,2);
    gu gu1 = new (0,1);
    gu gu2 = new (1,2);
    ga gav1 = new (64);
    ga gav2 = new (32);
    initial begin
        a = 0; gv1.sample(); gu1.sample(); gav1.sample();
        a = 1; gv1.sample(); gv2.sample(); gu1.sample(); gu2.sample(); gav2.sample();
        gt_type = $realtobits(gt::get_coverage());
        gv1_inst = $realtobits(gv1.get_inst_coverage());
        gv2_inst = $realtobits(gv2.get_inst_coverage());
        gu_type = $realtobits(gu::get_coverage());
        gu1_inst = $realtobits(gu1.get_inst_coverage());
        ga_avg = $realtobits(ga::get_coverage());
        ga::type_option.merge_instances = 1;
        ga_type = $realtobits(ga::get_coverage(ga_covered, ga_total));
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert!(close(real(&mut sim, "gt_type"), 200.0 / 3.0));
        assert_eq!(real(&mut sim, "gv1_inst"), 100.0);
        assert_eq!(real(&mut sim, "gv2_inst"), 50.0);
        // Without `option.get_inst_coverage` an instance gives the coverage
        // of the type.
        assert!(close(real(&mut sim, "gu_type"), 200.0 / 3.0));
        assert!(close(real(&mut sim, "gu1_inst"), 200.0 / 3.0));
        // The weighted average of 1 of 64 and 1 of 32, then the union: 2 of
        // 96.
        assert!(close(real(&mut sim, "ga_avg"), (100.0 / 64.0 + 100.0 / 32.0) / 2.0));
        assert!(close(real(&mut sim, "ga_type"), 200.0 / 96.0));
        for (name, value) in [("ga_covered", 2u8), ("ga_total", 96)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
        let report = sim.coverage();
        // The bins of the type: `b[1]` with the hits of both instances.
        let gt = group(&report, "Top.gt");
        let point = &gt.items[0];
        assert_eq!(
            point
                .bins
                .iter()
                .map(|bin| (bin.name.as_str(), bin.hits))
                .collect::<Vec<_>>(),
            [("b[0]", 1), ("b[1]", 2), ("b[2]", 0)]
        );
        assert_eq!((gt.covered, gt.total), (2, 3));
    }

    /// 19.11.3: the automatic bins of a cross are united over the instances
    /// by their names, "<binname1,...,binnameN>", as the bins of its
    /// coverpoints are; without `merge_instances` the type is the average
    /// of its instances.
    fn merge_instances_with_crosses(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned merged, averaged, cross_merged,
           output int covered, total);
    bit a, b;
    covergroup cg;
        ca: coverpoint a;
        cb: coverpoint b;
        x: cross ca, cb;
    endgroup
    cg c1 = new, c2 = new;
    initial begin
        a = 0; b = 0; c1.sample(); c2.sample();
        a = 1; b = 1; c2.sample();
        averaged = $realtobits(cg::get_coverage());
        cg::type_option.merge_instances = 1;
        merged = $realtobits(cg::get_coverage());
        cross_merged = $realtobits(cg::x::get_coverage(covered, total));
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        // `c1`: 50, 50 and 1 of 4; `c2`: 100, 100 and 2 of 4.
        assert!(close(
            real(&mut sim, "averaged"),
            ((50.0 + 50.0 + 25.0) / 3.0 + (100.0 + 100.0 + 50.0) / 3.0) / 2.0
        ));
        // The union: both coverpoints whole, 2 of the 4 cross bins.
        assert!(close(real(&mut sim, "merged"), (100.0 + 100.0 + 50.0) / 3.0));
        assert_eq!(real(&mut sim, "cross_merged"), 50.0);
        for (name, value) in [("covered", 2u8), ("total", 4)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
    }

    /// 19.8: with the two `ref int` arguments, `get_coverage` and
    /// `get_inst_coverage` give the covered bins and the bins, of an item
    /// or of the covergroup, of an instance or of all instances. The
    /// example of the clause, with the values sampled as arguments of
    /// `sample` (a `ref` argument of the covergroup is not supported):
    /// totals 2, 5, 6 and 16.
    fn coverage_counts_by_reference(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output int c0, t0, c1, t1, c2, t2, c3, t3,
           output longint unsigned r0, r1, r2, r3);
    covergroup cg (int xb, yb) with function sample(int x, int y);
        coverpoint x {bins xbins[] = { [0:xb] }; }
        coverpoint y {bins ybins[] = { [0:yb] }; }
    endgroup
    cg cv1 = new (1,2); // cv1.x has 2 bins, cv1.y has 3 bins
    cg cv2 = new (3,6); // cv2.x has 4 bins, cv2.y has 7 bins
    int covered, total;
    initial begin
        cv1.sample(0, 0);
        cv2.sample(3, 6);
        r0 = $realtobits(cv1.x.get_inst_coverage(covered,total)); c0 = covered; t0 = total;
        r1 = $realtobits(cv1.get_inst_coverage(covered,total)); c1 = covered; t1 = total;
        r2 = $realtobits(cg::x::get_coverage(covered,total)); c2 = covered; t2 = total;
        r3 = $realtobits(cg::get_coverage(covered,total)); c3 = covered; t3 = total;
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        for (name, value) in [
            ("c0", 1u8),
            ("t0", 2),
            ("c1", 2),
            ("t1", 5),
            ("c2", 2),
            ("t2", 6),
            ("c3", 4),
            ("t3", 16),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
        assert_eq!(real(&mut sim, "r0"), 50.0);
        assert!(close(real(&mut sim, "r1"), (50.0 + 100.0 / 3.0) / 2.0));
        // The type coverage of `x` is the average of its coverage in the
        // instances, of the covergroup that of the instances (19.11.3).
        assert!(close(real(&mut sim, "r2"), (50.0 + 25.0) / 2.0));
        assert!(close(
            real(&mut sim, "r3"),
            ((50.0 + 100.0 / 3.0) / 2.0 + (25.0 + 100.0 / 7.0) / 2.0) / 2.0
        ));
    }

    /// 19.8, Table 19-5: `get_coverage` and `get_inst_coverage` of a
    /// coverpoint and of a cross, through an instance and, for the type,
    /// through `::`.
    fn item_methods(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned pi, pt, ps, xi, xt, xs);
    bit [1:0] a, b;
    covergroup cg;
        pa: coverpoint a { bins lo = {[0:1]}; bins hi = {[2:3]}; }
        coverpoint b { bins lo = {[0:1]}; bins hi = {[2:3]}; }
        x: cross pa, b;
    endgroup
    cg c1 = new, c2 = new;
    initial begin
        a = 0; b = 0; c1.sample();
        a = 2; b = 3; c2.sample();
        a = 3; b = 0; c2.sample();
        pi = $realtobits(c1.pa.get_inst_coverage());
        pt = $realtobits(c1.pa.get_coverage());
        ps = $realtobits(cg::b::get_coverage());
        xi = $realtobits(c2.x.get_inst_coverage());
        xt = $realtobits(c1.x.get_coverage());
        xs = $realtobits(cg::x::get_coverage());
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(real(&mut sim, "pi"), 50.0);
        // `pa`: 50 in `c1`, 50 in `c2`; `b`: 50, 100.
        assert_eq!(real(&mut sim, "pt"), 50.0);
        assert_eq!(real(&mut sim, "ps"), 75.0);
        // `x`: 1 of 4 in `c1`, 2 of 4 in `c2`.
        assert_eq!(real(&mut sim, "xi"), 50.0);
        assert_eq!(real(&mut sim, "xt"), 37.5);
        assert_eq!(real(&mut sim, "xs"), 37.5);
    }

    /// 19.8: `stop()` and `start()` of the covergroup and of each item. A
    /// stopped covergroup takes no sample, and evaluates nothing of one; a
    /// stopped coverpoint counts none, nor do the crosses of it, and a
    /// stopped cross counts none. An item collects while it and the
    /// covergroup do.
    fn start_and_stop(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    class holder; bit [1:0] v; endclass
    bit [1:0] a, b;
    holder h;
    covergroup cg;
        ca: coverpoint a;
        cb: coverpoint b;
        x: cross ca, cb;
    endgroup
    covergroup of_object;
        coverpoint h.v;
    endgroup
    cg c = new;
    of_object d = new;
    initial begin
        a = 0; b = 0; c.sample();
        c.stop();
        a = 1; b = 1; c.sample();
        c.start(); c.ca.stop();
        a = 2; b = 2; c.sample();
        c.ca.start(); c.x.stop();
        a = 3; b = 3; c.sample();
        c.stop(); c.x.start(); c.x.stop(); c.start();
        a = 1; b = 1; c.sample();
        c.x.start();
        a = 0; b = 0; c.sample();
        d.stop();
        d.sample();
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
        assert_eq!(samples(&report, "Top.cg"), [5]);
        assert_eq!(
            bins(&report, "Top.cg", 0, "ca"),
            hits(&[("auto[0]", 2), ("auto[1]", 1), ("auto[2]", 0), ("auto[3]", 1)])
        );
        assert_eq!(
            bins(&report, "Top.cg", 0, "cb"),
            hits(&[("auto[0]", 2), ("auto[1]", 1), ("auto[2]", 1), ("auto[3]", 1)])
        );
        assert_eq!(counted(&report, "Top.cg", 0, "x"), (1, 16));
        assert_eq!(
            rows(&report, "Top.cg", 0, "x").0,
            counting(&[("<auto[0],auto[0]>", 2)])
        );
        assert!(group(&report, "Top.cg").instances[0].collecting);
        // A stopped covergroup did not read `h.v` of a `null` handle.
        let d = &group(&report, "Top.of_object").instances[0];
        assert_eq!(d.samples, 0);
        assert!(!d.collecting);
    }

    /// 19.8, Table 19-1: `set_inst_name` sets the name of the instance,
    /// which `option.name` reads and the report and the messages of the
    /// instance show; `option.name` sets it too.
    fn set_inst_name(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output int same);
    bit [1:0] a;
    covergroup cg;
        coverpoint a { illegal_bins bad = {3}; }
    endgroup
    cg c1 = new, c2 = new, c3 = new;
    string s;
    initial begin
        s = "first";
        c1.set_inst_name(s);
        c2.option.name = {"sec", "ond"};
        same = c1.option.name == "first" && c2.option.name == "second";
        a = 3; c1.sample();
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let same = sim.signal("same");
        assert_eq!(sim.get(same), BigUint::from(1u8));
        let report = sim.coverage();
        let names: Vec<_> = group(&report, "Top.cg")
            .instances
            .iter()
            .map(|instance| instance.name.as_str())
            .collect();
        assert_eq!(names, ["first", "second", "Top.cg#2"]);
        assert_eq!(
            reports(sim.drain_runtime_events()),
            [(
                celox::Severity::Error,
                "illegal_bins `bad` of coverpoint `a` of covergroup `Top.cg` (instance `first`): \
                 value 3"
                    .to_string()
            )]
        );
    }

    /// 19.7.1: `type_option` is a static member, which an instance only
    /// names, of the covergroup and of its items: through a handle that is
    /// `null` it is the member of the type.
    fn type_option_through_an_instance(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output int weight, same);
    bit [1:0] a;
    covergroup cg;
        cp: coverpoint a;
    endgroup
    cg h, never;
    initial begin
        never.type_option.weight = 4;
        never.cp.type_option.comment = "of cp";
        h = new;
        h.cp.type_option.goal = 30;
        weight = cg::type_option.weight;
        same = never.cp.type_option.comment == "of cp" && cg::cp::type_option.goal == 30;
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        for (name, value) in [("weight", 4u8), ("same", 1)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
        let report = sim.coverage();
        let cg = group(&report, "Top.cg");
        assert_eq!(cg.weight, 4);
        assert_eq!(
            (cg.items[0].comment.as_str(), cg.items[0].goal),
            ("of cp", 30)
        );
    }

    /// 19.7: "When set at the covergroup level, the weight, goal, comment,
    /// and per_instance options do not act as default values to the lower
    /// syntactic levels": a covergroup of weight 0 leaves the weights of its
    /// items at 1 and its own coverage as it is, and drops out of the
    /// coverage of the type (19.11.3).
    fn group_weight_is_not_an_item_default(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned i1, ty, output int weight);
    bit [1:0] a;
    covergroup cg;
        coverpoint a { bins lo = {[0:1]}; bins hi = {[2:3]}; }
    endgroup
    cg c1 = new, c2 = new;
    initial begin
        c1.option.weight = 0;
        a = 0; c1.sample(); c2.sample();
        a = 2; c1.sample();
        i1 = $realtobits(c1.get_inst_coverage());
        ty = $realtobits(cg::get_coverage());
        weight = c1.a.option.weight;
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(real(&mut sim, "i1"), 100.0);
        assert_eq!(real(&mut sim, "ty"), 50.0);
        let weight = sim.signal("weight");
        assert_eq!(sim.get(weight), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(item(&report, "Top.cg", 0, "a").weight, 1);
    }

    /// The options and methods of an embedded covergroup are those of the
    /// instance of each object (19.4): through the variable of the
    /// covergroup in a method of the class and through an object.
    fn embedded_covergroups_of_several_objects(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned r1, r2, output int c1, t1);
    class C;
        bit [1:0] v;
        covergroup cg(int al);
            cv: coverpoint v;
        endgroup
        function new(string name, int al);
            cg = new(al);
            cg.set_inst_name(name);
            cg.cv.option.at_least = al;
        endfunction
        function void record(bit [1:0] value); v = value; cg.sample(); endfunction
        function void pause(); cg.stop(); endfunction
        function real inst(output int covered, total);
            return cg.cv.get_inst_coverage(covered, total);
        endfunction
    endclass
    C o1, o2;
    initial begin
        o1 = new("one", 1);
        o2 = new("two", 2);
        o1.record(0); o2.record(0); o2.record(1); o2.record(1);
        o2.pause(); o2.record(2);
        o1.cg.option.comment = "of one";
        r1 = $realtobits(o1.inst(c1, t1));
        r2 = $realtobits(o2.cg.get_inst_coverage());
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        assert_eq!(real(&mut sim, "r1"), 25.0);
        // `o2` needs two hits: only `auto[1]` has them.
        assert_eq!(real(&mut sim, "r2"), 25.0);
        for (name, value) in [("c1", 1u8), ("t1", 4)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
        }
        let report = sim.coverage();
        let instances = &group(&report, "C::cg").instances;
        assert_eq!(
            instances
                .iter()
                .map(|instance| (instance.name.as_str(), instance.samples, instance.collecting))
                .collect::<Vec<_>>(),
            [("one", 1, true), ("two", 3, false)]
        );
        assert_eq!(instances[0].comment, "of one");
        assert_eq!(item(&report, "C::cg", 1, "cv").at_least, 2);
    }

    /// The options written after construction are part of a checkpoint:
    /// restoring one restores them with the samples.
    fn options_roll_back_with_a_checkpoint(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output int weight);
    bit a;
    covergroup cg;
        coverpoint a;
    endgroup
    cg c = new;
    initial begin
        c.option.weight = 2;
        c.a.option.at_least = 3;
        cg::type_option.comment = "before";
        #2;
        c.option.weight = 5;
        c.a.option.at_least = 1;
        cg::type_option.comment = "after";
        c.stop();
        #2 weight = c.option.weight;
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        sim.run_until(3).unwrap();
        let options = |report: &celox::CoverageReport| {
            let instance = &group(report, "Top.cg").instances[0];
            (
                instance.weight,
                instance.items[0].at_least,
                instance.collecting,
                group(report, "Top.cg").comment.clone(),
            )
        };
        assert_eq!(options(&sim.coverage()), (5, 1, false, "after".to_string()));
        sim.restore(&checkpoint).unwrap();
        assert_eq!(options(&sim.coverage()), (2, 3, true, "before".to_string()));
        sim.run_until(5).unwrap();
        let weight = sim.signal("weight");
        assert_eq!(sim.get(weight), BigUint::from(5u8));
    }

    /// Table 19-1: `option.detect_overlap` warns, when an instance is
    /// constructed, of two bins of a coverpoint whose values overlap.
    fn detect_overlap_warns_at_new(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [3:0] a, b;
    covergroup cg(int hi);
        option.detect_overlap = 1;
        ca: coverpoint a { bins low = {[0:hi]}; bins high = {[4:15]}; }
        cb: coverpoint b { option.detect_overlap = 0; bins x = {[0:8]}; bins y = {[4:15]}; }
    endgroup
    cg c1 = new(3), c2 = new(6);
    initial done = 1;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let warned = reports(sim.drain_runtime_events());
        assert_eq!(warned.len(), 1, "{warned:?}");
        assert_eq!(warned[0].0, celox::Severity::Warning);
        assert!(
            warned[0].1.contains("coverpoint `ca`")
                && warned[0].1.contains("bins `low` and `high` overlap"),
            "{warned:?}"
        );
    }

    /// Table 19-3: `type_option.distribute_first` distributes the values of
    /// a bin array among its bins before its `with` filter is applied.
    fn distribute_first_orders_the_filter(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit [2:0] a;
    covergroup after;
        coverpoint a { bins b[2] = {[0:7]} with (item < 3); }
    endgroup
    covergroup first;
        type_option.distribute_first = 1;
        coverpoint a { bins b[2] = {[0:7]} with (item < 3); }
    endgroup
    after c1 = new;
    first c2 = new;
    initial begin
        a = 1; c1.sample(); c2.sample();
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
        // The values 0, 1 and 2 in two bins: 0, then 1 and 2.
        assert_eq!(
            bins(&report, "Top.after", 0, "a"),
            hits(&[("b[0]", 0), ("b[1]", 1)])
        );
        // The values 0 to 3 and 4 to 7, then filtered: 0, 1 and 2, then
        // none.
        assert_eq!(bins(&report, "Top.first", 0, "a"), hits(&[("b[0]", 1)]));
        assert_eq!(counted(&report, "Top.first", 0, "a"), (1, 1));
    }
}

/// 19.7, 19.8: the options and methods of an instance read its handle, and
/// through `null` they are a run-time error; those of the type do not
/// (`get_coverage`, `type_option`).
#[test]
fn instance_options_and_methods_through_null() {
    for (statement, faults) in [
        ("n = never.get_inst_coverage(n, t) > 0;", true),
        ("never.option.weight = 2;", true),
        ("n = never.option.goal;", true),
        ("never.cp.option.at_least = 2;", true),
        ("never.start();", true),
        ("never.cp.stop();", true),
        ("never.set_inst_name(\"x\");", true),
        ("n = never.get_coverage(n, t) >= 0;", false),
        ("n = never.cp.get_coverage() >= 0;", false),
        ("never.type_option.goal = 2;", false),
        ("n = never.cp.type_option.weight;", false),
    ] {
        let source = format!(
            "`timescale 1ns/1ns
module Top(output int reached, done);
    bit [1:0] a;
    covergroup cg; cp: coverpoint a; endgroup
    cg never;
    int n, t;
    initial begin
        reached = 1;
        #1 {statement}
        #1 done = 1;
    end
endmodule
"
        );
        let mut sim = simulation(&source).four_state(false).build().unwrap();
        sim.run_until(0).unwrap();
        let result = sim.run_until(3);
        assert_eq!(result.is_err(), faults, "{statement}: {result:?}");
        if let Err(error) = result {
            assert!(error.to_string().contains("null"), "{statement}: {error}");
        } else {
            let done = sim.signal("done");
            assert_eq!(sim.get(done), BigUint::from(1u8), "{statement}");
        }
    }
}

/// Options and methods of a covergroup in the clocked procedure that
/// samples it, under the untimed `Simulator`: the weight of an item
/// assigned from an input, `stop()` and `start()` from the state of the
/// design. At every optimization level and in both state modes.
#[test]
fn options_and_methods_in_clocked_logic_under_simulator() {
    let source = r#"module Top(input bit clk, input logic [1:0] d, input logic [3:0] w,
           output bit [7:0] q, output int covered, output int at_least);
    covergroup cg;
        cd: coverpoint d;
    endgroup
    cg c;
    int total;
    always_ff @(posedge clk) q <= q + 8'd1;
    always @(posedge clk) begin
        if (c == null) c = new;
        c.cd.option.weight = w;
        if (q == 8'd2) c.stop();
        if (q == 8'd4) c.start();
        c.sample();
        void'(c.cd.get_inst_coverage(covered, total));
        at_least = c.cd.option.at_least + c.cd.option.weight;
    end
endmodule
"#;
    for level in [
        celox::OptLevel::O0,
        celox::OptLevel::O1,
        celox::OptLevel::O2,
    ] {
        for four_state in [false, true] {
            let mut sim =
                Simulator::from_sv_sources(vec![(source, Path::new("covergroup_ff.sv"))], "Top")
                    .four_state(four_state)
                    .opt_level(level)
                    .build()
                    .unwrap();
            let clk = sim.event("clk");
            let d = sim.signal("d");
            let w = sim.signal("w");
            // `q` is the number of edges before the one that samples: the
            // third and fourth are not counted.
            for value in [0u8, 1, 2, 3, 3, 1] {
                sim.modify(|io| {
                    io.set(d, value);
                    // The top bit set: the weight is 9, the value of the
                    // unsigned input.
                    io.set(w, 9u8);
                })
                .unwrap();
                sim.tick(clk).unwrap();
            }
            let context = format!("{level:?} four_state={four_state}");
            let report = sim.coverage();
            assert_eq!(samples(&report, "Top.cg"), [4], "{context}");
            assert_eq!(
                bins(&report, "Top.cg", 0, "cd"),
                hits(&[
                    ("auto[0]", 1),
                    ("auto[1]", 2),
                    ("auto[2]", 0),
                    ("auto[3]", 1)
                ]),
                "{context}"
            );
            assert_eq!(item(&report, "Top.cg", 0, "cd").weight, 9, "{context}");
            let covered = sim.signal("covered");
            assert_eq!(sim.get(covered), BigUint::from(3u8), "{context}");
            let at_least = sim.signal("at_least");
            assert_eq!(sim.get(at_least), BigUint::from(10u8), "{context}");
        }
    }
}

/// Table 19-1, Table 19-3: a weight "shall be a non-negative integral
/// value": a procedure that assigns a negative one ends the simulation
/// with an error.
#[test]
fn negative_weight_assigned_in_a_procedure_ends_the_simulation() {
    for statement in [
        "c.option.weight = -1;",
        "c.cp.option.weight = -2;",
        "cg::type_option.weight = -3;",
        "cg::cp::type_option.weight = -4;",
    ] {
        let source = format!(
            "`timescale 1ns/1ns
module Top(output int done);
    bit [1:0] a;
    covergroup cg; cp: coverpoint a; endgroup
    cg c = new;
    initial begin
        #1 {statement}
        #1 done = 1;
    end
endmodule
"
        );
        let mut sim = simulation(&source).four_state(false).build().unwrap();
        let error = sim.run_until(3).unwrap_err().to_string();
        assert!(error.contains("negative weight"), "{statement}: {error}");
    }
}

sv_backends! {
    /// 19.10: an `int` or `bit` member takes the value assigned to it as a
    /// variable of its type does: an unsigned value narrower than 32 bits is
    /// extended with zeros, a signed one by its sign, a wider one is cut, x
    /// and z bits are 0 and a `real` is rounded, ties away from zero
    /// (6.12.1, 10.7). For `option` and `type_option`, of the covergroup and
    /// of a coverpoint.
    fn option_values_take_the_type_of_the_member(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned cov,
           output int g0, g1, g2, g3, g4, g5, g6, g7, g8, g9, g10, g11, g12, g13);
    bit [1:0] a;
    bit [3:0] b = 9;
    bit [1:0] two = 2;
    bit [39:0] wide = 40'h12_0000_0005;
    byte neg = -3;
    real r = 2.6;
    covergroup cg;
        cp: coverpoint a { bins lo = {[0:1]}; bins hi = {[2:3]}; }
    endgroup
    cg c = new;
    initial begin
        c.option.goal = b; g0 = c.option.goal;
        c.option.goal = 8'hFF; g1 = c.option.goal;
        c.option.goal = 1'b1; g2 = c.option.goal;
        c.option.goal = wide; g3 = c.option.goal;
        c.option.goal = neg; g4 = c.option.goal;
        c.option.goal = r; g5 = c.option.goal;
        c.option.goal = 2.5; g6 = c.option.goal;
        c.option.goal = -0.4; g7 = c.option.goal;
        c.cp.option.weight = 4'd9; g8 = c.cp.option.weight;
        cg::type_option.goal = 8'hFF; g9 = cg::type_option.goal;
        cg::cp::type_option.weight = r; g10 = cg::cp::type_option.weight;
        c.type_option.goal = -2.5; g11 = c.type_option.goal;
        c.type_option.merge_instances = 2'b10; g12 = c.type_option.merge_instances;
        c.type_option.merge_instances = 0.6; g13 = cg::type_option.merge_instances;
        c.cp.option.at_least = two;
        a = 0; c.sample();
        a = 2; c.sample();
        cov = $realtobits(c.get_inst_coverage());
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let expected: [i32; 14] = [9, 255, 1, 5, -3, 3, 3, 0, 9, 255, 3, -3, 0, 1];
        for (index, value) in expected.into_iter().enumerate() {
            let signal = sim.signal(&format!("g{index}"));
            assert_eq!(sim.get(signal), BigUint::from(value as u32), "g{index}");
        }
        // Two hits needed and one in each bin: nothing is covered.
        assert_eq!(real(&mut sim, "cov"), 0.0);
        let report = sim.coverage();
        let instance = &group(&report, "Top.cg").instances[0];
        assert_eq!(instance.goal, 0);
        assert_eq!(
            (instance.items[0].weight, instance.items[0].at_least),
            (9, 2)
        );
        let cg = group(&report, "Top.cg");
        assert_eq!((cg.goal, cg.items[0].weight), (-3, 3));
        assert!(cg.merge_instances);
    }

    /// 19.7.1: a `type_option` of the definition is a constant expression,
    /// a `real` one among them, which the member takes rounded.
    fn real_type_options_of_the_definition(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    bit a;
    covergroup cg;
        type_option.goal = 2.5;
        cp: coverpoint a { bins b = {0}; type_option.weight = 1.5; }
    endgroup
    cg c = new;
    initial done = 1;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(1).unwrap();
        let report = sim.coverage();
        let cg = group(&report, "Top.cg");
        assert_eq!((cg.goal, cg.items[0].weight), (3, 2));
    }
}

/// An `int` member assigned a value with x bits takes 0 for them, as an
/// `int` variable does (6.11.2).
#[test]
fn option_values_with_unknown_bits_take_zero() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done, output int goal, output int plain);
    bit a;
    logic [3:0] lx = 4'b1x01;
    covergroup cg; coverpoint a; endgroup
    cg c = new;
    int i;
    initial begin
        c.option.goal = lx;
        goal = c.option.goal;
        i = lx;
        plain = i;
        done = 1;
    end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim = simulation(source).four_state(four_state).build().unwrap();
        sim.run_until(1).unwrap();
        let goal = sim.signal("goal");
        let plain = sim.signal("plain");
        assert_eq!(sim.get(goal), BigUint::from(9u8), "four_state={four_state}");
        assert_eq!(sim.get(goal), sim.get(plain), "four_state={four_state}");
    }
}

/// `$bits` of a member of an option structure, through the type as through
/// an instance, is the width of its type (20.6.2, 19.10); the operand is not
/// evaluated.
#[test]
fn bits_of_option_members() {
    let source = r#"`timescale 1ns/1ns
module Top(output int done, output int bits_type, bits_instance, bits_bit);
    bit a;
    covergroup cg; cp: coverpoint a; endgroup
    cg never;
    initial begin
        bits_type = $bits(cg::type_option.weight);
        bits_instance = $bits(never.cp.option.at_least);
        bits_bit = $bits(cg::type_option.merge_instances);
        done = 1;
    end
endmodule
"#;
    let mut sim = simulation(source).four_state(false).build().unwrap();
    sim.run_until(1).unwrap();
    for (name, value) in [("bits_type", 32u8), ("bits_instance", 32), ("bits_bit", 1)] {
        let signal = sim.signal(name);
        assert_eq!(sim.get(signal), BigUint::from(value), "{name}");
    }
}

/// The design of the tests of clocking events: `clk` toggles every 5 time
/// units from 0, so its rising edges are at 5, 15, 25 and 35 and its falling
/// edges at 10, 20, 30 and 40 up to time 42; a flip-flop counts the rising
/// edges into `q`, and `en` toggles with a nonblocking assignment at each of
/// them. `items` are among the module's items.
fn clocked(items: &str) -> String {
    format!(
        r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    bit en = 0;
    bit [2:0] q = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) q <= q + 3'd1;
    always @(posedge clk) en <= ~en;
{items}
endmodule
"#
    )
}

/// The value bins `b[0]` to `b[7]` of a coverpoint of `q` that were hit.
fn q_bins(values: &[(u64, u64)]) -> Vec<(String, u64)> {
    values
        .iter()
        .map(|(value, hits)| (format!("b[{value}]"), *hits))
        .collect()
}

sv_backends! {
    /// 19.3: an instance that a declaration constructs is sampled at each
    /// occurrence of the clocking event of its type, `posedge`, `negedge`,
    /// `edge`, any change, `iff` (9.4.2.3) and `or` or comma lists
    /// (9.4.2.1), and at no other time: a value that a nonblocking
    /// assignment of the same edge updates is sampled before the update,
    /// and the edges of an `or` list in different delta cycles of one time
    /// step are two samples.
    fn static_instances_sample_at_their_clocking_events(sim) {
        @setup {
            let source = clocked(
                "    bit s = 0;
    initial #7 s = 1;
    covergroup pos @(posedge clk); coverpoint q { bins b[] = {[0:7]}; } endgroup
    covergroup neg @(negedge clk); coverpoint q { bins b[] = {[0:7]}; } endgroup
    covergroup both @(edge clk); endgroup
    covergroup change @(clk); endgroup
    covergroup level @(s); endgroup
    covergroup gated @(posedge clk iff en); coverpoint q { bins b[] = {[0:7]}; } endgroup
    covergroup listed @(posedge clk or negedge clk); endgroup
    covergroup comma @(posedge clk, posedge en); endgroup
    pos p = new;
    neg n = new;
    both b = new;
    change a = new;
    level l = new;
    gated g = new;
    listed o = new;
    comma m = new;",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        assert_eq!(
            all_samples(&report),
            expected(&[
                ("Top.both", &[8]),
                ("Top.change", &[8]),
                ("Top.comma", &[6]),
                ("Top.gated", &[2]),
                ("Top.level", &[1]),
                ("Top.listed", &[8]),
                ("Top.neg", &[4]),
                ("Top.pos", &[4]),
            ])
        );
        let point = "q";
        assert_eq!(
            hit(bins(&report, "Top.pos", 0, point)),
            q_bins(&[(0, 1), (1, 1), (2, 1), (3, 1)])
        );
        assert_eq!(
            hit(bins(&report, "Top.neg", 0, point)),
            q_bins(&[(1, 1), (2, 1), (3, 1), (4, 1)])
        );
        assert_eq!(
            hit(bins(&report, "Top.gated", 0, point)),
            q_bins(&[(1, 1), (3, 1)])
        );
    }
}

/// The same in both state modes, at every optimization level and with each
/// dead-store policy: no sample before the first event, `@(s)` in two-state
/// mode included, which an `always @(s)` would run once at time zero.
#[test]
fn static_instances_sample_alike_in_every_mode() {
    let source = clocked(
        "    logic s = 0;
    initial #7 s = 1;
    covergroup pos @(posedge clk); coverpoint q { bins b[] = {[0:7]}; } endgroup
    covergroup level @(s); endgroup
    covergroup change @(clk); endgroup
    pos p = new;
    level l = new;
    change a = new;",
    );
    for four_state in [false, true] {
        for level in [
            celox::OptLevel::O0,
            celox::OptLevel::O1,
            celox::OptLevel::O2,
        ] {
            for policy in [
                celox::DeadStorePolicy::Off,
                celox::DeadStorePolicy::PreserveTopPorts,
                celox::DeadStorePolicy::PreserveAllPorts,
            ] {
                let context = format!("four_state={four_state} {level:?} {policy:?}");
                let mut sim = simulation(&source)
                    .four_state(four_state)
                    .opt_level(level)
                    .dead_store_policy(policy)
                    .build()
                    .unwrap_or_else(|error| panic!("{context}: {error}"));
                sim.run_until(6).unwrap();
                assert_eq!(
                    all_samples(&sim.coverage()),
                    expected(&[("Top.change", &[1]), ("Top.level", &[0]), ("Top.pos", &[1])]),
                    "{context}"
                );
                sim.run_until(42).unwrap();
                let report = sim.coverage();
                assert_eq!(
                    all_samples(&report),
                    expected(&[("Top.change", &[8]), ("Top.level", &[1]), ("Top.pos", &[4])]),
                    "{context}"
                );
                assert_eq!(
                    hit(bins(&report, "Top.pos", 0, "q")),
                    q_bins(&[(0, 1), (1, 1), (2, 1), (3, 1)]),
                    "{context}"
                );
            }
        }
    }
}

sv_backends! {
    /// A named event (15.5) and an `or` of two, as the clocking event: each
    /// trigger is a sample.
    fn static_instances_sample_at_named_events(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top;
    event e, f;
    covergroup one @e; endgroup
    covergroup two @(e or f); endgroup
    one o = new;
    two t = new;
    initial begin
        #1 -> e;
        #1 -> f;
        #1 -> e;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(5).unwrap();
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[("Top.one", &[2]), ("Top.two", &[3])])
        );
    }
}

sv_backends! {
    /// A clock the host drives on an input sampled by an instance alone, with
    /// no flip-flop on it: each rising edge is a sample.
    fn static_instances_sample_a_host_clock_without_flip_flops(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(input bit clk, input bit [3:0] d);
    covergroup cg @(posedge clk); coverpoint d { bins b[] = {[0:3]}; } endgroup
    cg c = new;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        let d = sim.signal("d");
        sim.modify(|io| io.set(d, 2u8)).unwrap();
        // Rising edges at 5, 15 and 25.
        sim.add_clock("clk", 10, 5);
        sim.run_until(32).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [3]);
        assert_eq!(hit(bins(&report, "Top.cg", 0, "d")), hits(&[("b[2]", 3)]));
    }
}

sv_backends! {
    /// What a covergroup instance does procedurally applies to one sampled at
    /// its clocking event: `sample()` samples it once more (19.8: the
    /// methods "can be invoked procedurally at any time"), and while it is
    /// stopped the events give no sample (19.8, `stop()`/`start()`); its
    /// options are those of any instance.
    fn static_instances_take_procedural_methods_and_options(sim) {
        @setup {
            let source = clocked(
                "    covergroup cg @(posedge clk);
        option.at_least = 2;
        coverpoint q { bins lo = {[0:1]}; bins hi = {[2:7]}; }
    endgroup
    cg c = new;
    initial begin
        #12 c.stop();
        #10 c.start();
        #1 c.sample();
    end",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        // Sampled at 5 (q = 0), 23 (q = 2, procedurally), 25 (q = 2) and 35
        // (q = 3), not at 15 (stopped).
        assert_eq!(samples(&report, "Top.cg"), [4]);
        assert_eq!(bins(&report, "Top.cg", 0, "q"), hits(&[("lo", 1), ("hi", 3)]));
        // Only `hi` reaches `at_least`.
        assert!(close(group(&report, "Top.cg").instances[0].coverage, 50.0));
    }
}

sv_backends! {
    /// The sampler of a declaration samples the instance the declaration
    /// constructs: assigning the variable later neither stops the sampling
    /// of that instance nor samples another one twice.
    fn static_sites_follow_the_instance_not_the_variable(sim) {
        @setup {
            let source = clocked(
                "    covergroup cg @(posedge clk); endgroup
    cg c = new, d = new;
    cg e = new;
    initial begin
        #12 c = d;
        e = null;
    end",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [4, 4, 4]);
    }
}

sv_backends! {
    /// Declarations in generate blocks, a loop's included, each sample their
    /// instance; the event is bound where the covergroup is declared, so a
    /// block that declares a signal of the same name as the clock does not
    /// change what the instance samples at. Two instances of a module each
    /// sample theirs at their own clock, and an event may name the net of a
    /// child instance.
    fn static_sites_per_generate_block_and_instance(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Child(input bit clk, output bit slow);
    covergroup cg @(posedge clk); endgroup
    cg c = new;
    bit half = 0;
    always @(posedge clk) half <= ~half;
    assign slow = half;
endmodule
module Top;
    bit clk = 0, clk2 = 0;
    always #5 clk = ~clk;
    always #10 clk2 = ~clk2;
    covergroup cg @(posedge clk); endgroup
    for (genvar i = 0; i < 3; i++) begin : g
        cg c = new;
    end
    if (1) begin : h
        bit clk = 0;
        cg c = new;
        covergroup hc @(negedge clk); endgroup
        hc x = new;
        initial #12 clk = 1;
        initial #17 clk = 0;
    end
    Child u1(.clk(clk), .slow());
    Child u2(.clk(clk2), .slow());
    covergroup child @(posedge u1.slow); endgroup
    child k = new;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(42).unwrap();
        // `clk`: rising at 5, 15, 25, 35; `clk2`: at 10, 30; `u1.slow`: at 5
        // and 25; `h.clk`: falling at 17.
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[
                ("Child.cg", &[4, 2]),
                ("Top.cg", &[4, 4, 4, 4]),
                ("Top.child", &[2]),
                ("Top.h.hc", &[1]),
            ])
        );
    }
}

sv_backends! {
    /// IEEE 1800-2023 19.3 samples "the instant the clocking event takes
    /// place, as if the process triggering the event were to call the
    /// built-in sample() method". The sampler of a declaration is a process
    /// that waits for the event, which differs in three ways, pinned here:
    /// (1) a process that makes the event occur several times without
    /// suspending gives one sample, (2) the sample sees a blocking write that
    /// the triggering process makes after the triggering write, and (3) an
    /// event before the sampler first waits, at time zero by a process that
    /// runs before it, is missed. Occurrences separated by `#0` are samples
    /// each.
    fn clocking_events_are_waited_for_by_a_process(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top;
    bit early = 0, s = 0, t = 0, clk = 0;
    bit [3:0] d = 0;
    event e;
    initial begin
        early = 1;
        #1 early = 0;
        #1 early = 1;
    end
    covergroup first @(early); endgroup
    covergroup first_edge @(posedge early); endgroup
    covergroup twice @(posedge s); endgroup
    covergroup apart @(posedge t); endgroup
    covergroup triggered @e; endgroup
    covergroup later @(posedge clk); coverpoint d { bins b[] = {[0:7]}; } endgroup
    first f = new;
    first_edge fe = new;
    twice w = new;
    apart a = new;
    triggered g = new;
    later l = new;
    initial begin
        #5 s = 1; s = 0; s = 1;
        #1 t = 1; #0 t = 0; #0 t = 1;
        #1 -> e; -> e;
        #1 d = 1; clk = 1; d = 5;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(10).unwrap();
        let report = sim.coverage();
        assert_eq!(
            all_samples(&report),
            expected(&[
                ("Top.apart", &[2]),
                ("Top.first", &[2]),
                ("Top.first_edge", &[2]),
                ("Top.later", &[1]),
                ("Top.triggered", &[1]),
                ("Top.twice", &[1]),
            ])
        );
        assert_eq!(hit(bins(&report, "Top.later", 0, "d")), hits(&[("b[5]", 1)]));
    }
}

sv_backends! {
    /// A program samples in the Reactive region set (IEEE 1800-2023 24.3.1):
    /// the instance a declaration of a program constructs sees what the
    /// nonblocking assignments of the design made at the edge.
    fn program_sites_sample_in_the_reactive_region(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
program prog(input bit clk, input bit [2:0] q);
    covergroup pc @(posedge clk); coverpoint q { bins b[] = {[0:7]}; } endgroup
    pc c = new;
endprogram
module Top;
    bit clk = 0;
    bit [2:0] q = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) q <= q + 3'd1;
    prog p(.clk(clk), .q(q));
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "prog.pc"), [4]);
        assert_eq!(
            hit(bins(&report, "prog.pc", 0, "q")),
            q_bins(&[(1, 1), (2, 1), (3, 1), (4, 1)])
        );
    }
}

/// The untimed `Simulator` runs no process, so an instance sampled at its
/// clocking event needs a `Simulation`.
#[test]
fn static_instances_require_simulation() {
    let source = "`timescale 1ns/1ns\nmodule Top(input bit clk, input bit [3:0] d);\n    covergroup cg @(posedge clk); coverpoint d; endgroup\n    cg c = new;\nendmodule\n";
    let sources = || vec![(source, Path::new("covergroup_clocked.sv"))];
    celox::Simulation::from_sv_sources(sources(), "Top")
        .build()
        .unwrap();
    let error = match Simulator::from_sv_sources(sources(), "Top").build() {
        Ok(_) => panic!("a clocking event needs a Simulation"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error.kind(),
            celox::SimulatorErrorKind::ProcessRequiresSimulation(_)
        ),
        "{error:?}"
    );
}

sv_backends! {
    /// The clocking event may be an edge of a member of the interface
    /// instance that a virtual interface variable of the module denotes
    /// (25.9), followed as an event control on it is.
    fn static_instance_virtual_interface_clock(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
interface ifc(input bit clk);
endinterface
module Top;
    bit clk = 0, other = 0;
    always #5 clk = ~clk;
    always #2 other = ~other;
    ifc i(.clk(clk));
    ifc j(.clk(other));
    virtual ifc vif = i;
    covergroup cg @(posedge vif.clk); endgroup
    cg c = new;
    initial #20 vif = j;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(19).unwrap();
        // `clk` rises at 5 and 15.
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [2]);
        // `other` rises at 22, 26 and 30.
        sim.run_until(31).unwrap();
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [5]);
    }
}

sv_backends! {
    /// The third difference from 19.3 from the host: a rising edge of an
    /// input that the host makes before the first `run_until`, before the
    /// sampler first waits, is missed; the later ones are samples.
    fn host_events_before_the_sampler_waits_are_missed(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(input bit go);
    covergroup cg @(go); endgroup
    cg c = new;
    covergroup rising @(posedge go); endgroup
    rising r = new;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        let go = sim.signal("go");
        sim.modify(|io| io.set(go, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        // The change is missed by the process; the edge sampler, which runs
        // with the flip-flops of the edge, samples it.
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[("Top.cg", &[0]), ("Top.rising", &[1])])
        );
        for (time, level) in [(2, 0u8), (3, 1), (4, 0), (5, 1)] {
            sim.modify(|io| io.set(go, level)).unwrap();
            sim.run_until(time).unwrap();
        }
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[("Top.cg", &[4]), ("Top.rising", &[3])])
        );
    }
}

/// The design of the tests of clocks that nonblocking assignments update,
/// up to time 82: `clk` toggles every 5 time units from 0 (rising at 5, 15,
/// ..., 75), `div` is `clk` divided by two by a flip-flop (rising at 5, 25,
/// 45, 65, falling at 15, 35, 55, 75), `div2` is `div` divided by two
/// (rising at 5 and 45), `nclk` toggles by a nonblocking assignment every 5
/// time units, `gclk` is `clk` gated by `en_g` from time 12 (rising at 15,
/// 25, ..., 75), and `slow` is the output of a child instance that divides
/// `clk` by two with a flip-flop. Each counter counts with a nonblocking
/// assignment the events of the covergroup that samples it, so a sample at
/// the k-th event, taken before the updates of the event, sees k - 1.
fn nba_clocks(logic: &str, extra: &str) -> String {
    format!(
        r#"`timescale 1ns/1ns
module Half(input {logic} clk, output {logic} slow);
    {logic} half = 0;
    always_ff @(posedge clk) half <= ~half;
    assign slow = half;
endmodule
interface ifc(input {logic} clk);
endinterface
module Top;
    {logic} clk = 0, div = 0, div2 = 0, nclk = 0, en_g = 0, slow;
    {logic} gclk;
    assign gclk = clk & en_g;
    bit [3:0] r = 0, r2 = 0, q = 0, g = 0, h = 0, rn = 0, cn = 0;
    always #5 clk = ~clk;
    always #5 nclk <= ~nclk;
    initial #12 en_g = 1;
    always_ff @(posedge clk) div <= ~div;
    always_ff @(posedge div) div2 <= ~div2;
    Half u(.clk(clk), .slow(slow));
    ifc i(.clk(div));
    virtual ifc vif = i;
    always_ff @(posedge div) r <= r + 4'd1;
    always_ff @(posedge div2) r2 <= r2 + 4'd1;
    always_ff @(posedge nclk) q <= q + 4'd1;
    always_ff @(posedge gclk) g <= g + 4'd1;
    always_ff @(posedge slow) h <= h + 4'd1;
    always_ff @(negedge div) rn <= rn + 4'd1;
    always_ff @(negedge clk) cn <= cn + 4'd1;
    covergroup on_div @(posedge div); coverpoint r {{ bins b[] = {{[0:15]}}; }} endgroup
    covergroup on_div2 @(posedge div2); coverpoint r2 {{ bins b[] = {{[0:15]}}; }} endgroup
    covergroup on_nclk @(posedge nclk); coverpoint q {{ bins b[] = {{[0:15]}}; }} endgroup
    covergroup on_gclk @(posedge gclk); coverpoint g {{ bins b[] = {{[0:15]}}; }} endgroup
    covergroup on_child @(posedge u.slow); coverpoint h {{ bins b[] = {{[0:15]}}; }} endgroup
    covergroup on_fall @(negedge div); coverpoint rn {{ bins b[] = {{[0:15]}}; }} endgroup
    covergroup on_edge @(edge div); e: coverpoint r + rn {{ bins b[] = {{[0:15]}}; }} endgroup
    covergroup on_list @(posedge div or negedge clk); l: coverpoint r + cn {{ bins b[] = {{[0:15]}}; }} endgroup
    covergroup on_iff @(posedge div iff (r[0] == 0)); coverpoint r {{ bins b[] = {{[0:15]}}; }} endgroup
    covergroup on_many @(posedge div); coverpoint r {{ bins b[] = {{[0:15]}}; }} endgroup
    on_div c_div = new;
    on_div2 c_div2 = new;
    on_nclk c_nclk = new;
    on_gclk c_gclk = new;
    on_child c_child = new;
    on_fall c_fall = new;
    on_edge c_edge = new;
    on_list c_list = new;
    on_iff c_iff = new;
    on_many c_many = new, d_many = new;
    for (genvar k = 0; k < 2; k++) begin : gen
        on_many c = new;
    end
{extra}
endmodule
"#
    )
}

/// The bins `b[...]` that samples of `values`, in increasing order, hit,
/// each with its hits.
fn once(values: &[u64]) -> Vec<(String, u64)> {
    let mut hits: Vec<(String, u64)> = Vec::new();
    for value in values {
        let name = format!("b[{value}]");
        match hits.last_mut() {
            Some((last, count)) if *last == name => *count += 1,
            _ => hits.push((name, 1)),
        }
    }
    hits
}

/// The samples of the designs of [`nba_clocks`]: each edge sampler sees what
/// the flip-flops of its edge assigned before they assigned it.
/// In four-state simulation `div`, which has both edge procedures and the
/// declaration initializer 0, also falls at time zero, as every procedure of
/// the falling edge sees.
fn check_nba_clocks(report: &celox::CoverageReport, four_state: bool) {
    let falls: &[u64] = if four_state {
        &[0, 1, 2, 3, 4]
    } else {
        &[0, 1, 2, 3]
    };
    for (name, point, values) in [
        ("Top.on_div", "r", &[0, 1, 2, 3][..]),
        ("Top.on_div2", "r2", &[0, 1]),
        ("Top.on_nclk", "q", &[0, 1, 2, 3, 4, 5, 6, 7]),
        ("Top.on_gclk", "g", &[0, 1, 2, 3, 4, 5, 6]),
        ("Top.on_child", "h", &[0, 1, 2, 3]),
        ("Top.on_fall", "rn", falls),
        ("Top.on_iff", "r", &[0, 2]),
    ] {
        assert_eq!(samples(report, name), [values.len() as u64], "{name}");
        assert_eq!(hit(bins(report, name, 0, point)), once(values), "{name}");
    }
    // Several declarations of one type, also in a generate loop.
    assert_eq!(samples(report, "Top.on_many"), [4; 4]);
    for instance in 0..4 {
        assert_eq!(
            hit(bins(report, "Top.on_many", instance, "r")),
            once(&[0, 1, 2, 3]),
            "{instance}"
        );
    }
}

sv_backends! {
    /// 19.3: "as if the process triggering the event were to call the
    /// built-in sample() method" — at a clock that a flip-flop divides
    /// (once and twice), one a nonblocking assignment toggles, a gated
    /// clock and the output of a child instance, with `negedge`, an edge
    /// with an `iff` and several declarations of one type, each sample sees
    /// the values before the nonblocking assignments of its edge, as a
    /// flip-flop of the edge does.
    fn edges_of_nba_updated_clocks_sample_before_the_updates(sim) {
        @setup { let source = nba_clocks("bit", ""); }
        @build simulation(&source).four_state(false);
        sim.run_until(82).unwrap();
        check_nba_clocks(&sim.coverage(), false);
    }
}

sv_backends! {
    /// The same with four-state clocks.
    fn edges_of_nba_updated_four_state_clocks_sample_before_the_updates(sim) {
        @setup { let source = nba_clocks("logic", ""); }
        @build simulation(&source).four_state(true);
        sim.run_until(82).unwrap();
        check_nba_clocks(&sim.coverage(), true);
    }
}

sv_backends! {
    /// A change event (`@(div)`), `edge`, an `or` list of edges, a named
    /// event triggered at an edge and a clock read through a virtual
    /// interface are waited for by a process,
    /// which runs after the nonblocking assignments of the edge that makes
    /// the event occur: the sample sees their values (a difference from
    /// 19.3, documented). Here `div` is the divided clock of
    /// [`nba_clocks`], and `r + rn` and `r` count its changes and rising
    /// edges.
    fn waited_events_of_nba_updated_clocks_sample_after_the_updates(sim) {
        @setup {
            let source = nba_clocks(
                "bit",
                "    event e;
    always @(posedge div) -> e;
    covergroup on_change @(div); e: coverpoint r + rn { bins b[] = {[0:15]}; } endgroup
    covergroup on_named @e; coverpoint r { bins b[] = {[0:15]}; } endgroup
    covergroup on_vif @(posedge vif.clk); coverpoint r { bins b[] = {[0:15]}; } endgroup
    on_change c_change = new;
    on_named c_named = new;
    on_vif c_vif = new;",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(82).unwrap();
        let report = sim.coverage();
        check_nba_clocks(&report, false);
        for (name, point, values) in [
            ("Top.on_change", "e", &[1, 2, 3, 4, 5, 6, 7, 8][..]),
            ("Top.on_edge", "e", &[1, 2, 3, 4, 5, 6, 7, 8]),
            // `posedge div` sees the update of `r`, `negedge clk`, a
            // blocking clock, not that of `cn`: 1 at 5 and 10, 2 at 20, 4 at
            // 25 and 30, and so on.
            ("Top.on_list", "l", &[1, 1, 2, 4, 4, 5, 7, 7, 8, 10, 10, 11]),
            ("Top.on_named", "r", &[1, 2, 3, 4]),
            ("Top.on_vif", "r", &[1, 2, 3, 4]),
        ] {
            assert_eq!(samples(&report, name), [values.len() as u64], "{name}");
            assert_eq!(hit(bins(&report, name, 0, point)), once(values), "{name}");
        }
    }
}

sv_backends! {
    /// Occurrences of an event that a process waits for, before the process
    /// runs again, give one sample, from one process or from several: here
    /// two processes that raise the two terms of `@(posedge a or posedge
    /// b)`, or of `@(a or b)`, in the same time step, before the sampler,
    /// which is created after them, runs; 19.3 counts two. An edge sampler
    /// counts each edge of a process that makes several without suspending
    /// once too (`s = 1; s = 0; s = 1;`), and the edges of different delta
    /// cycles each.
    fn occurrences_before_the_sampler_runs_again_are_one_sample(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top;
    bit a = 0, b = 0, s = 0, t = 0;
    always #5 a = ~a;
    always #5 b = ~b;
    covergroup edges @(posedge a or posedge b); endgroup
    covergroup changes @(a or b); endgroup
    covergroup twice @(posedge s); endgroup
    covergroup apart @(posedge t); endgroup
    edges e = new;
    changes c = new;
    twice w = new;
    apart p = new;
    initial begin
        #2 s = 1; s = 0; s = 1;
        #1 t = 1; #0 t = 0; #0 t = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(42).unwrap();
        // `a` and `b` rise together at 5, 15, 25 and 35 and change together
        // at every multiple of 5.
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[
                ("Top.apart", &[2]),
                ("Top.changes", &[8]),
                ("Top.edges", &[4]),
                ("Top.twice", &[1]),
            ])
        );
    }
}

/// At time zero in four-state simulation: a declaration initializer
/// (`logic d = 1;`) is no event (10.5); assignments of an `initial`
/// procedure that make x 1 or 0 are edges (Table 9-2), which an edge
/// sampler samples as the flip-flops of the edge run then, and which a
/// sampler process misses when the procedure runs before it waits (it is
/// declared first here). The later edges are samples of both.
#[test]
fn four_state_time_zero_edges() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic d = 1;
    logic a, b, c;
    initial begin
        a = 1; b = 0; c = 0;
        #1 a = 0; b = 1; c = 1; d = 0;
        #1 a = 1; b = 0; c = 0; d = 1;
    end
    covergroup declared @(posedge d); endgroup
    covergroup rising @(posedge a); endgroup
    covergroup falling @(negedge b); endgroup
    covergroup change @(c); endgroup
    declared w = new;
    rising r = new;
    falling f = new;
    change h = new;
endmodule
"#;
    let mut sim = simulation(source).four_state(true).build().unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(
        all_samples(&sim.coverage()),
        expected(&[
            ("Top.change", &[0]),
            ("Top.declared", &[0]),
            ("Top.falling", &[1]),
            ("Top.rising", &[1]),
        ])
    );
    sim.run_until(5).unwrap();
    assert_eq!(
        all_samples(&sim.coverage()),
        expected(&[
            ("Top.change", &[2]),
            ("Top.declared", &[1]),
            ("Top.falling", &[2]),
            ("Top.rising", &[2]),
        ])
    );
}

sv_backends! {
    /// An edge sampler beside an edge-sensitive procedure of the design that
    /// calls `sample()` of the same covergroup type runs as a process, as
    /// two such procedures the design writes do, and so sees the
    /// nonblocking assignments of an edge that the NBA region updates (a
    /// difference from 19.3, documented): at the rising edges of the
    /// divided clock `div`, at 5 and 25, the site samples `r` as 1 and 2,
    /// not 0 and 1.
    fn edge_samplers_beside_a_written_sample_of_the_type_run_as_processes(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0, div = 0;
    bit [3:0] r = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) div <= ~div;
    always_ff @(posedge div) r <= r + 4'd1;
    covergroup cg @(posedge div); coverpoint r { bins b[] = {[0:15]}; } endgroup
    cg c = new, e = new;
    always @(negedge clk) e.sample();
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [2, 6]);
        assert_eq!(hit(bins(&report, "Top.cg", 0, "r")), once(&[1, 2]));
    }
}

sv_backends! {
    /// 19.3: an instance that any other `new` constructs is sampled at the
    /// clocking event of its type too: in an `initial` procedure, an
    /// element of an array in a `foreach`, a task, a function and an
    /// `always` procedure (from the instant it constructs one: here at 12).
    /// A value that a nonblocking assignment of the edge updates is sampled
    /// before the update, at a clock that blocking assignments make, and a
    /// declaration of the type keeps its own sampler beside them.
    fn constructed_instances_sample_at_their_clocking_events(sim) {
        @setup {
            let source = clocked(
                "    covergroup cg @(posedge clk); coverpoint q { bins b[] = {[0:7]}; } endgroup
    covergroup fall @(negedge clk); coverpoint q { bins b[] = {[0:7]}; } endgroup
    cg s = new;
    cg c, d, e, f;
    cg a[2];
    fall n;
    bit go = 0;
    task automatic make(); d = new; endtask
    function automatic void build(); f = new; endfunction
    initial begin
        c = new;
        foreach (a[i]) a[i] = new;
        make();
        build();
        n = new;
    end
    initial #12 go = 1;
    always @(posedge go) e = new;",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        // In the order of construction: `s`, `c`, `a[0]`, `a[1]`, `d`, `f`,
        // then `e`.
        assert_eq!(
            all_samples(&report),
            expected(&[("Top.cg", &[4, 4, 4, 4, 4, 4, 3]), ("Top.fall", &[4])])
        );
        for instance in 0..6 {
            assert_eq!(
                hit(bins(&report, "Top.cg", instance, "q")),
                q_bins(&[(0, 1), (1, 1), (2, 1), (3, 1)]),
                "{instance}"
            );
        }
        assert_eq!(
            hit(bins(&report, "Top.cg", 6, "q")),
            q_bins(&[(1, 1), (2, 1), (3, 1)])
        );
        assert_eq!(
            hit(bins(&report, "Top.fall", 0, "q")),
            q_bins(&[(1, 1), (2, 1), (3, 1), (4, 1)])
        );
    }
}

/// The same in both state modes, at every optimization level and with each
/// dead-store policy, for instances that a procedure and a class
/// constructor construct: no sample before the first event, `@(s)` in
/// two-state mode included.
#[test]
fn constructed_instances_sample_alike_in_every_mode() {
    let source = clocked(
        "    logic s = 0;
    initial #7 s = 1;
    covergroup pos @(posedge clk); coverpoint q { bins b[] = {[0:7]}; } endgroup
    covergroup level @(s); endgroup
    covergroup change @(clk); endgroup
    class K;
        covergroup kc @(posedge clk); coverpoint q { bins b[] = {[0:7]}; } endgroup
        function new(); kc = new; endfunction
    endclass
    pos p;
    level l;
    change a;
    K k;
    initial begin p = new; l = new; a = new; k = new; end",
    );
    for four_state in [false, true] {
        for level in [
            celox::OptLevel::O0,
            celox::OptLevel::O1,
            celox::OptLevel::O2,
        ] {
            for policy in [
                celox::DeadStorePolicy::Off,
                celox::DeadStorePolicy::PreserveTopPorts,
                celox::DeadStorePolicy::PreserveAllPorts,
            ] {
                let context = format!("four_state={four_state} {level:?} {policy:?}");
                let mut sim = simulation(&source)
                    .four_state(four_state)
                    .opt_level(level)
                    .dead_store_policy(policy)
                    .build()
                    .unwrap_or_else(|error| panic!("{context}: {error}"));
                sim.run_until(6).unwrap();
                assert_eq!(
                    all_samples(&sim.coverage()),
                    expected(&[
                        ("K::kc", &[1]),
                        ("Top.change", &[1]),
                        ("Top.level", &[0]),
                        ("Top.pos", &[1])
                    ]),
                    "{context}"
                );
                sim.run_until(42).unwrap();
                let report = sim.coverage();
                assert_eq!(
                    all_samples(&report),
                    expected(&[
                        ("K::kc", &[4]),
                        ("Top.change", &[8]),
                        ("Top.level", &[1]),
                        ("Top.pos", &[4])
                    ]),
                    "{context}"
                );
                for name in ["Top.pos", "K::kc"] {
                    assert_eq!(
                        hit(bins(&report, name, 0, "q")),
                        q_bins(&[(0, 1), (1, 1), (2, 1), (3, 1)]),
                        "{context} {name}"
                    );
                }
            }
        }
    }
}

sv_backends! {
    /// 19.4: a covergroup embedded in a class is sampled at its clocking
    /// event, bound in the class: a change of a property, an edge of a
    /// one-bit property, a named event of the object, through `this` and
    /// through a handle property, and an edge of a signal of the module
    /// that declares the class.
    fn embedded_covergroups_sample_at_events_of_their_class(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
class Obj; event m_ev; endclass
module Top;
    bit sclk = 0;
    class C;
        bit clk_bit;
        bit [3:0] m_z;
        event ev;
        Obj m_obj;
        bit [3:0] v;
        covergroup a @m_z; coverpoint v { bins b[] = {[0:15]}; } endgroup
        covergroup b @(posedge clk_bit); coverpoint v { bins b[] = {[0:15]}; } endgroup
        covergroup c @ev; coverpoint v { bins b[] = {[0:15]}; } endgroup
        covergroup d @(m_obj.m_ev); coverpoint v { bins b[] = {[0:15]}; } endgroup
        covergroup e @(posedge sclk); coverpoint v { bins b[] = {[0:15]}; } endgroup
        covergroup f @(this.ev); coverpoint v { bins b[] = {[0:15]}; } endgroup
        function new();
            m_obj = new;
            a = new; b = new; c = new; d = new; e = new; f = new;
        endfunction
    endclass
    C o;
    initial begin
        o = new;
        repeat (4) begin
            #5 o.v++; o.m_z++; o.clk_bit = 1; -> o.ev; -> o.m_obj.m_ev; sclk = 1;
            #5 o.clk_bit = 0; sclk = 0;
        end
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        assert_eq!(
            all_samples(&report),
            expected(&[
                ("C::a", &[4]),
                ("C::b", &[4]),
                ("C::c", &[4]),
                ("C::d", &[4]),
                ("C::e", &[4]),
                ("C::f", &[4]),
            ])
        );
        for name in ["C::a", "C::b", "C::c", "C::d", "C::e", "C::f"] {
            assert_eq!(
                hit(bins(&report, name, 0, "v")),
                hits(&[("b[1]", 1), ("b[2]", 1), ("b[3]", 1), ("b[4]", 1)]),
                "{name}"
            );
        }
    }
}

sv_backends! {
    /// The common verification form: a covergroup embedded in a monitor
    /// class of a package, sampled at an edge of the clock of the interface
    /// instance that a virtual interface property of the object denotes.
    /// When the property is assigned another instance (at 20), the wait
    /// that is armed already fires once more, at the next edge of the old
    /// clock (25), and the later waits are on the new one (30, 50, 70): a
    /// difference from 9.4.2, which waits on the new one at once, that a
    /// task of the class waiting on the same event control shows too (to
    /// be filed, documented).
    fn embedded_covergroups_sample_through_a_virtual_interface(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
interface ifc(input bit clk);
    bit [3:0] data;
endinterface
package mon_pkg;
    class Monitor;
        virtual ifc vif;
        covergroup cg @(posedge vif.clk); d: coverpoint vif.data { bins b[] = {[0:15]}; } endgroup
        function new(virtual ifc vif);
            this.vif = vif;
            cg = new;
        endfunction
    endclass
endpackage
module Top;
    import mon_pkg::*;
    bit clk = 0, slow = 0;
    always #5 clk = ~clk;
    always #10 slow = ~slow;
    ifc i(.clk(clk));
    ifc j(.clk(slow));
    Monitor m;
    initial begin
        i.data = 3;
        j.data = 9;
        m = new(i);
        #20 m.vif = j;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(19).unwrap();
        // `clk` rises at 5 and 15.
        assert_eq!(samples(&sim.coverage(), "mon_pkg::Monitor::cg"), [2]);
        // The old clock at 25, then `slow` at 30, 50 and 70.
        sim.run_until(82).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "mon_pkg::Monitor::cg"), [6]);
        assert_eq!(
            hit(bins(&report, "mon_pkg::Monitor::cg", 0, "d")),
            hits(&[("b[3]", 2), ("b[9]", 4)])
        );
    }
}

sv_backends! {
    /// A named event of an object that a handle property holds,
    /// `@(h.e)`: when the property is assigned another object (at 8), the
    /// wait that is armed stays on the event of the first object, so the
    /// triggers of the new object's event (at 10 and, through the handle,
    /// at 12) are no samples, as 15.5.5.1 has it for a wait on an event
    /// variable that is assigned. A process of the source waiting on
    /// `@(h.e)` of a module variable `h` wakes once the same way.
    fn embedded_covergroup_events_stay_on_the_object_of_their_first_wait(sim) {
        @setup {
            let source = clocked(
                "    class H; event e; endclass
    class E;
        H h;
        covergroup ec @(h.e); endgroup
        function new(); h = new; ec = new; endfunction
    endclass
    E e;
    H other;
    initial begin
        e = new;
        other = new;
        #6 -> e.h.e;
        #2 e.h = other;
        #2 -> other.e;
        #2 -> e.h.e;
    end",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        assert_eq!(samples(&sim.coverage(), "E::ec"), [1]);
    }
}

sv_backends! {
    /// 19.4: an embedded covergroup is assigned in the `new` method of its
    /// class alone; constructing it again there leaves the instance it held
    /// unsampled at the event, which the new one is. An instance of a type
    /// of the module stays sampled, whatever holds it.
    fn reconstruction_retires_the_old_sampler(sim) {
        @setup {
            let source = clocked(
                "    class K;
        bit [2:0] v;
        covergroup kc @(posedge clk); endgroup
        function new(int times); repeat (times) kc = new; endfunction
    endclass
    covergroup cg @(posedge clk); endgroup
    K one, two;
    cg c;
    initial begin
        one = new(1);
        two = new(3);
        c = new;
        c = new;
    end",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[("K::kc", &[4, 0, 0, 4]), ("Top.cg", &[4, 4])])
        );
    }
}

sv_backends! {
    /// The sampler holds the instance, and the object of an embedded one,
    /// for the rest of the simulation: they are sampled after the variables
    /// that held them are `null`.
    fn samplers_keep_their_instances_and_objects(sim) {
        @setup {
            let source = clocked(
                "    covergroup cg @(posedge clk); endgroup
    class K;
        covergroup kc @(posedge clk); endgroup
        function new(); kc = new; endfunction
    endclass
    cg c;
    K k;
    initial begin
        c = new;
        k = new;
        #12 c = null;
        k = null;
    end",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[("K::kc", &[4]), ("Top.cg", &[4])])
        );
    }
}

sv_backends! {
    /// Constructions that declaration initializers of a module make: the
    /// `new` of a class that embeds a clocked covergroup, the elements of an
    /// array of instances, a static property of a class, and instances of
    /// types of a package and of the compilation unit, whose events are
    /// bound where they are declared.
    fn constructions_from_declaration_initializers_sample(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
bit uclk = 0;
covergroup ucg @(posedge uclk); endgroup
package p;
    bit pclk = 0;
    covergroup pcg @(posedge pclk); endgroup
endpackage
module Top;
    bit clk = 0;
    always #5 begin
        clk = ~clk;
        uclk = ~uclk;
        p::pclk = ~p::pclk;
    end
    class K;
        static K single = new;
        covergroup kc @(posedge clk); endgroup
        function new(); kc = new; endfunction
    endclass
    covergroup cg @(posedge clk); endgroup
    K k = new;
    cg a[2] = '{new, new};
    p::pcg pc = new;
    ucg uc = new;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "K::kc"), [4, 4]);
        assert_eq!(samples(&report, "Top.cg"), [4, 4]);
        assert_eq!(samples(&report, "p::pcg"), [4]);
        assert_eq!(
            report
                .covergroups
                .iter()
                .filter(|group| group.name.ends_with("::ucg"))
                .map(|group| group.instances.iter().map(|i| i.samples).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            [vec![4]]
        );
    }
}

/// 18.14.1: the sampler of a constructed instance takes no seed of the
/// process that constructs it, so adding a clocked covergroup to a
/// procedure or to a class constructor changes neither its random values
/// nor those of the children it forks.
#[test]
fn constructing_clocked_instances_keeps_the_random_stimulus() {
    let draws = |construct: bool, in_class: bool| {
        fn when(construct: bool, text: &str) -> &str {
            if construct { text } else { "" }
        }
        let new = |text| when(construct, text);
        let source = format!(
            "`timescale 1ns/1ns
module Top(output int unsigned a, b, c, d, output int done);
    bit clk;
    always #5 clk = ~clk;
    covergroup cg @(posedge clk); endgroup
    class C;
        covergroup ecg @(posedge clk); endgroup
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
        fork d = $urandom; join_none
        c = $urandom;
        #1 done = 1;
    end
endmodule
",
            embedded = new(if in_class { "ecg = new;" } else { "" }),
            scope = new(if in_class { "" } else { "g = new;" }),
        );
        let mut sim = simulation(&source).four_state(false).build().unwrap();
        sim.run_until(12).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        if construct {
            assert_eq!(all_samples(&sim.coverage()).len(), 1);
        }
        ["a", "b", "c", "d"].map(|name| {
            let signal = sim.signal(name);
            sim.get(signal)
        })
    };
    let plain = draws(false, false);
    assert_ne!(plain[0], plain[2], "the stimulus is random");
    assert_eq!(draws(true, false), plain);
    assert_eq!(draws(true, true), plain);
}

sv_backends! {
    /// The sampler is no child of the process that constructs the instance:
    /// `wait fork` there returns when its own children end (9.6.1), and
    /// neither `disable fork` (9.6.3) nor `kill()` of that process (9.7)
    /// stops the sampling. A `fork`-`join_any` after a construction joins
    /// its own first branch.
    fn samplers_are_no_children_of_the_constructing_process(sim) {
        @setup {
            let source = clocked(
                "    covergroup cg @(posedge clk); endgroup
    cg c, d, e;
    int waited = -1, joined = -1;
    process p;
    initial begin
        c = new;
        fork #3; join_none
        wait fork;
        waited = $time;
        d = new;
        disable fork;
        fork #2; #6; join_any
        joined = $time;
    end
    initial begin
        p = process::self();
        e = new;
        #100;
    end
    initial #12 p.kill();",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        let waited = sim.signal("waited");
        let joined = sim.signal("joined");
        assert_eq!(sim.get(waited), BigUint::from(3u8));
        assert_eq!(sim.get(joined), BigUint::from(5u8));
        assert_eq!(samples(&sim.coverage(), "Top.cg"), [4, 4, 4]);
    }
}

sv_backends! {
    /// An instance of a type of the module that a procedure constructs is
    /// sampled by the edge-sensitive procedure of the type, with the
    /// flip-flops of the edge, as the instance of a declaration is: at the
    /// rising edges of the divided clock `div`, at 5 and 25, both sample `r`
    /// as 0 and 1, before the nonblocking assignments of the edge. The
    /// sampler of an embedded covergroup is a waiting process, which runs
    /// after those assignments and sees `r` as 1 and 2
    /// (celox-sim/celox#4771; a difference from 19.3, documented).
    fn constructed_instances_of_nba_updated_clocks(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0, div = 0;
    bit [3:0] r = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) div <= ~div;
    always_ff @(posedge div) r <= r + 4'd1;
    covergroup cg @(posedge div); coverpoint r { bins b[] = {[0:15]}; } endgroup
    class K;
        covergroup kc @(posedge div); coverpoint r { bins b[] = {[0:15]}; } endgroup
        function new(); kc = new; endfunction
    endclass
    cg s = new, c;
    K k;
    initial begin c = new; k = new; end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [2, 2]);
        assert_eq!(hit(bins(&report, "Top.cg", 0, "r")), once(&[0, 1]));
        assert_eq!(hit(bins(&report, "Top.cg", 1, "r")), once(&[0, 1]));
        assert_eq!(hit(bins(&report, "K::kc", 0, "r")), once(&[1, 2]));
    }
}

sv_backends! {
    /// `sample()`, `stop()`, `start()` and the options apply to a
    /// constructed instance as to any (19.8).
    fn constructed_instances_take_procedural_methods_and_options(sim) {
        @setup {
            let source = clocked(
                "    covergroup cg @(posedge clk);
        option.at_least = 2;
        coverpoint q { bins lo = {[0:1]}; bins hi = {[2:7]}; }
    endgroup
    class K;
        covergroup kc @(posedge clk);
            option.at_least = 2;
            coverpoint q { bins lo = {[0:1]}; bins hi = {[2:7]}; }
        endgroup
        function new(); kc = new; endfunction
    endclass
    cg c;
    K k;
    initial begin
        c = new;
        k = new;
        #12 c.stop();
        k.kc.stop();
        #10 c.start();
        k.kc.start();
        #1 c.sample();
        k.kc.sample();
    end",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        // Sampled at 5 (q = 0), 23 (q = 2, procedurally), 25 (q = 2) and 35
        // (q = 3), not at 15 (stopped).
        assert_eq!(samples(&report, "Top.cg"), [4]);
        assert_eq!(bins(&report, "Top.cg", 0, "q"), hits(&[("lo", 1), ("hi", 3)]));
        assert!(close(group(&report, "Top.cg").instances[0].coverage, 50.0));
        assert_eq!(samples(&report, "K::kc"), [4]);
        assert_eq!(bins(&report, "K::kc", 0, "q"), hits(&[("lo", 1), ("hi", 3)]));
    }
}

/// The untimed `Simulator` runs no process: a design that constructs a
/// clocked instance in a procedure or a declaration initializer needs a
/// `Simulation`.
#[test]
fn constructed_instances_require_simulation() {
    for items in [
        "    cg c;\n    initial c = new;",
        "    class K; covergroup kc @(posedge clk); endgroup function new(); kc = new; endfunction endclass\n    K k = new;",
    ] {
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top(input bit clk, input bit [3:0] d);\n    covergroup cg @(posedge clk); coverpoint d; endgroup\n{items}\nendmodule\n"
        );
        let sources = || vec![(source.as_str(), Path::new("covergroup_constructed.sv"))];
        celox::Simulation::from_sv_sources(sources(), "Top")
            .build()
            .unwrap();
        let error = match Simulator::from_sv_sources(sources(), "Top").build() {
            Ok(_) => panic!("{items}: a clocking event needs a Simulation"),
            Err(error) => format!("{error:?}"),
        };
        assert!(error.contains("Simulation"), "{items}: {error}");
    }
}

/// A declaration initializer may construct a clocked instance, whose
/// sampler is a process of the analysis, but a fork of the source that a
/// function it calls makes stays the error 13.4.4 makes it, also beside a
/// clocked covergroup in the same constructor.
#[test]
fn user_forks_of_declaration_initializers_stay_rejected() {
    let source = |fork: &str| {
        format!(
            "`timescale 1ns/1ns
module Top;
    bit clk;
    int n;
    class K;
        covergroup kc @(posedge clk); endgroup
        function new(); kc = new; {fork} endfunction
    endclass
    K k = new;
endmodule
"
        )
    };
    simulation(&source("")).build().unwrap();
    let error = match simulation(&source("fork n++; join_none")).build() {
        Ok(_) => panic!("a fork of a declaration initializer"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("background process scheduled by a declaration initializer; IEEE 1800-2023 13.4.4 requires an initial/always process"),
        "{error}"
    );
}

sv_backends! {
    /// The instances that procedures construct are held by lists of the
    /// module instance: two instances of a module each sample theirs at
    /// their own clock, a type of a generate block samples at its event,
    /// and more instances than the variables of a list hold (here 7, of an
    /// edge and of a change event) are sampled all, from the queue of the
    /// list. An instance constructed between occurrences samples from the
    /// next one, also one an edge-sensitive procedure constructs at an
    /// edge, which the sampler of the edge ran before.
    fn constructed_instances_are_held_by_the_lists_of_their_scope(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Sub(input bit clk);
    covergroup cg @(posedge clk); endgroup
    cg c;
    initial c = new;
endmodule
module Top;
    bit clk = 0, slow = 0;
    always #5 clk = ~clk;
    always #10 slow = ~slow;
    Sub fast_one(.clk(clk));
    Sub slow_one(.clk(slow));
    covergroup many @(posedge clk); endgroup
    covergroup changes @(clk); endgroup
    many m[7];
    changes h[7];
    if (1) begin : g
        covergroup gc @(negedge clk); endgroup
        gc x;
        initial x = new;
    end
    many late, at_edge;
    initial begin
        foreach (m[i]) m[i] = new;
        foreach (h[i]) h[i] = new;
        #12 late = new;
    end
    always @(posedge clk) if (at_edge == null) at_edge = new;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(42).unwrap();
        // `clk` rises at 5, 15, 25 and 35 and changes 8 times, `slow` rises
        // at 10 and 30.
        let mut many = vec![4; 7];
        many.extend([3, 3]);
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[
                ("Sub.cg", &[4, 2]),
                ("Top.changes", &[8; 7]),
                ("Top.g.gc", &[4]),
                ("Top.many", &many),
            ])
        );
    }
}

/// A checkpoint keeps the lists: restored into another simulation, the
/// instances of the list, the queue of it included, sample on.
#[test]
fn constructed_instance_lists_roll_back_with_a_checkpoint() {
    let source = clocked(
        "    covergroup cg @(posedge clk); coverpoint q { bins b[] = {[0:7]}; } endgroup
    cg a[6];
    initial begin
        foreach (a[i]) begin
            #4 a[i] = new;
        end
    end",
    );
    let build = || simulation(&source).four_state(false).build().unwrap();
    let mut first = build();
    // At 22 five instances exist, one of them in the queue of the list.
    first.run_until(22).unwrap();
    let checkpoint = first.checkpoint().unwrap();
    first.run_until(42).unwrap();
    let expected = first.coverage();
    // Constructed at 4, 8, ..., 24: sampled from 5, 15 and 25 on.
    assert_eq!(samples(&expected, "Top.cg"), [4, 3, 3, 2, 2, 2]);
    let mut second = build();
    second.restore(&checkpoint).unwrap();
    second.run_until(42).unwrap();
    assert_eq!(second.coverage(), expected);
}

/// The instances of a type of the module may be constructed in any
/// procedure, as they join the list of the type: in `always_comb` and a
/// continuous assignment (both at 12, when `g` rises), in `always_ff` (at
/// the edge at 5, whose sampler ran before) and in a final procedure,
/// directly and through a function (no sample after it).
#[test]
fn module_type_instances_are_constructed_in_any_procedure() {
    let source = clocked(
        "    covergroup ta @(posedge clk); endgroup
    covergroup tb @(posedge clk); endgroup
    covergroup tc @(posedge clk); endgroup
    covergroup td @(posedge clk); endgroup
    covergroup te @(posedge clk); endgroup
    ta a;
    tb b;
    tc c;
    td d;
    te e;
    bit g = 0, made = 0;
    initial #12 g = 1;
    function automatic void mka(); a = new; endfunction
    function automatic bit mkb(input bit v); if (v) b = new; return v; endfunction
    function automatic void mkd(); d = new; endfunction
    always_comb if (g) mka();
    wire w = mkb(g);
    always_ff @(posedge clk) if (!made) begin c = new; made <= 1; end
    final mkd();
    final e = new;
    initial #42 $finish;",
    );
    for four_state in [false, true] {
        let mut sim = simulation(&source).four_state(four_state).build().unwrap();
        sim.run_until(50).unwrap();
        assert_eq!(
            all_samples(&sim.coverage()),
            expected(&[
                ("Top.ta", &[3]),
                ("Top.tb", &[3]),
                ("Top.tc", &[3]),
                ("Top.td", &[0]),
                ("Top.te", &[0]),
            ]),
            "four_state={four_state}"
        );
    }
}

/// A hierarchical call of a function that constructs an instance of a
/// covergroup with a clocking event of its module is not supported, as one
/// of a function that names another variable of that module is not; the
/// rejection names the covergroup, not the variables of its list.
#[test]
fn hierarchical_constructions_name_the_covergroup() {
    let source = "`timescale 1ns/1ns
module Sub(input bit clk);
    covergroup cg @(posedge clk); endgroup
    cg d;
    function void mk(); d = new; endfunction
endmodule
module Top;
    bit clk = 0;
    always #5 clk = ~clk;
    Sub u(clk);
    initial #12 u.mk();
endmodule
";
    let error = match simulation(source).build() {
        Ok(_) => panic!("a hierarchical call that constructs"),
        Err(error) => error.to_string(),
    };
    // The report wraps its lines.
    let error = error
        .split_whitespace()
        .filter(|word| *word != "│")
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        error.contains(
            "hierarchical call `u.mk` of a subroutine that constructs an instance of a covergroup with a clocking event of its scope"
        ),
        "{error}"
    );
    assert!(!error.contains("%covergroup"), "{error}");
}

/// A counter `q` of the rising edges of `clk` (every 10 ns from 5 ns), the
/// `logic` variables `bus_a` and `bus_b` that continuous assignments drive
/// from it, and the unpacked array `bus_event`, as the coverage interfaces
/// of Caliptra have them; `items` are among the module's items.
fn buses(items: &str) -> String {
    format!(
        r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned ca, cb);
    bit clk = 0;
    always #5 clk = ~clk;
    bit [2:0] q = 0;
    always_ff @(posedge clk) q <= q + 3'd1;
    logic [3:0] bus_a, bus_b;
    assign bus_a = {{1'b0, q}};
    assign bus_b = {{1'b1, q}};
    logic [3:0] bus_event[0:7];
    for (genvar i = 0; i < 8; i++) begin : drive
        assign bus_event[i] = {{1'b0, q}} + 4'(i);
    end
{items}
endmodule
"#
    )
}

/// The value bins `b[v]` of `values` hit once each.
fn once_each(values: &[u64]) -> Vec<(String, u64)> {
    values
        .iter()
        .map(|value| (format!("b[{value}]"), 1))
        .collect()
}

sv_backends! {
    /// 19.3: "A ref argument tracks the value of the actual argument as it
    /// changes": an instance that a declaration constructs reads, at each
    /// sample at its clocking event, the variables that the actuals of the
    /// declaration name, each declaration its own, in its coverpoints,
    /// guards, crosses and in `with` filters a sample evaluates, an input
    /// formal beside them keeping the value `new` passed. An unpacked
    /// array is passed as a whole. The samples are those of a covergroup
    /// whose coverpoints name the variables themselves.
    fn reference_arguments_track_the_actuals_of_their_declarations(sim) {
        @setup {
            let source = buses(
                "    covergroup at_edge(ref logic [3:0] v, input bit [3:0] lo) @(posedge clk);
        coverpoint v { bins b[] = {[0:15]}; }
        above: coverpoint v iff (v >= lo) { bins b[] = {[0:15]}; }
    endgroup
    at_edge a = new(bus_a, 4'd1), b = new(bus_b, 4'd10);
    covergroup at_change(const ref logic [3:0] v) @(clk);
        coverpoint v { bins b[] = {[0:15]}; }
    endgroup
    at_change c = new(bus_a);
    covergroup direct @(clk); coverpoint bus_a { bins b[] = {[0:15]}; } endgroup
    direct d = new;
    covergroup events(ref logic [3:0] e[0:7]) @(posedge clk);
        e2: coverpoint e[2] { bins b[] = {[0:15]}; }
    endgroup
    events e = new(bus_event);
    covergroup pair(ref logic [3:0] v, ref logic [3:0] w) @(negedge clk);
        cv: coverpoint v { bins b[] = {[0:15]}; }
        vw: cross v, w;
    endgroup
    pair p = new(bus_a, bus_b);",
            );
        }
        @build simulation(&source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        assert_eq!(
            all_samples(&report),
            expected(&[
                ("Top.at_change", &[8]),
                ("Top.at_edge", &[4, 4]),
                ("Top.direct", &[8]),
                ("Top.events", &[4]),
                ("Top.pair", &[4]),
            ])
        );
        // Before the nonblocking assignments of each edge, as at any
        // clocking event (19.3).
        assert_eq!(hit(bins(&report, "Top.at_edge", 0, "v")), once_each(&[0, 1, 2, 3]));
        assert_eq!(hit(bins(&report, "Top.at_edge", 0, "above")), once_each(&[1, 2, 3]));
        assert_eq!(hit(bins(&report, "Top.at_edge", 1, "v")), once_each(&[8, 9, 10, 11]));
        assert_eq!(hit(bins(&report, "Top.at_edge", 1, "above")), once_each(&[10, 11]));
        let changes = hits(&[("b[0]", 1), ("b[1]", 2), ("b[2]", 2), ("b[3]", 2), ("b[4]", 1)]);
        assert_eq!(hit(bins(&report, "Top.at_change", 0, "v")), changes);
        assert_eq!(hit(bins(&report, "Top.direct", 0, "bus_a")), changes);
        assert_eq!(hit(bins(&report, "Top.events", 0, "e2")), once_each(&[2, 3, 4, 5]));
        // At the falling edges, after the counter moved: v is 1..4 and w
        // 9..12.
        assert_eq!(hit(bins(&report, "Top.pair", 0, "cv")), once_each(&[1, 2, 3, 4]));
        assert_eq!(hit(bins(&report, "Top.pair", 0, "vw")).len(), 4);
    }
}

sv_backends! {
    /// 19.8's example shape, a covergroup with `ref` formals and no
    /// clocking event: `sample()` through the variable of the declaration
    /// reads the actuals of that declaration, from a procedure, a task, a
    /// function or a method of a class of the module, and at an edge. An
    /// option that reads a `ref` formal reads it when `new` runs, as the
    /// expressions of options do (19.7). The actual may be a member of an
    /// unpacked structure (13.5.2). A `ref` formal of an overridden sample
    /// method is passed by each call, through any handle.
    fn reference_arguments_without_an_event_are_sampled_through_their_declarations(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done, output longint unsigned ra, rb);
    int x = 0, y = 0;
    int least = 2;
    covergroup cg(ref int v, input int base, ref int n);
        option.at_least = n;
        cp: coverpoint v - base { bins b[] = {[0:7]}; }
    endgroup
    cg a = new(x, 0, least), b = new(y, 4, least);
    task automatic both(); a.sample(); b.sample(); endtask
    function automatic void only_a(); a.sample(); endfunction
    class K; function void go(); b.sample(); endfunction endclass
    covergroup sf with function sample(ref int s); coverpoint s { bins b[] = {[0:7]}; } endgroup
    sf e = new;
    sf h;
    typedef struct { int m; int n; } pair_t;
    pair_t pr;
    covergroup member(ref int v); coverpoint v { bins b[] = {[0:7]}; } endgroup
    member m = new(pr.n);
    bit clk = 0;
    int edges = 0;
    always @(posedge clk) if (edges < 2) begin a.sample(); edges <= edges + 1; end
    initial begin
        K k;
        k = new;
        h = e;
        x = 1; y = 5;
        a.sample(); b.sample();
        x = 2; y = 6; both();
        x = 3; only_a();
        y = 7; k.go();
        least = 9;
        a.sample();
        e.sample(x); h.sample(y);
        pr.m = 1; pr.n = 6; m.sample();
        x = 4;
        #1 clk = 1;
        #1 clk = 0; x = 5;
        #1 clk = 1;
        #1 ra = $realtobits(a.get_inst_coverage());
        rb = $realtobits(b.get_inst_coverage());
        done = 1;
    end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(10).unwrap();
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(1u8));
        let report = sim.coverage();
        assert_eq!(
            all_samples(&report),
            expected(&[("Top.cg", &[6, 3]), ("Top.member", &[1]), ("Top.sf", &[2])])
        );
        assert_eq!(hit(bins(&report, "Top.member", 0, "v")), once_each(&[6]));
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "cp")),
            hits(&[("b[1]", 1), ("b[2]", 1), ("b[3]", 2), ("b[4]", 1), ("b[5]", 1)])
        );
        assert_eq!(hit(bins(&report, "Top.cg", 1, "cp")), once_each(&[1, 2, 3]));
        assert_eq!(hit(bins(&report, "Top.sf", 0, "s")), once_each(&[3, 7]));
        // `at_least` is 2, the value of `least` at the construction: one
        // bin of eight of `a` is covered, none of `b`.
        assert_eq!(real(&mut sim, "ra"), 12.5);
        assert_eq!(real(&mut sim, "rb"), 0.0);
    }
}

/// The same in both state modes, at every optimization level and with each
/// dead-store policy, with sites in a generate loop: an actual that nothing
/// but the covergroup reads keeps its value.
#[test]
fn reference_arguments_sample_alike_in_every_mode() {
    let source = buses(
        "    logic [3:0] only;
    assign only = {1'b0, q};
    covergroup cg(ref logic [3:0] v) @(posedge clk); coverpoint v { bins b[] = {[0:15]}; } endgroup
    cg c = new(only);
    covergroup ch(ref logic [3:0] v) @(clk); coverpoint v { bins b[] = {[0:15]}; } endgroup
    ch h = new(only);
    for (genvar i = 0; i < 2; i++) begin : g
        logic [3:0] own;
        assign own = {1'b0, q} + 4'(i);
        cg s = new(own);
        cg t = new(bus_event[i + 4]);
    end",
    );
    for four_state in [false, true] {
        for level in [
            celox::OptLevel::O0,
            celox::OptLevel::O1,
            celox::OptLevel::O2,
        ] {
            for policy in [
                celox::DeadStorePolicy::Off,
                celox::DeadStorePolicy::PreserveTopPorts,
                celox::DeadStorePolicy::PreserveAllPorts,
            ] {
                let context = format!("four_state={four_state} {level:?} {policy:?}");
                let mut sim = simulation(&source)
                    .four_state(four_state)
                    .opt_level(level)
                    .dead_store_policy(policy)
                    .build()
                    .unwrap_or_else(|error| panic!("{context}: {error}"));
                sim.run_until(42).unwrap();
                let report = sim.coverage();
                assert_eq!(
                    all_samples(&report),
                    expected(&[("Top.cg", &[4, 4, 4, 4, 4]), ("Top.ch", &[8])]),
                    "{context}"
                );
                let pins = [
                    (0, [0, 1, 2, 3]),
                    (1, [0, 1, 2, 3]),
                    (2, [4, 5, 6, 7]),
                    (3, [1, 2, 3, 4]),
                    (4, [5, 6, 7, 8]),
                ];
                for (instance, values) in pins {
                    assert_eq!(
                        hit(bins(&report, "Top.cg", instance, "v")),
                        once_each(&values),
                        "{context} {instance}"
                    );
                }
                assert_eq!(
                    hit(bins(&report, "Top.ch", 0, "v")),
                    hits(&[
                        ("b[0]", 1),
                        ("b[1]", 2),
                        ("b[2]", 2),
                        ("b[3]", 2),
                        ("b[4]", 1)
                    ]),
                    "{context}"
                );
            }
        }
    }
}

sv_backends! {
    /// `sample()` in an `always_comb` procedure runs it at each change of
    /// what the procedure reads (9.2.2.2), the sampled variables included:
    /// through a `ref` argument the same changes as through the variable
    /// itself.
    fn reference_arguments_sampled_in_always_comb_follow_their_actuals(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top(output int done);
    int y = 0;
    covergroup direct; coverpoint y { bins b[] = {[0:7]}; } endgroup
    direct d = new;
    covergroup cg(ref int v); coverpoint v { bins b[] = {[0:7]}; } endgroup
    cg c = new(y);
    always_comb d.sample();
    always_comb c.sample();
    initial begin #1 y = 1; #1 y = 2; #1 y = 2; #1 y = 5; #1 done = 1; end
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(10).unwrap();
        let report = sim.coverage();
        let direct = samples(&report, "Top.direct");
        assert!(direct[0] >= 4, "{direct:?}");
        assert_eq!(samples(&report, "Top.cg"), direct);
        assert_eq!(bins(&report, "Top.cg", 0, "v"), bins(&report, "Top.direct", 0, "y"));
        assert_eq!(
            hit(bins(&report, "Top.cg", 0, "v")).iter().map(|(bin, _)| bin.as_str()).collect::<Vec<_>>(),
            ["b[0]", "b[1]", "b[2]", "b[5]"]
        );
    }
}

sv_backends! {
    /// 19.5.1.1: "The with clause behaves as if the expression were
    /// evaluated for every value in the covergroup_range_list at the time the
    /// covergroup instance is constructed": a filter that reads an input
    /// argument keeps the value `new` passed, also over a range of more values
    /// than a construction is asked about, whose filter a sample evaluates.
    /// (A `ref` argument may not be read there, 19.5.)
    fn with_filters_read_input_arguments_as_new_passed_them(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    always #5 clk = ~clk;
    int x = 5;
    int w = 0;
    initial #1 w = 100;
    covergroup cg(input int r) @(posedge clk);
        big: coverpoint x { bins near = {[0:$]} with (item >= r); }
        sm: coverpoint x { bins near = {[0:15]} with (item >= r); }
    endgroup
    cg c = new(w);
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(32).unwrap();
        let report = sim.coverage();
        assert_eq!(samples(&report, "Top.cg"), [3]);
        assert_eq!(hit(bins(&report, "Top.cg", 0, "big")), hits(&[("near", 3)]));
        assert_eq!(hit(bins(&report, "Top.cg", 0, "sm")), hits(&[("near", 3)]));
    }
}

sv_backends! {
    /// A static property of a class is a variable (8.9, 13.5.2): as a
    /// `ref` actual it is sampled as a coverpoint of the property is.
    fn static_class_properties_are_reference_actuals(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    always #5 clk = ~clk;
    class S; static logic [3:0] m = 0; endclass
    always_ff @(posedge clk) S::m <= S::m + 4'd3;
    covergroup cg(ref logic [3:0] v) @(posedge clk); cp: coverpoint v { bins b[] = {[0:15]}; } endgroup
    cg c = new(S::m);
    covergroup dg @(posedge clk); cp: coverpoint S::m { bins b[] = {[0:15]}; } endgroup
    dg d = new;
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(42).unwrap();
        let report = sim.coverage();
        assert_eq!(hit(bins(&report, "Top.cg", 0, "cp")), once_each(&[0, 3, 6, 9]));
        assert_eq!(bins(&report, "Top.cg", 0, "cp"), bins(&report, "Top.dg", 0, "cp"));
    }
}

sv_backends! {
    /// 19.11.3, 6.22: instances of a module whose parameter takes its default
    /// and instances that write the same value, by position or by name, share
    /// the covergroup type: with `merge_instances` the coverage of the type is
    /// that of the three samples together, and the report lists it once.
    fn module_instances_with_the_same_parameter_values_share_types(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module M #(int N = 2) (input logic [3:0] d, output bit [63:0] t);
    covergroup cg; type_option.merge_instances = 1; coverpoint d { bins b[] = {[0:15]}; } endgroup
    cg c;
    initial begin c = new; #1 c.sample(); #1 t = $realtobits(c.get_coverage()); end
endmodule
module Top(output bit [63:0] ta, tb, tc, te, tf, tg);
    logic [3:0] x = 1, y = 2, z = 3;
    M a(.d(x), .t(ta));
    M #(2) b(.d(y), .t(tb));
    M #(.N(2)) c(.d(z), .t(tc));
    M #(3) e(.d(z), .t(te));
    M #(32'sd2) f(.d(z), .t(tf));
    M #(4'd2) g(.d(z), .t(tg));
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(3).unwrap();
        for name in ["ta", "tb", "tc", "tf", "tg"] {
            assert_eq!(real(&mut sim, name), 18.75, "{name}");
        }
        assert_eq!(real(&mut sim, "te"), 6.25);
        let report = sim.coverage();
        assert_eq!(instances_of_types(&report, "M.cg"), [1, 5]);
    }
}

/// The number of instances of each covergroup type named `name` in
/// `report`, fewest first.
fn instances_of_types(report: &celox::CoverageReport, name: &str) -> Vec<usize> {
    let mut groups = report
        .covergroups
        .iter()
        .filter(|group| group.name == name)
        .map(|group| group.instances.len())
        .collect::<Vec<_>>();
    groups.sort();
    groups
}

sv_backends! {
    /// 6.22, 6.20.2: an unpacked array parameter is a value of the
    /// specialization like any other parameter. Instances whose arrays hold
    /// other values have types of their own, with the bins those values
    /// give; one that writes the default shares the type of one that takes
    /// it.
    fn module_instances_with_other_array_parameters_have_other_types(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module M #(int A [2] = '{1, 2}) (input logic [3:0] d, output bit [63:0] t);
    covergroup cg; coverpoint d { bins first = {A[0]}; bins high = {15}; } endgroup
    cg c;
    initial begin c = new; #1 c.sample(); #1 t = $realtobits(c.get_inst_coverage()); end
endmodule
module Top(output bit [63:0] ta, tb, tc);
    logic [3:0] x = 1, y = 5;
    M a(.d(x), .t(ta));
    M #('{5, 2}) b(.d(y), .t(tb));
    M #('{1, 2}) c(.d(x), .t(tc));
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(3).unwrap();
        for name in ["ta", "tb", "tc"] {
            assert_eq!(real(&mut sim, name), 50.0, "{name}");
        }
        let report = sim.coverage();
        assert_eq!(instances_of_types(&report, "M.cg"), [1, 2]);
    }
}

sv_backends! {
    /// 6.22, 6.20.2: a value parameter without a type takes the type of its
    /// value, so instances that give it values of other widths have types
    /// of their own, with the automatic bins of a coverpoint of that width
    /// (19.5.3); two that give it the same value of the same width, however
    /// written, share one.
    fn module_instances_with_parameter_values_of_other_types_have_other_types(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
module M #(parameter P = 1) (input logic [7:0] d, output bit [63:0] t);
    logic [$bits(P)-1:0] v;
    assign v = d[$bits(P)-1:0];
    covergroup cg; coverpoint v; endgroup
    cg c;
    initial begin c = new; #1 c.sample(); #1 t = $realtobits(c.get_inst_coverage()); end
endmodule
module Top(output bit [63:0] ta, tb, tc, te);
    logic [7:0] x = 1;
    M a(.d(x), .t(ta));
    M #(1'b1) b(.d(x), .t(tb));
    M #(4'b1) c(.d(x), .t(tc));
    M #(4'h1) e(.d(x), .t(te));
endmodule
"#;
        }
        @build simulation(source).four_state(false);
        sim.run_until(3).unwrap();
        // 64 automatic bins of a 32-bit value, 2 of a bit, 16 of 4 bits.
        assert_eq!(real(&mut sim, "ta"), 100.0 / 64.0);
        assert_eq!(real(&mut sim, "tb"), 50.0);
        assert_eq!(real(&mut sim, "tc"), 6.25);
        assert_eq!(real(&mut sim, "te"), 6.25);
        let report = sim.coverage();
        assert_eq!(instances_of_types(&report, "M.cg"), [1, 1, 2]);
    }
}

#[path = "covergroup_transitions.rs"]
mod transitions;
