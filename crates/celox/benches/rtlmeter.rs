//! RTLMeter designs through the `celox` command, on CodSpeed's walltime
//! instrument.
//!
//! `scripts/run-rtlmeter-bench.py codspeed-layout SOURCE DIR` lays out the
//! cases of `benchmarks/rtlmeter/codspeed_manifest.json`; this benchmark runs
//! the cases in `CELOX_RTLMETER_LAYOUT=DIR` (none without it). Each run is
//! one `celox run --backend native -O O2 --two-state --threads 1 --report`
//! process that simulates a fixed window (`+max_cycles`), and reports two
//! benchmarks from the times the command records (`CELOX_TEST_TIMING`):
//!
//! - `rtlmeter/<case>/compile`: analysis and code generation (`compile_ns`);
//! - `rtlmeter/<case>/cycle`: the window's execution (`execute_ns`, which
//!   includes the testbench's start-up such as program loading) divided by
//!   its clock cycles.
//!
//! Both come from the same runs, so the compile time of a multi-minute
//! design is paid once per sample, not once per benchmark. The celox binary
//! is the one Cargo builds next to this benchmark, or `CELOX_RTLMETER_BIN`.
//!
//! A run whose resident memory passes `CELOX_RTLMETER_MEMORY_LIMIT_MIB`
//! (default [`MEMORY_LIMIT_MIB`]) or whose time passes
//! `CELOX_RTLMETER_TIMEOUT_SECS` (default [`TIMEOUT_SECS`]) is killed and
//! fails its benchmark, so one case cannot take the runner down. Each run
//! prints its start, peak memory and times, and leaves its output in
//! `<case>/run/celox.log`.

// The layout and binary come from the environment, and CodSpeed's harness
// reads its own.
#![allow(clippy::disallowed_methods)]

use std::{
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant},
};

/// Resident memory a run may use. CodSpeed's ARM64 Macro Runner has 32 GB;
/// VeeR-EH1 peaks at about 7 GB.
const MEMORY_LIMIT_MIB: u64 = 20 * 1024;
/// Time a run may take (a BlackParrot compile takes several minutes).
const TIMEOUT_SECS: u64 = 60 * 60;

use codspeed_criterion_compat::{
    Criterion, IterManualOptions,
    measurement::{Measurement, ValueFormatter, WallTime},
};

/// A duration the measured routine reports instead of the wall time around
/// it: the routine sends it, the measurement of its round receives it.
struct Reported {
    values: Receiver<Duration>,
    wall_time: WallTime,
}

impl Measurement for Reported {
    type Intermediate = ();
    type Value = Duration;

    fn start(&self) {
        // Drop values of unmeasured (warm-up) rounds.
        while self.values.try_recv().is_ok() {}
    }

    fn end(&self, (): ()) -> Duration {
        self.values.try_iter().sum()
    }

    fn add(&self, v1: &Duration, v2: &Duration) -> Duration {
        *v1 + *v2
    }

    fn zero(&self) -> Duration {
        Duration::ZERO
    }

    fn to_f64(&self, value: &Duration) -> f64 {
        value.as_nanos() as f64
    }

    fn formatter(&self) -> &dyn ValueFormatter {
        self.wall_time.formatter()
    }
}

/// A case written by `codspeed-layout`.
struct Case {
    name: String,
    /// Holds `case.json`; the process runs in its `run` directory.
    directory: PathBuf,
    args: Vec<String>,
    window: u64,
    rounds: u64,
}

impl Case {
    fn load(directory: PathBuf) -> Self {
        let path = directory.join("case.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let json: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let field = |key: &str| {
            json.get(key)
                .unwrap_or_else(|| panic!("{}: missing {key}", path.display()))
        };
        Self {
            name: field("name")
                .as_str()
                .expect("name is a string")
                .to_string(),
            args: field("args")
                .as_array()
                .expect("args is an array")
                .iter()
                .map(|arg| arg.as_str().expect("args are strings").to_string())
                .collect(),
            window: field("window").as_u64().expect("window is a count"),
            rounds: field("rounds").as_u64().expect("rounds is a count"),
            directory,
        }
    }
}

