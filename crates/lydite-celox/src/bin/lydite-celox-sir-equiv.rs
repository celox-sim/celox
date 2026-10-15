//! Translation validation of the SIR optimizer over the reusable Veryl suite.
//! Each case runs on the default Celox backend so that every design it builds
//! is compiled; the SIR before and after optimization of that same compile is
//! then checked phase by phase with `lydite_celox::equivalence`.
use celox::{BigUint, SimBackend, Simulator, TraceOptions};
use celox_test_suite::{Backend, Design, Result, SignalPath, veryl::cases};
use lydite_celox::equivalence::{self, Status};
use lydite_solver::finite::Limits;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

struct CeloxBackend<B: SimBackend>(Simulator<B>);

impl<B: SimBackend> CeloxBackend<B> {
    fn signal(&self, path: &SignalPath) -> celox::SignalRef {
        let instances: Vec<_> = path
            .instances
            .iter()
            .map(|i| (i.name.as_str(), i.index.unwrap_or(0)))
            .collect();
        self.0.child_signal(&instances, &path.name)
    }
}

impl<B: SimBackend> Backend for CeloxBackend<B> {
    fn write(&mut self, path: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        let signal = self.signal(path);
        if mask == BigUint::default() {
            self.0.set_wide(signal, payload);
        } else {
            self.0.set_four_state(signal, payload, mask);
        }
        Ok(())
    }
    fn read(&mut self, path: &SignalPath) -> Result<(BigUint, BigUint)> {
        Ok(self.0.get_four_state(self.signal(path)))
    }
    fn eval_comb(&mut self) -> Result<()> {
        self.0.eval_comb().map_err(Into::into)
    }
    fn run_testbench(&mut self) -> Result<()> {
        let program = celox::testbench::compile_initial_testbench(&self.0)
            .ok_or("testbench program was not compiled")?;
        match celox::testbench::run_compiled_testbench_to_finish(&mut self.0, &program) {
            celox::TestResult::Pass => Ok(()),
            result => Err(format!("testbench failed: {result:?}").into()),
        }
    }
    fn tick(&mut self, event: &str) -> Result<()> {
        self.0.tick(self.0.event(event)).map_err(Into::into)
    }
}

fn status(s: &Status) -> Value {
    match s {
        Status::Equivalent => json!({"status":"equivalent"}),
        Status::Counterexample { lane, witness } => {
            json!({"status":"counterexample","lane":lane,"witness":witness})
        }
        Status::Unsupported(reason) => json!({"status":"unsupported","reason":reason}),
        Status::Unknown(reason) => json!({"status":"unknown","reason":reason}),
    }
}

fn validate(design: &Design, limits: &Limits) -> Result<(Value, Box<dyn Backend>)> {
    let start = Instant::now();
    let sources = design
        .sources
        .iter()
        .map(|source| (source.text.as_str(), source.path.as_path()))
        .collect::<Vec<_>>();
    let builder = design.parameters.iter().fold(
        Simulator::from_sources(sources, &design.top)
            .four_state(design.four_state)
            .allow_always_ff_function_effects(true)
            .trace(TraceOptions {
                pre_optimized_sir: true,
                post_optimized_sir: true,
                ..Default::default()
            }),
        |builder, (name, value)| builder.param(name, *value),
    );
    let result = builder.build_with_trace();
    let sim = result.res.map_err(|e| e.to_string())?;
    let mut row = json!({"top": design.top, "four_state": design.four_state});
    if design.four_state {
        row["status"] = "four_state_unsupported".into();
    } else {
        let pre = result
            .trace
            .pre_optimized_sir
            .ok_or("missing pre-optimized SIR")?;
        let post = result
            .trace
            .post_optimized_sir
            .ok_or("missing post-optimized SIR")?;
        let objects = pre.runtime.design.semantic().state_objects.iter();
        let phases = equivalence::check(objects, &pre.sir, &post.sir, limits);
        let mut unchecked = equivalence::unchecked(&pre.sir);
        unchecked.extend(equivalence::unchecked(&post.sir));
        unchecked.dedup();
        let worst = phases
            .iter()
            .map(|p| match p.status {
                Status::Counterexample { .. } => 3,
                Status::Unknown(_) => 2,
                Status::Unsupported(_) => 1,
                Status::Equivalent => 0,
            })
            .max()
            .unwrap_or(0);
        row["status"] = ["equivalent", "unsupported", "unknown", "counterexample"][worst].into();
        if worst == 0 && !unchecked.is_empty() {
            row["status"] = "equivalent_partial".into();
        }
        row["unchecked_collections"] = json!(unchecked);
        row["phases"] = phases
            .iter()
            .map(|p| {
                let mut v = status(&p.status);
                v["phase"] = p.phase.clone().into();
                v["lanes"] = p.lanes.into();
                v["solver_queries"] = p.solver_queries.into();
                v["instructions"] = json!([p.instructions.0, p.instructions.1]);
                v
            })
            .collect();
    }
    row["elapsed_ms"] = (start.elapsed().as_millis() as u64).into();
    Ok((row, Box::new(CeloxBackend(sim))))
}

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(out) = args.first().map(PathBuf::from) else {
        eprintln!("usage: lydite-celox-sir-equiv OUT.json [CASE...]");
        std::process::exit(2)
    };
    let limits = Limits {
        timeout_ms: Duration::from_secs(10).as_millis() as u64,
        ..Limits::default()
    };
    let mut rows = vec![];
    for case in cases().filter(|c| args.len() == 1 || args[1..].iter().any(|n| n == c.name)) {
        let designs = Rc::new(RefCell::new(vec![]));
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            case.run(&mut |design| match validate(design, &limits) {
                Ok((row, backend)) => {
                    designs.borrow_mut().push(row);
                    Ok(backend)
                }
                Err(e) => {
                    designs.borrow_mut().push(
                        json!({"top":design.top,"status":"build_failed","reason":e.to_string()}),
                    );
                    Err(e)
                }
            })
        }));
        let designs = designs.take();
        let summary = designs
            .iter()
            .map(|d| d["status"].as_str().unwrap_or("?").to_owned())
            .collect::<Vec<_>>();
        println!("{} {}", case.name, summary.join(","));
        rows.push(json!({"case":case.name,"case_completed":run.is_ok(),"designs":designs}));
    }
    std::fs::write(&out, serde_json::to_string_pretty(&rows).unwrap()).unwrap();
}
