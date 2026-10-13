//! Capture compiler decisions and real lane execution into an offline report.
#![allow(clippy::disallowed_macros)]

use celox::{SimBackend, Simulator};
use clap::{Parser, ValueEnum};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
use tracing::{
    Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Backend {
    Native,
    Cranelift,
}

#[derive(Parser, Debug)]
#[command(
    about = "Visualize Celox partition decisions, physical dependencies and real execution (offline HTML + JSON)"
)]
struct Args {
    /// Veryl files to concatenate. Omit for the replicated datapath demo.
    #[arg(long = "source")]
    sources: Vec<PathBuf>,
    /// Load a Veryl project, preserving file paths and project metadata.
    #[arg(long, requires = "testbench_ticks")]
    project: Option<PathBuf>,
    /// Run the real initial testbench up to this tick count (project mode).
    #[arg(long, requires = "project", conflicts_with = "inputs", value_parser = clap::value_parser!(u64).range(1..))]
    testbench_ticks: Option<u64>,
    #[arg(long, default_value = "Top")]
    top: String,
    #[arg(long, value_enum, default_value = "native")]
    backend: Backend,
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=64))]
    threads: u32,
    /// Number of nonlinear stages per worker in the built-in demo.
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..=4096))]
    demo_stages: u32,
    /// Omit expanded SIR listings for large designs; retain source graphs and dependencies.
    #[arg(long)]
    omit_sir: bool,
    /// Force partitioning and parallel execution, even when slower.
    #[arg(long)]
    always: bool,
    #[arg(long, default_value = "clk")]
    clock: String,
    /// Constant input assignments, e.g. --set seed=123. Applied before warmup.
    #[arg(long = "set", value_parser = assignment)]
    inputs: Vec<(String, u64)>,
    /// Signals compared with a 1-thread build, or final snapshots in testbench mode.
    #[arg(long = "observe")]
    outputs: Vec<String>,
    #[arg(long, default_value_t = 256)]
    warmup: u32,
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..))]
    cycles: u32,
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..=1000))]
    capture: u32,
    #[arg(long, default_value = "work/parallel-report")]
    output: PathBuf,
}

fn assignment(value: &str) -> Result<(String, u64), String> {
    let (name, value) = value.split_once('=').ok_or("expected NAME=INTEGER")?;
    Ok((
        name.into(),
        value
            .parse()
            .map_err(|_| "expected an unsigned decimal integer")?,
    ))
}

#[derive(Default)]
struct Fields(serde_json::Map<String, Value>, bool);
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if self.1 && field.name() == "sir" {
            self.0.insert(
                "sir".into(),
                json!("Omitted by --omit-sir; source paths, expressions and dependencies retained"),
            );
            return;
        }
        let value = format!("{value:?}");
        self.0.insert(
            field.name().into(),
            serde_json::from_str(&value).unwrap_or(Value::String(value)),
        );
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), json!(value));
    }
}

struct Capture {
    events: Arc<Mutex<Vec<Value>>>,
    window: Arc<Window>,
    omit_sir: bool,
}

#[derive(Default)]
struct Window {
    active: AtomicBool,
    running: AtomicBool,
    calls: AtomicU64,
    after: u64,
    length: u64,
}
impl Window {
    fn enables(&self, metadata: &tracing::Metadata<'_>) -> bool {
        match metadata.target() {
            "celox::parallel" => self.active.load(Ordering::Relaxed),
            "celox::parallel::selection" => true,
            "celox::parallel::call" => self.running.load(Ordering::Relaxed),
            _ => false,
        }
    }
}
impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        fields
            .0
            .insert("span".into(), json!(attrs.metadata().name()));
        fields
            .0
            .insert("id".into(), json!(id.into_u64().to_string()));
        ctx.span(id).unwrap().extensions_mut().insert(fields);
    }
    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        if event.metadata().target() == "celox::parallel::call"
            && !self.window.running.load(Ordering::Relaxed)
        {
            return;
        }
        let mut fields = Fields(serde_json::Map::new(), self.omit_sir);
        event.record(&mut fields);
        if event.metadata().target() == "celox::parallel::call" {
            if !self.window.running.load(Ordering::Relaxed) {
                return;
            }
            let call = self.window.calls.fetch_add(1, Ordering::Relaxed);
            let capture =
                call >= self.window.after && call < self.window.after + self.window.length;
            self.window.active.store(capture, Ordering::Relaxed);
            if !capture {
                return;
            }
        }
        if self.window.running.load(Ordering::Relaxed) {
            fields.0.insert(
                "kernel_call".into(),
                json!(self.window.calls.load(Ordering::Relaxed)),
            );
        }
        if let Some(scope) = ctx.event_scope(event) {
            let scopes = scope
                .from_root()
                .filter_map(|span| {
                    span.extensions()
                        .get::<Fields>()
                        .map(|v| Value::Object(v.0.clone()))
                })
                .collect::<Vec<_>>();
            fields.0.insert("scopes".into(), json!(scopes));
        }
        let mut events = self.events.lock().unwrap();
        if events.len().is_multiple_of(100_000) {
            eprintln!(
                "Captured {} records; event {}",
                events.len(),
                fields.0.get("event").unwrap_or(&Value::Null)
            );
        }
        events.push(Value::Object(fields.0));
    }
}