/// The times one run of a case recorded.
struct Run {
    compile: Duration,
    execute: Duration,
    cycles: u64,
}

impl Run {
    fn per_cycle(&self) -> Duration {
        self.execute / u32::try_from(self.cycles).expect("window fits u32")
    }
}

/// Output that a passing window never prints (as `accept` in
/// `scripts/run-rtlmeter-bench.py`).
const FAILURES: &[&str] = &[
    "TEST FAIL",
    "TEST_FAIL",
    "Time Out !!!",
    "simulation finished with error",
    "%Error:",
    "$fatal",
];

fn failed(name: &str, status: &ExitStatus, output: &str, reason: &str) -> ! {
    let lines: Vec<&str> = output.lines().collect();
    let tail = lines[lines.len().saturating_sub(40)..].join("\n");
    panic!("{name}: {reason} ({status}); last output:\n{tail}")
}

/// Limits of one run of the `celox` command.
struct Limits {
    memory_kib: u64,
    timeout: Duration,
}

impl Limits {
    fn from_env() -> Self {
        let number = |key: &str, default: u64| {
            std::env::var(key)
                .ok()
                .map(|value| {
                    value
                        .parse::<u64>()
                        .unwrap_or_else(|_| panic!("{key}={value} is not a number"))
                })
                .unwrap_or(default)
        };
        Self {
            memory_kib: number("CELOX_RTLMETER_MEMORY_LIMIT_MIB", MEMORY_LIMIT_MIB) * 1024,
            timeout: Duration::from_secs(number("CELOX_RTLMETER_TIMEOUT_SECS", TIMEOUT_SECS)),
        }
    }
}

