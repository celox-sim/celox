#![allow(clippy::disallowed_macros)] // CLI errors and progress intentionally use stderr

//! Run a Rica ELF on the `HardwareCoreTestTop` board-memory model with Celox
//! and report split benchmark timing.
//!
//! The simulation loop mirrors `verify-riscv --hardware-memory-rtl` from the
//! Rica repository: the CPU clock ticks every cycle and the memory clock ticks
//! every `cpu_hz / memory_hz` cycles, with the host sampling the data port
//! before each CPU edge to observe the `tohost` mailbox. `--model timed`
//! mirrors `--timed-clocks`, which evaluates coincident edges atomically with
//! the timed scheduler.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use celox::{
    DeadStorePolicy, NativeBackend, NativeProgramImage, OptLevel, SignalRef, Simulation, Simulator,
    SimulatorBuilder, SirPass,
};
use clap::{Parser, ValueEnum};
use object::{Object, ObjectSegment, ObjectSymbol};

#[derive(Parser)]
#[command(about = "Run a Rica ELF on the Celox board-memory model and report split timing")]
struct Cli {
    /// Rica `riscv/` project directory (contains `src/` and `platform-user.json`).
    #[arg(long)]
    project: PathBuf,
    /// RV64 ELF files to run, each on a fresh simulator over the same compiled model.
    #[arg(long = "elf", required = true)]
    elfs: Vec<PathBuf>,
    #[arg(long, value_enum, ignore_case = true, default_value = "o2")]
    opt_level: OptimizationLevel,
    #[arg(long = "sir-pass", value_parser = parse_pass_override)]
    pass_overrides: Vec<(bool, SirPass)>,
    #[arg(long, value_enum, default_value = "preserve-top-ports")]
    dead_store_policy: DeadStore,
    #[arg(long, value_enum, default_value = "coincident")]
    model: Model,
    /// Serialization of coincident CPU/memory edges in the `board` model.
    #[arg(long, value_enum, default_value = "mem-then-cpu")]
    coincident_order: CoincidentOrder,
    /// Host-side signal access style of the native-simulator models:
    /// pre-resolved handles with fixed-width reads, or Rica's current name
    /// lookup with `BigUint` reads. The timed model always reads
    /// pre-resolved handles through `Simulation::get`.
    #[arg(long, value_enum, default_value = "resolved")]
    harness: Harness,
    #[arg(long, default_value_t = 2_000_000)]
    max_cycles: usize,
    /// Execute every ELF this many times on the same compiled model.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    repeat: u32,
    #[arg(long)]
    compile_only: bool,
    /// Measure time inside generated native functions. Reading the clock
    /// around every call costs a few percent of execution time.
    #[arg(long)]
    jit_timing: bool,
    /// Write the post-optimized SIR, native SIR, MIR and state-layout
    /// analysis of the board model to this directory.
    #[arg(long)]
    dump_ir_dir: Option<PathBuf>,
    /// Native block from a perf profile (`function:block:samples`) to analyze
    /// in the state-layout dump; repeatable.
    #[arg(long = "native-profile-block", value_parser = parse_native_profile_block)]
    native_profile_blocks: Vec<celox::NativeProfileBlock>,
    /// Disable the memory-side features that the working profile enables.
    #[arg(long)]
    no_cache: bool,
    #[arg(long, default_value_t = 8)]
    predictor_config: u64,
    /// Enable the JTAG console (`--machine-console`).
    #[arg(long)]
    machine_console: bool,
    /// Enable the A extension (`--a-extension`).
    #[arg(long)]
    a_extension: bool,
    /// Enable U mode and read the clocks from `platform-user.json` (`--u-mode`).
    #[arg(long)]
    u_mode: bool,
    /// Enable S mode and Sv39; implies `--u-mode` (`--sv39`).
    #[arg(long)]
    sv39: bool,
}

impl Cli {
    fn enable_s(&self) -> bool {
        self.sv39
    }