fn demo(stages: u32) -> String {
    let mut code = String::from(
        "module Worker (clk: input clock, seed: input logic<64>, result: output logic<64>) {\nvar state: logic<64>;\nalways_ff { state = result + 64'd1; }\nlet x0: logic<64> = state ^ seed;\n",
    );
    for i in 1..=stages {
        code += &format!(
            "let x{i}: logic<64> = ((x{} << 7) ^ (x{} >> 3)) + 64'd{};\n",
            i - 1,
            i - 1,
            i * 2 + 1
        );
    }
    code += &format!(
        "assign result = x{stages};\n}}\nmodule Top (clk: input clock, seed: input logic<64>,\n"
    );
    for i in 0..16 {
        code += &format!("o{i}: output logic<64>,\n");
    }
    code += ") {\n";
    for i in 0..16 {
        code += &format!("inst worker_{i}: Worker (clk, seed, result: o{i});\n");
    }
    code + "}\n"
}

type Error = Box<dyn std::error::Error>;

fn exercise<B: SimBackend>(
    args: &Args,
    mut parallel: Simulator<B>,
    mut sequential: Simulator<B>,
    active: &AtomicBool,
    outputs: &[String],
) -> Result<Value, Error> {
    active.store(true, Ordering::Relaxed);
    for address in parallel.program().design.state_objects.keys() {
        let name = parallel.program().design.get_path(address);
        if let Some(variable) = parallel.program().design.variable(address) {
            tracing::trace!(target: "celox::parallel", event = "source_signal", address = %address,
                name, instance = address.instance_id.0, object = address.var_id.0,
                source_address = format!("AbsoluteAddr({}, {})", address.instance_id, variable.source_id),
                source_object = variable.source_id.0);
        }
    }

    active.store(false, Ordering::Relaxed);
    let pclk = parallel.try_event(&args.clock)?;
    let sclk = sequential.try_event(&args.clock)?;
    for (name, value) in &args.inputs {
        let p = parallel.try_signal(name)?;
        let s = sequential.try_signal(name)?;
        parallel.modify(|io| io.set(p, *value))?;
        sequential.modify(|io| io.set(s, *value))?;
    }
    let signals = outputs
        .iter()
        .map(|name| Ok((parallel.try_signal(name)?, sequential.try_signal(name)?)))
        .collect::<Result<Vec<_>, Error>>()?;
    active.store(false, Ordering::Relaxed);
    for _ in 0..args.warmup {
        parallel.tick(pclk)?;
        sequential.tick(sclk)?;
    }
    // Alternate benchmark blocks, avoiding a systematic first-run advantage.
    let mut pn = Vec::new();
    let mut sn = Vec::new();
    for round in 0..6 {
        for is_parallel in if round % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let start = Instant::now();
            for _ in 0..args.cycles {
                if is_parallel {
                    parallel.tick(pclk)?;
                } else {
                    sequential.tick(sclk)?;
                }
            }
            let ns = start.elapsed().as_nanos() as f64 / f64::from(args.cycles);
            if is_parallel {
                pn.push(ns);
            } else {
                sn.push(ns);
            }
        }
    }
    let median = |v: &mut [f64]| {
        v.sort_by(f64::total_cmp);
        (v[2] + v[3]) / 2.0
    };
    let parallel_ns = median(&mut pn);
    let sequential_ns = median(&mut sn);
    let mut checks = 0;
    for cycle in 0..args.capture {
        active.store(true, Ordering::Relaxed);
        tracing::trace!(target: "celox::parallel", event = "capture_cycle", cycle);
        parallel.tick(pclk)?;
        active.store(false, Ordering::Relaxed);
        sequential.tick(sclk)?;
        for (index, &(p, s)) in signals.iter().enumerate() {
            let actual = parallel.get(p);
            let expected = sequential.get(s);
            if actual != expected {
                return Err(format!(
                    "cycle {cycle}, {}: parallel {actual}, sequential {expected}",
                    outputs[index]
                )
                .into());
            }
            checks += 1;
        }
    }
    Ok(
        json!({"parallel_ns_per_tick":parallel_ns,"sequential_ns_per_tick":sequential_ns,"speedup":sequential_ns/parallel_ns,
        "parallel_samples":pn,"sequential_samples":sn,"checks":checks,"observed":outputs,
        "warning":"Task tracing perturbs execution. Speedup uses six untraced blocks after warmup; overlap is wall-clock overlap, not proof of simultaneous CPU residency. Verification covers only the listed signals and stimulus."}),
    )
}