/// The resident memory of process `pid` in KiB: (current, peak).
fn resident_kib(pid: u32) -> Option<(u64, u64)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = |key: &str| {
        status.lines().find_map(|line| {
            line.strip_prefix(key)?
                .trim()
                .strip_suffix("kB")?
                .trim()
                .parse::<u64>()
                .ok()
        })
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

/// Run `command` to its end, killing it past `limits`. Returns its status,
/// its peak resident memory in KiB and why it was killed, if it was.
fn run_limited(command: &mut Command, limits: &Limits) -> (ExitStatus, u64, Option<String>) {
    let mut child = command.spawn().expect("celox starts");
    let start = Instant::now();
    let mut peak = 0;
    let mut killed = None;
    loop {
        if let Some(status) = child.try_wait().expect("celox is waitable") {
            return (status, peak, killed);
        }
        if killed.is_none() {
            if let Some((current, high)) = resident_kib(child.id()) {
                peak = peak.max(high);
                if current > limits.memory_kib {
                    killed = Some(format!(
                        "killed at {} MiB resident, over the {} MiB limit",
                        current / 1024,
                        limits.memory_kib / 1024
                    ));
                }
            }
            if start.elapsed() > limits.timeout {
                killed = Some(format!("killed after {:?}", limits.timeout));
            }
            if killed.is_some() {
                let _ = child.kill();
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run_case(binary: &Path, case: &Case, limits: &Limits) -> Run {
    let run_directory = case.directory.join("run");
    let cycles_file = run_directory.join("_rtlmeter_cycles.txt");
    let _ = std::fs::remove_file(&cycles_file);
    let log = run_directory.join("celox.log");
    let log_file = std::fs::File::create(&log).expect("celox log is writable");
    let mut command = Command::new(binary);
    command
        .args(&case.args)
        .current_dir(&run_directory)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone().expect("celox log handle"))
        .stderr(log_file);
    // No caller-specific simulator settings or extra workers.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CELOX_") {
            command.env_remove(key);
        }
    }
    command
        .env("RAYON_NUM_THREADS", "1")
        .env("RUST_LOG", "warn");
    println!("{}: run starts", case.name);
    let (status, peak_kib, killed) = run_limited(&mut command, limits);
    let text = String::from_utf8_lossy(&std::fs::read(&log).unwrap_or_default()).into_owned();
    macro_rules! fail {
        ($($reason:tt)*) => {
            failed(&case.name, &status, &text, &format!($($reason)*))
        };
    }
    println!(
        "{}: run ends ({status}), peak {} MiB resident",
        case.name,
        peak_kib / 1024
    );
    if let Some(reason) = killed {
        fail!("{reason}");
    }
    if !status.success() {
        fail!("celox failed");
    }
    if let Some(marker) = FAILURES.iter().find(|marker| text.contains(**marker)) {
        fail!("the testbench reported a failure ({marker})");
    }
    if !text.contains("RTLMeter: +max_cycles reached, exiting") {
        fail!("the run ended before its window");
    }
    let cycles = std::fs::read_to_string(&cycles_file)
        .ok()
        .and_then(|count| count.trim().parse::<u64>().ok())
        .unwrap_or_else(|| fail!("no RTLMeter cycle count"));
    if cycles != case.window {
        fail!("ran {cycles} cycles, not {}", case.window);
    }
    let timing = text
        .lines()
        .find(|line| line.starts_with("CELOX_TEST_TIMING "))
        .unwrap_or_else(|| fail!("no CELOX_TEST_TIMING record"));
    let field = |key: &str| {
        timing
            .split_whitespace()
            .find_map(|entry| entry.strip_prefix(key)?.strip_prefix('='))
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_nanos)
            .unwrap_or_else(|| fail!("no {key} in {timing}"))
    };
    let run = Run {
        compile: field("compile_ns"),
        execute: field("execute_ns"),
        cycles,
    };
    println!(
        "{}: compile {:.2} s, {cycles} cycles in {:.2} s ({:.2} us/cycle)",
        case.name,
        run.compile.as_secs_f64(),
        run.execute.as_secs_f64(),
        run.per_cycle().as_secs_f64() * 1e6,
    );
    run
}

fn benchmark_rtlmeter(c: &mut Criterion<Reported>, reports: &Sender<Duration>) {
    let Some(layout) = std::env::var_os("CELOX_RTLMETER_LAYOUT").map(PathBuf::from) else {
        println!("CELOX_RTLMETER_LAYOUT is not set; no RTLMeter case to run");
        return;
    };
    let limits = Limits::from_env();
    let binary = std::env::var_os("CELOX_RTLMETER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_celox")));
    let mut directories: Vec<PathBuf> = std::fs::read_dir(&layout)
        .unwrap_or_else(|error| panic!("{}: {error}", layout.display()))
        .map(|entry| entry.expect("layout entry").path())
        .filter(|path| path.join("case.json").is_file())
        .collect();
    directories.sort();
    assert!(
        !directories.is_empty(),
        "{} holds no case",
        layout.display()
    );
    for case in directories.into_iter().map(Case::load) {
        let rounds = IterManualOptions::new().rounds(case.rounds);
        let mut runs = Vec::new();
        c.bench_function(&format!("rtlmeter/{}/compile", case.name), |b| {
            b.iter_manual_unstable(rounds, || {
                let run = run_case(&binary, &case, &limits);
                reports.send(run.compile).expect("measurement receives");
                runs.push(run);
            });
        });
        c.bench_function(&format!("rtlmeter/{}/cycle", case.name), |b| {
            b.iter_manual_unstable(rounds, || {
                // A filter that skips the compile benchmark leaves no run.
                let run = runs
                    .pop()
                    .unwrap_or_else(|| run_case(&binary, &case, &limits));
                reports.send(run.per_cycle()).expect("measurement receives");
            });
        });
    }
}

// `criterion_group!` gives its targets no way to reach the measurement, so
// this is its expansion with the sender passed along.
fn main() {
    let (reports, values) = channel();
    let mut criterion = Criterion::default()
        .with_measurement(Reported {
            values,
            wall_time: WallTime,
        })
        .configure_from_args();
    if std::env::var("CODSPEED_ENV").is_ok() {
        criterion.set_current_file(codspeed_criterion_compat::abs_file!());
        criterion.set_macro_group("rtlmeter::benchmark_rtlmeter");
    }
    benchmark_rtlmeter(&mut criterion, &reports);
    Criterion::default().configure_from_args().final_summary();
}