    fn enable_u(&self) -> bool {
        self.u_mode || self.sv39
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum OptimizationLevel {
    O0,
    O1,
    O2,
}

#[derive(Clone, Copy, ValueEnum)]
enum DeadStore {
    Off,
    PreserveTopPorts,
    PreserveAllPorts,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Model {
    /// Manual tick loop; the shared CPU/memory edge is one `tick_events` call.
    Coincident,
    /// Manual `tick` loop with serialized coincident edges (`--hardware-memory-rtl`).
    Board,
    /// Timed scheduler with atomic coincident edges (`--timed-clocks`).
    Timed,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum CoincidentOrder {
    MemThenCpu,
    CpuThenMem,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Harness {
    Resolved,
    Rica,
}

#[derive(Debug, thiserror::Error)]
enum RicaError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Object(#[from] object::Error),
    #[error("Celox build failed: {0}")]
    Build(#[from] celox::SimulatorError),
    #[error("runtime error: {0}")]
    Runtime(#[from] celox::RuntimeErrorCode),
    #[error("{0}")]
    Message(String),
}

fn message(text: impl Into<String>) -> RicaError {
    RicaError::Message(text.into())
}

fn parse_native_profile_block(value: &str) -> Result<celox::NativeProfileBlock, CliValueError> {
    let invalid = || CliValueError::InvalidProfileBlock {
        value: value.to_owned(),
    };
    let (function, rest) = value.rsplit_once(':').ok_or_else(invalid)?;
    let (function, block) = function.rsplit_once(':').ok_or_else(invalid)?;
    Ok(celox::NativeProfileBlock {
        function: function.to_owned(),
        block: block.parse().map_err(|_| invalid())?,
        samples: rest.parse().map_err(|_| invalid())?,
    })
}

#[derive(Debug, thiserror::Error)]
enum CliValueError {
    #[error("invalid native profile block `{value}`; expected function:block:samples")]
    InvalidProfileBlock { value: String },
    #[error("invalid pass override `{value}`; expected +sir:<pass> or -sir:<pass>")]
    InvalidPassOverride { value: String },
    #[error("unknown SIR pass `{name}`")]
    UnknownSirPass { name: String },
}

fn parse_pass_override(value: &str) -> Result<(bool, SirPass), CliValueError> {
    let (enable, name) = if let Some(name) = value.strip_prefix('+') {
        (true, name)
    } else if let Some(name) = value.strip_prefix('-') {
        (false, name)
    } else {
        return Err(CliValueError::InvalidPassOverride {
            value: value.to_owned(),
        });
    };
    let pass = SirPass::parse(name).ok_or_else(|| CliValueError::UnknownSirPass {
        name: name.to_owned(),
    })?;
    Ok((enable, pass))
}

/// Source files joined in Rica's `CORE_SOURCES` order followed by the
/// board-memory model files.
const CORE_SOURCES: [&str; 32] = [
    "UserPlatform.veryl",
    "PipelineInterfaces.veryl",
    "RegisterFile.veryl",
    "CompressedDecoder.veryl",
    "Decoder.veryl",
    "PerformanceCounter64.veryl",
    "PerformanceEventCounters4.veryl",
    "CsrFile.veryl",
    "PmpCheck.veryl",
    "ExecuteUnit.veryl",
    "BitManipUnit.veryl",
    "MulDivUnit.veryl",
    "DspMultiply64.veryl",
    "ShiftUnit.veryl",
    "ShiftHighUnit.veryl",
    "LoadFormat.veryl",
    "CompletionUnit.veryl",
    "PhysicalAccessCheck.veryl",
    "PhysicalOffsetAccessCheck.veryl",
    "AtomicAlu.veryl",
    "AtomicLsu.veryl",
    "PageTableWalker.veryl",
    "Sv39Translation.veryl",
    "PmpUnit.veryl",
    "FetchQueue.veryl",
    "CommitTrace.veryl",
    "RedirectUnit.veryl",
    "InterruptControl.veryl",
    "FetchCursor.veryl",
    "Core.veryl",
    "StaticBranchPredictor.veryl",
    "CompactBranchPredictor.veryl",
];

/// Internal `HardwareCoreTestTop` variables the host reads. Nothing in the
/// design loads them, so dead store elimination must be told they are live.
const HOST_OBSERVED_VARS: [&str; 3] = [
    "data_store_retire_ready",
    "data_request_sequence",
    "data_response_sequence",
];

const BOARD_SOURCES: [&str; 4] = [
    "InstructionBuffer.veryl",
    "InstructionCache.veryl",
    "HardwareMemory.veryl",
    "HardwareCoreTestTop.veryl",
];

fn load_source(project: &Path) -> Result<String, RicaError> {
    CORE_SOURCES
        .iter()
        .chain(BOARD_SOURCES.iter())
        .map(|file| fs::read_to_string(project.join("src").join(file)).map_err(RicaError::from))
        .collect::<Result<Vec<_>, _>>()
        .map(|sources| sources.join("\n"))
}

#[derive(Clone, Copy, Debug)]
struct Platform {
    cpu_hz: u64,
    memory_hz: u64,
    pmp_data_stages: u64,
    early_alu: bool,
    direct_load_return: bool,
}

impl Platform {
    fn memory_divisor(self) -> usize {
        (self.cpu_hz / self.memory_hz) as usize
    }
}

/// Rica's M-mode profile uses fixed clocks; the U profile reads the board file.
fn load_platform(project: &Path, user: bool) -> Result<Platform, RicaError> {
    if !user {
        return Ok(Platform {
            cpu_hz: 124_800_000,
            memory_hz: 62_400_000,
            pmp_data_stages: 3,
            early_alu: false,
            direct_load_return: false,
        });
    }
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(project.join("platform-user.json"))?)?;
    let integer = |name: &str| {
        record[name]
            .as_u64()
            .ok_or_else(|| message(format!("platform-user.json: invalid integer `{name}`")))
    };
    let flag = |name: &str| {
        record[name]
            .as_bool()
            .ok_or_else(|| message(format!("platform-user.json: invalid flag `{name}`")))
    };
    let platform = Platform {
        cpu_hz: integer("cpu_hz")?,
        memory_hz: integer("memory_hz")?,
        pmp_data_stages: integer("pmp_data_stages")?,
        early_alu: flag("early_alu")?,
        direct_load_return: flag("direct_load_return")?,
    };
    if platform.cpu_hz == 0
        || platform.memory_hz == 0
        || !platform.cpu_hz.is_multiple_of(platform.memory_hz)
    {
        return Err(message(
            "platform-user.json: cpu_hz and memory_hz must be positive and cpu_hz a multiple of memory_hz",
        ));
    }
    Ok(platform)
}

/// Working-profile parameters from Rica's `docs/building-and-simulation.md`.
struct ModelConfig {
    predictor_config: u64,
    icache_index_bits: u64,
    fast_lsu: u64,
}

fn builder<'a, B>(
    builder: SimulatorBuilder<'a, B>,
    cli: &Cli,
    platform: Platform,
    config: &ModelConfig,
) -> SimulatorBuilder<'a, B> {
    let mut builder = builder
        .param("PMP_DATA_STAGES", platform.pmp_data_stages)
        .param("EARLY_ALU", u64::from(platform.early_alu))
        .param("DIRECT_LOAD_RETURN", u64::from(platform.direct_load_return))
        .param("MEMORY_HZ", platform.memory_hz)
        .param("PREDICTOR_MODE", config.predictor_config / 2)
        .param("ICACHE_INDEX_BITS", config.icache_index_bits)
        .param("FAST_LSU", config.fast_lsu)
        .param("ENABLE_M", 1)
        .param("M_USE_DSP", 1)
        .param("ENABLE_ZBA", 1)
        .param("ENABLE_ZBB", 1)
        .param("ENABLE_ZBS", 1)
        .param("EARLY_BITMANIP", 1)
        .param("EARLY_TAKEN_FETCH", 1)
        .param("EARLY_DIVIDE", 1)
        .param("ENABLE_INTERRUPTS", 1)
        .param("STRICT_MEMORY", 1)
        .param("ENABLE_A", u64::from(cli.a_extension))
        .param("ENABLE_U", u64::from(cli.enable_u()))
        .param("ENABLE_S", u64::from(cli.enable_s()))
        .param("ENABLE_SV39", u64::from(cli.sv39))
        .param("ENABLE_CONSOLE", u64::from(cli.machine_console))
        .opt_level(match cli.opt_level {
            OptimizationLevel::O0 => OptLevel::O0,
            OptimizationLevel::O1 => OptLevel::O1,
            OptimizationLevel::O2 => OptLevel::O2,
        })
        .dead_store_policy(match cli.dead_store_policy {
            DeadStore::Off => DeadStorePolicy::Off,
            DeadStore::PreserveTopPorts => DeadStorePolicy::PreserveTopPorts,
            DeadStore::PreserveAllPorts => DeadStorePolicy::PreserveAllPorts,
        })
        .diagnostics_from_env();
    for var in HOST_OBSERVED_VARS {
        builder = builder.live_signal(Vec::new(), vec![var.to_string()]);
    }
    for block in &cli.native_profile_blocks {
        builder = builder.trace_native_profile_block(&block.function, block.block, block.samples);
    }
    for (enable, pass) in &cli.pass_overrides {
        builder = if *enable {
            builder.enable_pass(*pass)
        } else {
            builder.disable_pass(*pass)
        };
    }
    builder
}

/// Addresses the board model's debug loader can write: the reset trampoline
/// and the RAM window.
const BOARD_RANGES: [std::ops::Range<u64>; 2] = [0..0x1000, 0x8000_0000..0x8003_f000];

struct ElfImage {
    memory: BTreeMap<u64, u8>,
    tohost: u64,
    name: String,
}

fn reset_trampoline(entry: u64) -> Result<Vec<u32>, RicaError> {
    if entry < 0x7fff_f800 {
        let entry = entry as u32;
        let upper = entry.wrapping_add(0x800) & 0xffff_f000;
        let lower = entry.wrapping_sub(upper) & 0xfff;
        return Ok(vec![
            upper | (5 << 7) | 0x37,
            (lower << 20) | (5 << 15) | 0x67,
        ]);
    }
    if entry == 0x8000_0000 {
        return Ok(vec![0x4000_02b7, 0x0052_82b3, 0x0002_8067]);
    }
    Err(message(format!(
        "entry {entry:#x} is outside the reset trampoline's address range"
    )))
}

impl ElfImage {
    fn load(path: &Path) -> Result<Self, RicaError> {
        let bytes = fs::read(path)?;
        let elf = object::File::parse(bytes.as_slice())?;
        if elf.architecture() != object::Architecture::Riscv64 || !elf.is_little_endian() {
            return Err(message(format!(
                "{} is not a little-endian RV64 ELF",
                path.display()
            )));
        }
        let mut memory = BTreeMap::new();
        for segment in elf.segments() {
            let address = segment.address();
            // Check the whole memory image, including a zero-filled tail.
            let end = address
                .checked_add(segment.size())
                .ok_or_else(|| message(format!("{}: segment overflows", path.display())))?;
            if segment.size() != 0
                && !BOARD_RANGES
                    .iter()
                    .any(|range| range.start <= address && end <= range.end)
            {
                return Err(message(format!(
                    "{}: segment {address:#x}..{end:#x} is outside the board memory",
                    path.display()
                )));
            }
            for (offset, byte) in segment.data()?.iter().copied().enumerate() {
                memory.insert(address + offset as u64, byte);
            }
        }
        let entry = elf.entry();
        if entry != 0 {
            for (offset, byte) in reset_trampoline(entry)?
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .enumerate()
            {
                memory.insert(offset as u64, byte);
            }
        }
        let tohost = elf
            .symbols()
            .find(|symbol| symbol.name().ok() == Some("tohost"))
            .ok_or_else(|| message(format!("{} has no tohost symbol", path.display())))?
            .address();
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            memory,
            tohost,
            name,
        })
    }
}

struct Outcome {
    cycles: usize,
    commits: usize,
}

/// Host observation of the data port before each CPU edge and the commit port
/// after it, as in Rica's `verify_hardware_memory_elf`.
trait Harnessed {
    fn read(&mut self, sim: &mut Simulator<NativeBackend>, name: &str, signal: SignalRef) -> u64;
}

struct ResolvedHarness;
struct RicaHarness;

impl Harnessed for ResolvedHarness {
    #[inline]
    fn read(&mut self, sim: &mut Simulator<NativeBackend>, _: &str, signal: SignalRef) -> u64 {
        sim.get_as::<u64>(signal)
    }
}

impl Harnessed for RicaHarness {
    fn read(&mut self, sim: &mut Simulator<NativeBackend>, name: &str, _: SignalRef) -> u64 {
        sim.get(sim.signal(name))
            .to_u64_digits()
            .first()
            .copied()
            .unwrap_or(0)
    }
}

struct Mailbox {
    tohost: u64,
    low: u64,
}

impl Mailbox {
    fn observe(&mut self, address: u64, data: u64, mask: u64) -> Option<u64> {
        if address == self.tohost && mask == 0xff {
            Some(data)
        } else if address == self.tohost && mask == 0x0f {
            self.low = data & 0xffff_ffff;
            None
        } else if address == self.tohost + 4 && mask == 0x0f {
            // The upper word completes a value written as two 32-bit stores.
            Some(self.low | (data & 0xffff_ffff) << 32)
        } else {
            None
        }
    }
}

fn run_board<H: Harnessed>(
    model: &NativeProgramImage,
    image: &ElfImage,
    platform: Platform,
    cli: &Cli,
    mut harness: H,
) -> Result<(Outcome, Duration, Option<Duration>), RicaError> {
    let mut sim =
        SimulatorBuilder::new("", "HardwareCoreTestTop").build_native_from_image(model.clone())?;
    let clk_cpu = sim.event("clk_cpu");
    let clk_mem = sim.event("clk_mem");
    let rst_n = sim.signal("rst_n");
    let run = sim.signal("run");
    let debug_write = sim.signal("debug_write");
    let debug_address = sim.signal("debug_address");
    let debug_data = sim.signal("debug_data");
    sim.modify(|io| {
        io.set(rst_n, 0_u8);
        io.set(run, 0_u8);
        io.set(debug_write, 0_u8);
        io.set(debug_address, 0_u32);
        io.set(debug_data, 0_u8);
    })?;
    sim.tick(clk_cpu)?;
    sim.tick(clk_mem)?;
    sim.modify(|io| io.set(rst_n, 1_u8))?;
    for (address, byte) in &image.memory {
        let address = *address as u32;
        let byte = *byte;
        sim.modify(|io| {
            io.set(debug_write, 1_u8);
            io.set(debug_address, address);
            io.set(debug_data, byte);
        })?;
        sim.tick(clk_mem)?;
    }
    sim.modify(|io| io.set(debug_write, 0_u8))?;
    sim.tick(clk_mem)?;
    sim.modify(|io| io.set(run, 1_u8))?;

    let data_we = sim.signal("data_we");
    let data_store_retire_ready = sim.signal("data_store_retire_ready");
    let data_response_sequence = sim.signal("data_response_sequence");
    let data_request_sequence = sim.signal("data_request_sequence");
    let data_addr = sim.signal("data_addr");
    let data_wdata = sim.signal("data_wdata");
    let data_wmask = sim.signal("data_wmask");
    let commit_valid = sim.signal("commit_valid");
    let commit_pc = sim.signal("commit_pc");
    let commit_instr = sim.signal("commit_instr");
    let commit_rd = sim.signal("commit_rd");
    let commit_data = sim.signal("commit_data");

    let divisor = platform.memory_divisor();
    let mut mailbox = Mailbox {
        tohost: image.tohost,
        low: 0,
    };
    let mut commits = 0_usize;
    let mut trace = std::collections::VecDeque::with_capacity(20);
    if cli.jit_timing {
        sim.start_native_execution_timing();
    }
    let execute_start = Instant::now();
    let mut outcome = None;
    for cycle in 1..=cli.max_cycles {
        let memory_edge = cycle % divisor == 0;
        let serialized = cli.model == Model::Board;
        if serialized && memory_edge && cli.coincident_order == CoincidentOrder::MemThenCpu {
            sim.tick(clk_mem)?;
        }
        let store_acknowledged = harness.read(&mut sim, "data_we", data_we) != 0
            && harness.read(&mut sim, "data_store_retire_ready", data_store_retire_ready) != 0
            && harness.read(&mut sim, "data_response_sequence", data_response_sequence)
                == harness.read(&mut sim, "data_request_sequence", data_request_sequence);
        let store = store_acknowledged.then(|| {
            (
                harness.read(&mut sim, "data_addr", data_addr),
                harness.read(&mut sim, "data_wdata", data_wdata),
                harness.read(&mut sim, "data_wmask", data_wmask),
            )
        });
        if serialized {
            sim.tick(clk_cpu)?;
            if memory_edge && cli.coincident_order == CoincidentOrder::CpuThenMem {
                sim.tick(clk_mem)?;
            }
        } else if memory_edge {
            sim.tick_events(&[clk_cpu, clk_mem])?;
        } else {
            sim.tick(clk_cpu)?;
        }
        if harness.read(&mut sim, "commit_valid", commit_valid) != 0 {
            commits += 1;
            if trace.len() == trace.capacity() {
                trace.pop_front();
            }
            trace.push_back((
                harness.read(&mut sim, "commit_pc", commit_pc),
                harness.read(&mut sim, "commit_instr", commit_instr),
                harness.read(&mut sim, "commit_rd", commit_rd),
                harness.read(&mut sim, "commit_data", commit_data),
            ));
        }
        if let Some(result) =
            store.and_then(|(address, data, mask)| mailbox.observe(address, data, mask))
            && result != 0
        {
            if result != 1 {
                return Err(message(format!(
                    "{}: tohost={result} (test {}) at cycle {cycle}; trace={trace:x?}",
                    image.name,
                    result >> 1
                )));
            }
            outcome = Some(Outcome {
                cycles: cycle,
                commits,
            });
            break;
        }
    }
    let execute_elapsed = execute_start.elapsed();
    let jit_elapsed = cli
        .jit_timing
        .then(|| sim.finish_native_execution_timing())
        .flatten()
        .map(|timing| timing.elapsed());
    let outcome = outcome.ok_or_else(|| {
        message(format!(
            "{} timed out after {} cycles and {commits} commits; trace={trace:x?}",
            image.name, cli.max_cycles
        ))
    })?;
    Ok((outcome, execute_elapsed, jit_elapsed))
}

fn timed_value(sim: &mut Simulation, signal: SignalRef) -> u64 {
    sim.get(signal)
        .to_u64_digits()
        .first()
        .copied()
        .unwrap_or(0)
}

fn timed_edge(sim: &mut Simulation, clocks: &[&str]) -> Result<(), RicaError> {
    let time = sim.time();
    for clock in clocks {
        sim.schedule(clock, time + 1, 0)?;
        sim.schedule(clock, time + 2, 1)?;
    }
    sim.run_until(time + 2)?;
    Ok(())
}

fn run_timed(
    source: &str,
    image: &ElfImage,
    platform: Platform,
    cli: &Cli,
    config: &ModelConfig,
) -> Result<(Outcome, Duration, Duration), RicaError> {
    let compile_start = Instant::now();
    let mut sim = builder(
        Simulation::builder(source, "HardwareCoreTestTop"),
        cli,
        platform,
        config,
    )
    .build()?;
    let compile_elapsed = compile_start.elapsed();
    let rst_n = sim.signal("rst_n");
    let run = sim.signal("run");
    let debug_write = sim.signal("debug_write");
    let debug_address = sim.signal("debug_address");
    let debug_data = sim.signal("debug_data");
    sim.modify(|io| {
        io.set(rst_n, 0_u8);
        io.set(run, 0_u8);
        io.set(debug_write, 0_u8);
        io.set(debug_address, 0_u32);
        io.set(debug_data, 0_u8);
    })?;
    timed_edge(&mut sim, &["clk_cpu", "clk_mem"])?;
    sim.modify(|io| io.set(rst_n, 1_u8))?;
    for (address, byte) in &image.memory {
        let address = *address as u32;
        let byte = *byte;
        sim.modify(|io| {
            io.set(debug_write, 1_u8);
            io.set(debug_address, address);
            io.set(debug_data, byte);
        })?;
        timed_edge(&mut sim, &["clk_mem"])?;
    }
    sim.modify(|io| io.set(debug_write, 0_u8))?;
    timed_edge(&mut sim, &["clk_mem"])?;
    sim.modify(|io| io.set(run, 1_u8))?;

    let data_we = sim.signal("data_we");
    let data_store_retire_ready = sim.signal("data_store_retire_ready");
    let data_response_sequence = sim.signal("data_response_sequence");
    let data_request_sequence = sim.signal("data_request_sequence");
    let data_addr = sim.signal("data_addr");
    let data_wdata = sim.signal("data_wdata");
    let data_wmask = sim.signal("data_wmask");
    let commit_valid = sim.signal("commit_valid");
    let divisor = platform.memory_divisor();
    let mut mailbox = Mailbox {
        tohost: image.tohost,
        low: 0,
    };
    let mut commits = 0_usize;
    let execute_start = Instant::now();
    let mut outcome = None;
    for cycle in 1..=cli.max_cycles {
        let store = timed_value(&mut sim, data_we) != 0
            && timed_value(&mut sim, data_store_retire_ready) != 0
            && timed_value(&mut sim, data_request_sequence)
                == timed_value(&mut sim, data_response_sequence);
        let mut result = None;
        if store {
            let address = timed_value(&mut sim, data_addr);
            let data = timed_value(&mut sim, data_wdata);
            let mask = timed_value(&mut sim, data_wmask);
            result = mailbox.observe(address, data, mask);
        }
        let clocks: &[&str] = if cycle % divisor == 0 {
            &["clk_cpu", "clk_mem"]
        } else {
            &["clk_cpu"]
        };
        timed_edge(&mut sim, clocks)?;
        if timed_value(&mut sim, commit_valid) != 0 {
            commits += 1;
        }
        if let Some(result) = result.filter(|value| *value != 0) {
            if result != 1 {
                return Err(message(format!(
                    "{}: tohost={result} at cycle {cycle}",
                    image.name
                )));
            }
            outcome = Some(Outcome {
                cycles: cycle,
                commits,
            });
            break;
        }
    }
    let execute_elapsed = execute_start.elapsed();
    let outcome = outcome.ok_or_else(|| {
        message(format!(
            "{} timed out after {} cycles and {commits} commits",
            image.name, cli.max_cycles
        ))
    })?;
    Ok((outcome, compile_elapsed, execute_elapsed))
}

fn report(
    name: &str,
    compile_elapsed: Duration,
    execute_elapsed: Duration,
    jit_elapsed: Option<Duration>,
    outcome: &Outcome,
) {
    let jit = jit_elapsed
        .map(|elapsed| elapsed.as_nanos().to_string())
        .unwrap_or_else(|| "NA".to_string());
    println!(
        "CELOX_TEST_TIMING test={name} compile_ns={} execute_ns={} jit_execute_ns={jit}",
        compile_elapsed.as_nanos(),
        execute_elapsed.as_nanos(),
    );
    let cycles_per_second = outcome.cycles as f64 / execute_elapsed.as_secs_f64();
    println!(
        "CELOX_TEST_RESULT test={name} status=pass cycles={} commits={} cycles_per_s={cycles_per_second:.0}",
        outcome.cycles, outcome.commits
    );
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), RicaError> {
    if !cli.native_profile_blocks.is_empty() && cli.dump_ir_dir.is_none() {
        return Err(message(
            "--native-profile-block writes its analysis to --dump-ir-dir; pass both",
        ));
    }
    let project = cli.project.canonicalize()?;
    let source = load_source(&project)?;
    let platform = load_platform(&project, cli.enable_u())?;
    let config = ModelConfig {
        predictor_config: cli.predictor_config,
        icache_index_bits: if cli.no_cache { 0 } else { 8 },
        fast_lsu: if cli.no_cache { 0 } else { 6 },
    };
    let images = cli
        .elfs
        .iter()
        .map(|elf| ElfImage::load(elf))
        .collect::<Result<Vec<_>, _>>()?;

    if cli.model == Model::Timed {
        if cli.harness != Harness::Resolved {
            return Err(message("--model timed supports only --harness resolved"));
        }
        if cli.dump_ir_dir.is_some() || !cli.native_profile_blocks.is_empty() || cli.jit_timing {
            return Err(message(
                "--dump-ir-dir, --native-profile-block and --jit-timing need a native-simulator model",
            ));
        }
        if cli.compile_only {
            // The timed model compiles a fresh simulation per run.
            let compile_start = Instant::now();
            builder(
                Simulation::builder(&source, "HardwareCoreTestTop"),
                &cli,
                platform,
                &config,
            )
            .build()?;
            println!(
                "CELOX_COMPILE top=HardwareCoreTestTop compile_ns={}",
                compile_start.elapsed().as_nanos()
            );
            return Ok(());
        }
        for image in &images {
            for _ in 0..cli.repeat {
                let (outcome, compile_elapsed, execute_elapsed) =
                    run_timed(&source, image, platform, &cli, &config)?;
                report(
                    &image.name,
                    compile_elapsed,
                    execute_elapsed,
                    None,
                    &outcome,
                );
            }
        }
        return Ok(());
    }

    let compile_start = Instant::now();
    let model_builder = builder(
        SimulatorBuilder::new(&source, "HardwareCoreTestTop"),
        &cli,
        platform,
        &config,
    );
    let model = if let Some(output_dir) = &cli.dump_ir_dir {
        fs::create_dir_all(output_dir)?;
        let (compiled, mut trace) = model_builder
            .trace_post_optimized_sir()
            .trace_mir()
            .compile_native_with_trace()?;
        let dumps = [
            ("post_optimized.sir", trace.format_post_optimized_sir()),
            ("native_optimized.sir", trace.native_optimized_sir.take()),
            ("mir.txt", trace.mir.take()),
            (
                "reactive_event_graph.txt",
                trace.reactive_event_graph.take(),
            ),
            ("native_state_layout.txt", trace.native_state_layout.take()),
        ];
        for (name, text) in dumps {
            let text = text.ok_or_else(|| message(format!("{name} was not captured")))?;
            let path = output_dir.join(name);
            fs::write(&path, &text)?;
            eprintln!("wrote {name} ({} bytes) to {}", text.len(), path.display());
        }
        compiled.into_program_image()
    } else {
        model_builder.compile_native()?.into_program_image()
    };
    let compile_elapsed = compile_start.elapsed();
    println!(
        "CELOX_COMPILE top=HardwareCoreTestTop compile_ns={}",
        compile_elapsed.as_nanos()
    );
    if cli.compile_only {
        return Ok(());
    }
    for image in &images {
        for _ in 0..cli.repeat {
            let (outcome, execute_elapsed, jit_elapsed) = match cli.harness {
                Harness::Resolved => run_board(&model, image, platform, &cli, ResolvedHarness)?,
                Harness::Rica => run_board(&model, image, platform, &cli, RicaHarness)?,
            };
            report(
                &image.name,
                compile_elapsed,
                execute_elapsed,
                jit_elapsed,
                &outcome,
            );
        }
    }
    Ok(())
}