fn exercise_testbench<B: SimBackend>(
    args: &Args,
    mut sim: Simulator<B>,
    ticks: u64,
    window: &Window,
) -> Result<Value, Error> {
    for address in sim.program().design.state_objects.keys() {
        let name = sim.program().design.get_path(address);
        if let Some(variable) = sim.program().design.variable(address) {
            tracing::trace!(target: "celox::parallel", event = "source_signal", address = %address,
                name, instance = address.instance_id.0, object = address.var_id.0,
                source_address = format!("AbsoluteAddr({}, {})", address.instance_id, variable.source_id),
                source_object = variable.source_id.0);
        }
    }
    let tb =
        celox::testbench::compile_initial_testbench(&sim).ok_or("No initial testbench found")?;
    window.active.store(false, Ordering::Relaxed);
    window.running.store(true, Ordering::Relaxed);
    eprintln!(
        "Running {} for up to {ticks} ticks; capture kernel calls {}..{}",
        args.top,
        window.after + 1,
        window.after + window.length
    );
    let start = Instant::now();
    let result = celox::testbench::run_compiled_testbench_with_tick_limit(&mut sim, &tb, ticks);
    let elapsed = start.elapsed().as_secs_f64();
    window.running.store(false, Ordering::Relaxed);
    window.active.store(false, Ordering::Relaxed);
    if let celox::testbench::TestResult::Fail(message) = &result.result {
        return Err(message.clone().into());
    }
    let mut final_signals = BTreeMap::new();
    for name in &args.outputs {
        let signal = sim.try_signal(name)?;
        final_signals.insert(name, format!("0x{:x}", sim.get(signal)));
    }
    Ok(
        json!({"mode":"testbench", "final_signals":final_signals, "top":args.top, "ticks":result.ticks,
        "tick_limit_reached":result.tick_limit_reached, "elapsed_seconds":elapsed,
        "kernel_calls":window.calls.load(Ordering::Relaxed), "capture_after_calls":window.after,
        "capture_calls":window.length, "checks":0,
        "warning":"Real initial testbench with original firmware and stimulus. Bounded run, not a completed Linux boot or a sequential-equivalence check. Capture coordinates count selected-kernel calls, not clock cycles. Traced calls perturb timing; this report does not claim a speedup. Automatic fused sequential execution may bypass kernel calls."}),
    )
}

