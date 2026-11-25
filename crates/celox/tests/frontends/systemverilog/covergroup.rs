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
    let package = "package p;\n    covergroup pcg(int x); coverpoint x; endgroup\nendpackage\n";
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
        assert!(error.contains("covergroup coverpoint"), "{body}: {error}");
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