fn main() -> Result<(), Error> {
    let mut args = Args::parse();
    if let Some(project) = &args.project {
        args.project = Some(fs::canonicalize(project)?);
        args.output = std::path::absolute(&args.output)?;
    }
    let project_sources = if let Some(project) = &args.project {
        let mut metadata =
            veryl_metadata::Metadata::load(&veryl_metadata::Metadata::search_from(project)?)?;
        let paths = if args.sources.is_empty() {
            metadata
                .paths::<&str>(&[], false, false)?
                .into_iter()
                .filter(|p| !p.example)
                .map(|p| p.src)
                .collect::<Vec<_>>()
        } else {
            args.sources
                .iter()
                .map(|p| {
                    if p.is_absolute() {
                        p.clone()
                    } else {
                        project.join(p)
                    }
                })
                .collect()
        };
        let sources = paths
            .into_iter()
            .map(|p| Ok((fs::read_to_string(&p)?, p)))
            .collect::<Result<Vec<_>, std::io::Error>>()?;
        Some((sources, metadata))
    } else {
        None
    };
    // HDL memory images are project-relative, as in the existing Heliodor
    // runner launched from its project directory. Preserve the output location.
    if let Some(project) = &args.project {
        std::env::set_current_dir(project)?;
    }
    let source = if let Some((sources, _)) = &project_sources {
        sources
            .iter()
            .map(|(s, p)| format!("// {}\n{s}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    } else if args.sources.is_empty() {
        demo(args.demo_stages)
    } else {
        args.sources
            .iter()
            .map(fs::read_to_string)
            .collect::<Result<Vec<_>, _>>()?
            .join("\n")
    };
    let outputs = if args.project.is_none() && args.sources.is_empty() && args.outputs.is_empty() {
        (0..16).map(|i| format!("o{i}")).collect()
    } else {
        args.outputs.clone()
    };
    let window = Arc::new(Window {
        active: AtomicBool::new(true),
        after: u64::from(args.warmup),
        length: u64::from(args.capture),
        ..Window::default()
    });
    let active = &window.active;
    let events = Arc::new(Mutex::new(Vec::new()));
    let enabled = window.clone();
    tracing_subscriber::registry()
        .with(Capture {
            events: events.clone(),
            window: window.clone(),
            omit_sir: args.omit_sir,
        })
        .with(tracing_subscriber::filter::dynamic_filter_fn(
            move |metadata, _| enabled.enables(metadata),
        ))
        .init();
    let builder = || {
        let b = if let Some((sources, metadata)) = &project_sources {
            Simulator::from_sources(
                sources
                    .iter()
                    .map(|(s, p)| (s.as_str(), p.as_path()))
                    .collect(),
                &args.top,
            )
            .with_metadata(metadata.clone())
        } else {
            Simulator::builder(&source, &args.top)
        }
        .threads(args.threads as usize);
        if args.always {
            b.parallel_partition(celox::ParallelPartition::Always)
        } else {
            b
        }
    };
    let summary = if let Some(ticks) = args.testbench_ticks {
        match args.backend {
            Backend::Native => {
                exercise_testbench(&args, builder().build_native()?, ticks, &window)?
            }
            Backend::Cranelift => {
                exercise_testbench(&args, builder().build_cranelift()?, ticks, &window)?
            }
        }
    } else {
        match args.backend {
            Backend::Native => {
                let p = builder().build_native()?;
                active.store(false, Ordering::Relaxed);
                let s = Simulator::builder(&source, &args.top).build_native()?;
                exercise(&args, p, s, active, &outputs)?
            }
            Backend::Cranelift => {
                let p = builder().build_cranelift()?;
                active.store(false, Ordering::Relaxed);
                let s = Simulator::builder(&source, &args.top).build_cranelift()?;
                exercise(&args, p, s, active, &outputs)?
            }
        }
    };
    let events = events.lock().unwrap();
    let mut counts = BTreeMap::new();
    for event in events.iter() {
        *counts
            .entry(event["event"].as_str().unwrap_or("unknown"))
            .or_insert(0usize) += 1;
    }
    let report = json!({"schema":1,"omit_sir":args.omit_sir,"debug_build":cfg!(debug_assertions),"backend":format!("{:?}", args.backend),"threads":args.threads,"forced":args.always,"warmup":args.warmup,"cycles_per_block":args.cycles,"source":source,"summary":summary,"counts":counts,"events":*events});
    fs::create_dir_all(&args.output)?;
    fs::write(
        args.output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    // Escape HTML's script terminator even for source text supplied by the user.
    let payload = serde_json::to_string(&report)?.replace('<', "\\u003c");
    let html = include_str!("parallel-report/report.html")
        .replace(
            "__REPORT_JS__",
            include_str!("../../../../scripts/parallel-report.mjs"),
        )
        .replace("__REPORT_JSON__", &payload);
    fs::write(args.output.join("report.html"), html)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    println!("Report: {}", args.output.join("report.html").display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_window_opens_before_work_and_closes_at_its_limit() {
        let window = Arc::new(Window {
            running: AtomicBool::new(true),
            after: 2,
            length: 2,
            ..Window::default()
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let enabled = window.clone();
        let subscriber = tracing_subscriber::registry()
            .with(Capture {
                events: events.clone(),
                window: window.clone(),
                omit_sir: false,
            })
            .with(tracing_subscriber::filter::dynamic_filter_fn(
                move |metadata, _| enabled.enables(metadata),
            ));
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..6 {
                tracing::trace!(target: "celox::parallel::call", event = "kernel_call");
                tracing::trace!(target: "celox::parallel", event = "task_run", task = 0);
            }
        });
        let events = events.lock().unwrap();
        let calls = events
            .iter()
            .filter(|e| e["event"] == "task_run")
            .map(|e| e["kernel_call"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(calls, [3, 4]);
        assert!(!window.active.load(Ordering::Relaxed));
    }

    #[test]
    fn call_markers_are_disabled_before_field_evaluation_outside_testbench_capture() {
        let window = Arc::new(Window::default());
        let enabled = window.clone();
        let subscriber = tracing_subscriber::registry()
            .with(Capture {
                events: Arc::new(Mutex::new(Vec::new())),
                window: window.clone(),
                omit_sir: true,
            })
            .with(tracing_subscriber::filter::dynamic_filter_fn(
                move |metadata, _| enabled.enables(metadata),
            ));
        let evaluated = AtomicU64::new(0);
        tracing::subscriber::with_default(subscriber, || {
            for running in [false, true, false] {
                window.running.store(running, Ordering::Relaxed);
                tracing::trace!(target: "celox::parallel::call", event = "kernel_call",
                    probe = evaluated.fetch_add(1, Ordering::Relaxed));
            }
        });
        assert_eq!(evaluated.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn multiple_programs_keep_compiler_and_runtime_diagnostics_connected() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Capture {
            events: events.clone(),
            window: Arc::new(Window::default()),
            omit_sir: true,
        });
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..2 {
                let source = demo(4);
                let builder = || {
                    Simulator::builder(&source, "Top")
                        .threads(4)
                        .parallel_partition(celox::ParallelPartition::Always)
                };
                let mut native = builder().build_native().unwrap();
                let clock = native.try_event("clk").unwrap();
                native.tick(clock).unwrap();
                let mut cranelift = builder().build_cranelift().unwrap();
                let clock = cranelift.try_event("clk").unwrap();
                cranelift.tick(clock).unwrap();
            }
        });
        let events = events.lock().unwrap();
        let tasks = events
            .iter()
            .filter(|e| e["event"] == "compiled_task")
            .collect::<Vec<_>>();
        let programs = tasks
            .iter()
            .map(|e| e["program"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(programs.len(), 4);
        let dispatches = events
            .iter()
            .filter(|e| e["event"] == "dispatch")
            .collect::<Vec<_>>();
        assert!(!dispatches.is_empty());
        let pools = dispatches
            .iter()
            .map(|e| e["pool"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(pools.len(), 4);
        for dispatch in dispatches {
            let metadata = tasks
                .iter()
                .filter(|t| t["schedule"] == dispatch["schedule"])
                .collect::<Vec<_>>();
            assert!(!metadata.is_empty());
            assert!(
                metadata
                    .iter()
                    .all(|t| t["program"] == metadata[0]["program"])
            );
        }
        let native_sir = events
            .iter()
            .filter(|e| e["event"] == "native_task_sir")
            .collect::<Vec<_>>();
        assert!(!native_sir.is_empty());
        for sir in native_sir {
            assert!(tasks.iter().any(|t| t["schedule"] == sir["schedule"]
                && t["program"] == sir["program"]
                && t["task"] == sir["task"]));
        }
        for level in ["units", "tasks"] {
            assert!(events.iter().any(|e| {
                e["event"] == "hazard"
                    && e["scopes"].as_array().is_some_and(|scopes| {
                        scopes.iter().any(|scope| {
                            scope["hazard_level"] == level
                                && tasks.iter().any(|task| {
                                    task["schedule"] == scope["schedule"]
                                        && task["hazard_level"] == level
                                })
                        })
                    })
            }));
        }
    }

    #[test]
    fn project_mode_requires_a_bounded_testbench() {
        assert!(Args::try_parse_from(["parallel-report", "--project", "."]).is_err());
        assert!(Args::try_parse_from(["parallel-report", "--testbench-ticks", "4"]).is_err());
        assert!(
            Args::try_parse_from([
                "parallel-report",
                "--project",
                ".",
                "--testbench-ticks",
                "4"
            ])
            .is_ok()
        );
    }
}
