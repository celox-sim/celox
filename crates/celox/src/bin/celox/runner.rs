use std::io::Write as _;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use celox::testbench::{
    compile_initial_testbench, run_compiled_testbench, run_compiled_testbench_with_tick_limit,
};
use celox::{
    OptLevel, OptimizeOptions, Simulator, SirPass, TestResult, TieredCompileKind,
    TieredExecutionStats, TieredExecutionTier, TieredExecutionTiming, TieredPromotionStatus,
};
use clap::{Args, ValueEnum};
use veryl_metadata::Metadata;

#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
#[path = "runner/build_cache.rs"]
mod build_cache;

// Structured records share the benchmark protocol; normal runs print only design output.
macro_rules! report {
    ($enabled:expr, $($args:tt)*) => { if $enabled { println!($($args)*); } };
}

#[path = "runner/backends.rs"]
mod backends;
#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
#[path = "runner/dump_ir.rs"]
mod dump_ir;
use backends::*;
#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
use dump_ir::*;

#[derive(Args, Default)]
pub(super) struct ExecutionArgs {
    /// Defaults to `tiered`, or to `native` when an option needs native code
    /// (compile-only, native images, the build cache or IR dumps) and in
    /// cross-codegen builds.
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// JIT strategy. Auto uses hotspot on supported single-thread native
    /// x86-64 and AArch64 runs.
    #[arg(
        long,
        value_enum,
        default_value = "auto",
        conflicts_with = "hotspot_jit"
    )]
    jit: JitMode,
    /// Alias for --jit hotspot.
    #[arg(long)]
    hotspot_jit: bool,
    #[arg(long)]
    compile_only: bool,
    /// Write a pointer-free native image during compile-only mode.
    #[arg(long, value_name = "PATH")]
    native_image_output: Option<PathBuf>,
    /// Load a pointer-free native image instead of generating machine code.
    #[arg(long, value_name = "PATH")]
    pub native_image_input: Option<PathBuf>,
    /// Reuse native compilation artifacts from this trusted local directory.
    #[arg(long, value_name = "DIR")]
    build_cache_dir: Option<PathBuf>,
    #[arg(long)]
    dump_ir_dir: Option<PathBuf>,
    #[arg(long, conflicts_with = "compile_only")]
    dump_ir_and_run: bool,
    #[arg(long = "native-profile-block", value_parser = parse_native_profile_block)]
    native_profile_blocks: Vec<celox::NativeProfileBlock>,
    #[arg(long = "sir-pass", value_parser = parse_pass_override)]
    pass_overrides: Vec<(bool, SirPass)>,
    #[arg(long, value_parser = parse_native_memory_width)]
    native_memory_width: Option<usize>,
    #[arg(long, value_enum)]
    x86_slp: Option<OnOff>,
    /// Write the partitioned settle's plan, with a lint of its
    /// dependencies, to this file while compiling.
    #[arg(long, value_name = "FILE")]
    pub dump_settle_plan: Option<PathBuf>,
    /// Count the partitioned settle's work and write, after the run, each
    /// partition's runs and the runs that changed nothing it compares.
    #[arg(long, value_name = "FILE", conflicts_with = "compile_only")]
    pub settle_profile: Option<PathBuf>,
    /// Record waveform changes at each simulation time step (VCD).
    #[arg(long, value_name = "FILE", conflicts_with = "compile_only")]
    pub vcd: Option<PathBuf>,
    /// Record waveform changes at each simulation time step (FST).
    #[arg(long, value_name = "FILE", conflicts_with_all = ["compile_only", "vcd"])]
    pub fst: Option<PathBuf>,
    /// Simulation threads, including the calling thread.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=64))]
    pub threads: u32,
    /// Emit machine-readable CELOX_* timing, configuration and result records.
    #[arg(long)]
    pub(super) report: bool,
    /// Require the tiered backend to adopt and execute generated code.
    #[arg(long, requires = "report")]
    require_compiled: bool,
    /// Run without the default scheduler step/tick limit.
    #[arg(long, conflicts_with = "max_steps")]
    pub unlimited: bool,
    /// Partition every phase with independent work instead of only the
    /// phases with an estimated speedup.
    #[arg(long)]
    always_partition: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum JitMode {
    #[default]
    Auto,
    Hotspot,
    WholeImage,
}

impl ExecutionArgs {
    /// Resolve after project settings, including force targets, are applied.
    pub(super) fn use_hotspot(&self, force_signals: bool) -> Result<bool, &'static str> {
        let mode = if self.hotspot_jit {
            JitMode::Hotspot
        } else {
            self.jit
        };
        let reason = self.hotspot_unavailable(force_signals);
        if mode == JitMode::Hotspot
            && let Some(reason) = reason
        {
            return Err(reason);
        }
        if mode == JitMode::WholeImage && self.backend() != Backend::Tiered {
            return Err("--jit whole-image requires --backend tiered");
        }
        Ok(mode != JitMode::WholeImage && reason.is_none())
    }

    fn hotspot_unavailable(&self, force_signals: bool) -> Option<&'static str> {
        if self.backend() != Backend::Tiered {
            Some("hotspot JIT requires --backend tiered")
        } else if !cfg!(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        )) {
            Some("hotspot JIT requires a native x86-64 or AArch64 build")
        } else if self.threads != 1 {
            Some("hotspot JIT requires --threads 1")
        } else if force_signals {
            Some("hotspot JIT does not support force targets")
        } else if !self.native_profile_blocks.is_empty() {
            Some("hotspot JIT does not support --native-profile-block")
        } else {
            None
        }
    }

    pub(super) fn report_jit(&self, force_signals: bool, hotspot: bool) {
        let mode = if hotspot {
            "hotspot"
        } else if self.backend() == Backend::Tiered {
            "whole-image"
        } else {
            "none"
        };
        let fallback = (self.jit == JitMode::Auto && !self.hotspot_jit)
            .then(|| self.hotspot_unavailable(force_signals))
            .flatten();
        report!(
            self.report,
            "CELOX_JIT_CONFIG mode={} reason_json={}",
            mode,
            serde_json::to_string(&fallback).expect("static reason")
        );
    }

    pub(super) fn backend(&self) -> Backend {
        self.backend.unwrap_or(
            // A cross-codegen build only exports foreign native images.
            if cfg!(any(
                all(target_arch = "x86_64", feature = "arm64-codegen"),
                all(target_arch = "aarch64", feature = "x86_64-codegen")
            )) || self.compile_only
                || self.native_image_output.is_some()
                || self.native_image_input.is_some()
                || self.build_cache_dir.is_some()
                || self.dump_ir_dir.is_some()
                || self.dump_ir_and_run
            {
                Backend::Native
            } else {
                Backend::Tiered
            },
        )
    }

    /// The `--sir-pass` overrides, each enabling or disabling one pass.
    pub(super) fn pass_overrides(&self) -> &[(bool, SirPass)] {
        &self.pass_overrides
    }

    pub(super) fn validate_sv(&self) -> Result<(), String> {
        if self.compile_only
            || self.native_image_input.is_some()
            || self.native_image_output.is_some()
            || self.build_cache_dir.is_some()
            || self.dump_ir_dir.is_some()
            || self.dump_ir_and_run
            || !self.native_profile_blocks.is_empty()
            || self.native_memory_width.is_some()
            || self.x86_slp.is_some()
            || self.always_partition
            || self.require_compiled
        {
            return Err(
                "compilation artifacts and codegen tuning currently require a Veryl testbench"
                    .into(),
            );
        }
        Ok(())
    }
}

struct Options {
    report: bool,
    hotspot_jit: bool,
    require_compiled: bool,
    design: super::project::Design,
    project: PathBuf,
    test: String,
    opt_level: OptLevel,
    backend: Backend,
    four_state: bool,
    dump_settle_plan: Option<PathBuf>,
    settle_profile: Option<PathBuf>,
    vcd: Option<PathBuf>,
    fst: Option<PathBuf>,
    compile_only: bool,
    native_image_output: Option<PathBuf>,
    native_image_input: Option<PathBuf>,
    build_cache_dir: Option<PathBuf>,
    tick_limit: Option<u64>,
    dump_ir_dir: Option<PathBuf>,
    dump_ir_and_run: bool,
    native_profile_blocks: Vec<celox::NativeProfileBlock>,
    pass_overrides: Vec<(bool, SirPass)>,
    native_memory_width: usize,
    x86_slp: bool,
    threads: u32,
    always_partition: bool,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum RunError {
    #[error(transparent)]
    Metadata(#[from] veryl_metadata::MetadataError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("compilation failed:\n{source}")]
    Build {
        #[from]
        #[source]
        source: celox::SimulatorError,
    },
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    #[error("native image container failed: {0}")]
    NativeImage(#[from] celox::NativeImageContainerError),
    #[error("{message}")]
    InvalidConfiguration { message: &'static str },
    #[error("{artifact} trace was not captured")]
    MissingTrace { artifact: &'static str },
    #[error("selected module has no native testbench; select a #[test] module with --top")]
    MissingInitialBlock,
    #[error("{message}")]
    TestFailed { message: String },
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
pub(super) enum Backend {
    Native,
    #[cfg(feature = "cranelift")]
    Cranelift,
    Interpreter,
    #[cfg(feature = "wasm-runtime")]
    Wasm,
    Tiered,
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

impl Backend {
    fn as_str(self) -> &'static str {
        match self {
            Backend::Native => "native",
            #[cfg(feature = "cranelift")]
            Backend::Cranelift => "cranelift",
            Backend::Interpreter => "interpreter",
            #[cfg(feature = "wasm-runtime")]
            Backend::Wasm => "wasm",
            Backend::Tiered => "tiered",
        }
    }
}

pub(super) fn run(
    cli: ExecutionArgs,
    design: super::project::Design,
    max_steps: u64,
) -> Result<(), RunError> {
    let mut opts = resolve_options(cli, design, max_steps)?;
    validate_options(&opts)?;
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    #[cfg_attr(
        any(
            all(target_arch = "x86_64", feature = "arm64-codegen"),
            all(target_arch = "aarch64", feature = "x86_64-codegen")
        ),
        allow(unused_mut)
    )]
    let mut native_image = load_native_image(&mut opts)?;
    #[cfg(not(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )))]
    let native_image: Option<()> = None;
    let (sources, metadata): (Vec<(String, PathBuf)>, Option<Metadata>) = if native_image.is_some()
    {
        // The image contains the target-specific machine code and all
        // source-independent runtime/testbench metadata needed to execute
        // it. The execution side therefore does not need the project
        // sources at all.
        (Vec::new(), None)
    } else {
        load_sources(&opts.design)?
    };
    if opts.build_cache_dir.is_some() && metadata.is_none() {
        return Err(RunError::InvalidConfiguration {
            message: "--build-cache-dir requires a Veryl.toml project",
        });
    }
    let source_refs: Vec<(&str, &Path)> = sources
        .iter()
        .map(|(source, path)| (source.as_str(), path.as_path()))
        .collect();
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    let project_root = metadata
        .as_ref()
        .map(Metadata::project_path)
        .unwrap_or(std::env::current_dir()?);
    report!(
        opts.report,
        "CELOX_TEST_CONFIG test={} backend={} opt_level={} four_state={} compile_only={}",
        opts.test,
        opts.backend.as_str(),
        opts.opt_level.as_str(),
        opts.four_state,
        opts.compile_only
    );
    // A separate record keeps the single-thread config line unchanged.
    if opts.threads > 1 {
        report!(
            opts.report,
            "CELOX_TEST_THREADS test={} threads={}",
            opts.test,
            opts.threads
        );
    }

    let total_start = Instant::now();
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    let build_cache = opts.build_cache_dir.as_ref().and_then(|dir| {
        match build_cache::BuildCache::new(dir, &opts, &sources, metadata.as_ref()?) {
            Ok(cache) => Some(cache),
            Err(error) => {
                let _ = writeln!(std::io::stderr().lock(), "build cache disabled: {error}");
                report!(
                    opts.report,
                    "CELOX_BUILD_CACHE test={} status=bypass",
                    opts.test
                );
                None
            }
        }
    });
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    if let Some(cache) = &build_cache {
        native_image = cache.load();
        report!(
            opts.report,
            "CELOX_BUILD_CACHE test={} status={} path={}",
            opts.test,
            if native_image.is_some() {
                "hit"
            } else {
                "miss"
            },
            cache.path().display()
        );
    }
    let optimize_options =
        OptimizeOptions::new(opts.opt_level).with_max_native_memory_width(opts.native_memory_width);
    let mut builder = Simulator::from_sources(source_refs, &opts.test);
    if let Some(metadata) = metadata {
        builder = builder.with_metadata(metadata);
    }
    let mut builder = builder
        .opt_level(opts.opt_level)
        .optimize_options(optimize_options)
        .x86_slp(opts.x86_slp)
        .diagnostic_policy(opts.design.diagnostics.clone())
        .signal_access(opts.design.signal_access.clone())
        .four_state(opts.four_state)
        .hotspot_jit(opts.hotspot_jit)
        .threads(opts.threads as usize)
        .diagnostics_from_env();
    if let Some(path) = &opts.dump_settle_plan {
        builder = builder.settle_plan_output(path);
    }
    if opts.settle_profile.is_some() {
        builder = builder.settle_profile(true);
    }
    if let Some(path) = &opts.vcd {
        builder = builder.vcd(path);
    }
    if let Some(path) = &opts.fst {
        builder = builder.fst(path);
    }
    builder = opts.design.apply_parameters(builder);
    if opts.always_partition {
        builder = builder.parallel_partition(celox::ParallelPartition::Always);
    }
    for block in &opts.native_profile_blocks {
        builder = builder.trace_native_profile_block(&block.function, block.block, block.samples);
    }
    for (enable, pass) in std::mem::take(&mut opts.pass_overrides) {
        builder = if enable {
            builder.enable_pass(pass)
        } else {
            builder.disable_pass(pass)
        };
    }
    if let Some(output_dir) = opts.dump_ir_dir.take() {
        if !matches!(opts.backend, Backend::Native) {
            return Err(RunError::InvalidConfiguration {
                message: "--dump-ir-dir is only supported with --backend native",
            });
        }
        #[cfg(not(any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        )))]
        unreachable!("native backend availability checked above");
        #[cfg(any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        ))]
        {
            return dump_native_ir(&opts, builder, output_dir, &project_root, total_start);
        }
    }
    if opts.compile_only {
        let compile_start = total_start;
        match opts.backend {
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Backend::Native => {
                compile_native_image(
                    &opts,
                    builder,
                    &mut native_image,
                    &build_cache,
                    &project_root,
                )?;
            }
            #[cfg(not(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            )))]
            Backend::Native => unreachable!("native backend availability checked above"),
            #[cfg(feature = "wasm-runtime")]
            Backend::Wasm => {
                builder.build_wasm()?;
            }
            #[cfg(feature = "cranelift")]
            Backend::Cranelift => {
                let _sim = builder.build_cranelift()?;
            }
            Backend::Interpreter | Backend::Tiered => {
                unreachable!("non-compiled backends were rejected above")
            }
        }
        let compile_elapsed = compile_start.elapsed();
        let elapsed = total_start.elapsed();
        print_celox_timing(
            opts.report,
            &opts.test,
            compile_elapsed,
            Duration::ZERO,
            matches!(opts.backend, Backend::Native).then_some(Duration::ZERO),
            None,
        );
        report!(
            opts.report,
            "CELOX_TEST_RESULT test={} status=compile-only elapsed_ns={}",
            opts.test,
            elapsed.as_nanos()
        );
        return Ok(());
    }

    let compile_start = total_start;
    let mut generated_code_exercised = false;
    let outcome = match opts.backend {
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        Backend::Native => run_native(
            &opts,
            builder,
            &mut native_image,
            &build_cache,
            compile_start,
        )?,
        #[cfg(not(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        )))]
        Backend::Native => unreachable!("native backend availability checked above"),
        #[cfg(feature = "cranelift")]
        Backend::Cranelift => run_cranelift(&opts, builder, compile_start)?,
        Backend::Interpreter => run_interpreter(&opts, builder, compile_start)?,
        #[cfg(feature = "wasm-runtime")]
        Backend::Wasm => run_wasm(&opts, builder, compile_start)?,
        Backend::Tiered => {
            run_tiered(&opts, builder, compile_start, &mut generated_code_exercised)?
        }
    };
    report_outcome(&opts, outcome, generated_code_exercised, total_start)
}

/// The options of a run: the command line's, with the defaults the design
/// and the target decide.
fn resolve_options(
    cli: ExecutionArgs,
    design: super::project::Design,
    max_steps: u64,
) -> Result<Options, RunError> {
    let backend = cli.backend();
    let force_signals = !design.signal_access.force_signals.is_empty();
    let hotspot_jit = cli
        .use_hotspot(force_signals)
        .map_err(|message| RunError::InvalidConfiguration { message })?;
    cli.report_jit(force_signals, hotspot_jit);
    Ok(Options {
        report: cli.report,
        hotspot_jit,
        require_compiled: cli.require_compiled,
        project: design.project_root.clone().unwrap_or_else(|| {
            design
                .sources
                .first()
                .cloned()
                .unwrap_or_else(|| PathBuf::from("."))
        }),
        test: design.top.clone(),
        opt_level: design.opt_level.unwrap_or(OptLevel::O2),
        backend,
        four_state: design.four_state.unwrap_or(false),
        dump_settle_plan: cli.dump_settle_plan,
        settle_profile: cli.settle_profile,
        vcd: cli.vcd,
        fst: cli.fst,
        compile_only: cli.compile_only,
        native_image_output: cli.native_image_output,
        native_image_input: cli.native_image_input,
        build_cache_dir: cli.build_cache_dir,
        tick_limit: (!cli.unlimited).then_some(max_steps),
        dump_ir_dir: cli.dump_ir_dir,
        dump_ir_and_run: cli.dump_ir_and_run,
        native_profile_blocks: cli.native_profile_blocks,
        pass_overrides: cli.pass_overrides,
        native_memory_width: cli.native_memory_width.unwrap_or({
            if cfg!(any(
                feature = "x86_64-codegen",
                all(target_arch = "x86_64", not(feature = "arm64-codegen"))
            )) {
                128
            } else {
                64
            }
        }),
        x86_slp: cli
            .x86_slp
            .map(|value| matches!(value, OnOff::On))
            .unwrap_or(cfg!(any(
                feature = "x86_64-codegen",
                all(target_arch = "x86_64", not(feature = "arm64-codegen"))
            ))),
        threads: cli.threads,
        always_partition: cli.always_partition,
        design,
    })
}

/// Reject the combinations of options that cannot run.
fn validate_options(opts: &Options) -> Result<(), RunError> {
    if opts.require_compiled && opts.backend != Backend::Tiered {
        return Err(RunError::InvalidConfiguration {
            message: "--require-compiled requires --backend tiered",
        });
    }
    if opts.build_cache_dir.is_some()
        && (!matches!(opts.backend, Backend::Native)
            || opts.native_image_input.is_some()
            || opts.dump_ir_dir.is_some())
    {
        return Err(RunError::InvalidConfiguration {
            message: "--build-cache-dir requires --backend native and cannot be combined with --native-image-input or --dump-ir-dir",
        });
    }
    if opts.native_image_output.is_some() && !opts.compile_only {
        return Err(RunError::InvalidConfiguration {
            message: "--native-image-output requires --compile-only",
        });
    }
    if opts.compile_only && matches!(opts.backend, Backend::Interpreter | Backend::Tiered) {
        return Err(RunError::InvalidConfiguration {
            message: "--compile-only requires a compiled backend",
        });
    }
    if opts.native_image_input.is_some() && opts.compile_only {
        return Err(RunError::InvalidConfiguration {
            message: "--native-image-input cannot be used with --compile-only",
        });
    }
    if opts.native_image_input.is_some() && opts.native_image_output.is_some() {
        return Err(RunError::InvalidConfiguration {
            message: "--native-image-input and --native-image-output are mutually exclusive",
        });
    }
    if (opts.native_image_input.is_some() || opts.native_image_output.is_some())
        && !matches!(opts.backend, Backend::Native)
    {
        return Err(RunError::InvalidConfiguration {
            message: "native image options require --backend native",
        });
    }
    if opts.native_image_input.is_some() && opts.dump_ir_dir.is_some() {
        return Err(RunError::InvalidConfiguration {
            message: "--native-image-input cannot be combined with --dump-ir-dir",
        });
    }
    if !opts.native_profile_blocks.is_empty() && opts.dump_ir_dir.is_none() {
        return Err(RunError::InvalidConfiguration {
            message: "--native-profile-block requires --dump-ir-dir",
        });
    }
    if opts.dump_ir_and_run && opts.dump_ir_dir.is_none() {
        return Err(RunError::InvalidConfiguration {
            message: "--dump-ir-and-run requires --dump-ir-dir",
        });
    }
    if opts.settle_profile.is_some() && opts.dump_ir_dir.is_some() && !opts.dump_ir_and_run {
        return Err(RunError::InvalidConfiguration {
            message: "--settle-profile with --dump-ir-dir requires --dump-ir-and-run",
        });
    }
    #[cfg(any(
        all(target_arch = "x86_64", feature = "arm64-codegen"),
        all(target_arch = "aarch64", feature = "x86_64-codegen")
    ))]
    if opts.dump_ir_and_run {
        return Err(RunError::InvalidConfiguration {
            message: "--dump-ir-and-run is unavailable during cross-codegen",
        });
    }
    #[cfg(all(target_arch = "x86_64", feature = "arm64-codegen"))]
    if matches!(opts.backend, Backend::Native) && !opts.compile_only {
        return Err(RunError::InvalidConfiguration {
            message: "AArch64 cross-codegen requires --compile-only on an x86-64 host",
        });
    }
    #[cfg(all(target_arch = "aarch64", feature = "x86_64-codegen"))]
    if matches!(opts.backend, Backend::Native) && !opts.compile_only {
        return Err(RunError::InvalidConfiguration {
            message: "x86-64 cross-codegen requires --compile-only on an AArch64 host",
        });
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )))]
    if matches!(opts.backend, Backend::Native) {
        return Err(RunError::InvalidConfiguration {
            message: "the custom native backend is unavailable on this target",
        });
    }
    Ok(())
}

/// The native image `--native-image-input` names, bound to the project
/// root; the image decides whether the run is four-state.
#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
fn load_native_image(opts: &mut Options) -> Result<Option<celox::NativeProgramImage>, RunError> {
    let native_image = match &opts.native_image_input {
        Some(path) => {
            let mut image = celox::NativeProgramImage::from_container_bytes(&fs::read(path)?)?;
            // Relative images need the current project root, but not its sources.
            // Existing standalone images with absolute paths remain loadable.
            let root = if let Ok(metadata_path) = Metadata::search_from(&opts.project) {
                fs::canonicalize(metadata_path)?
                    .parent()
                    .unwrap()
                    .to_path_buf()
            } else if opts.project.is_absolute() {
                opts.project.clone()
            } else {
                std::env::current_dir()?.join(&opts.project)
            };
            image.try_map_paths(|path| {
                Ok::<_, std::io::Error>(build_cache::bind_path(path, &root))
            })?;
            opts.four_state = image.four_state();
            Some(image)
        }
        None => None,
    };
    Ok(native_image)
}

/// `--compile-only` on the native backend: the image the run was given, the
/// build cache holds or the compiler produces, written where
/// `--native-image-output` names.
#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
fn compile_native_image(
    opts: &Options,
    builder: celox::SimulatorBuilder<'_, Simulator>,
    native_image: &mut Option<celox::NativeProgramImage>,
    build_cache: &Option<build_cache::BuildCache>,
    project_root: &Path,
) -> Result<(), RunError> {
    let image = if let Some(image) = native_image.take() {
        image
    } else if let Some(cache) = build_cache {
        let (compiled, dependencies) =
            celox_frontend_veryl::capture_file_dependencies(|collector| {
                builder
                    .file_dependency_collector(collector.clone())
                    .compile_native()
            });
        let compiled = compiled?;
        super::report_warnings(compiled.warnings())
            .map_err(|message| RunError::TestFailed { message })?;
        cache.store(compiled.program_image(), &dependencies);
        compiled.into_program_image()
    } else {
        let compiled = builder.compile_native()?;
        super::report_warnings(compiled.warnings())
            .map_err(|message| RunError::TestFailed { message })?;
        compiled.into_program_image()
    };
    if let Some(output_path) = &opts.native_image_output {
        if let Some(parent) = output_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        build_cache::write_relative_image(&image, project_root, output_path)?;
        report!(
            opts.report,
            "CELOX_NATIVE_IMAGE test={} mode=generated path={}",
            opts.test,
            output_path.display()
        );
    }
    Ok(())
}

/// Report the timing and the result of a run, and turn the result into the
/// runner's.
fn report_outcome(
    opts: &Options,
    outcome: Outcome,
    generated_code_exercised: bool,
    total_start: Instant,
) -> Result<(), RunError> {
    let (
        result,
        ticks,
        tick_limit_reached,
        compile_elapsed,
        execute_elapsed,
        jit_execute_elapsed,
        execute_cpu_elapsed,
        tiered_stats,
        tiered_promotion_error,
        tiered_timing,
    ) = outcome;
    let elapsed = total_start.elapsed();
    print_celox_timing(
        opts.report,
        &opts.test,
        compile_elapsed,
        execute_elapsed,
        jit_execute_elapsed,
        execute_cpu_elapsed,
    );
    if let Some(stats) = tiered_stats {
        let promotion_elapsed = tiered_timing.and_then(TieredExecutionTiming::promotion_elapsed);
        print_tiered_stats(opts.report, &opts.test, stats, promotion_elapsed);
        if let Some(timing) = tiered_timing {
            print_tiered_timeline(opts.report, &opts.test, timing);
        }
        if opts.require_compiled && !tick_limit_reached && !generated_code_exercised {
            let detail = tiered_promotion_error
                .as_deref()
                .unwrap_or("no generated code was exercised (the run may have finished before JIT publication)");
            report!(
                opts.report,
                "CELOX_TEST_RESULT test={} status=fail elapsed_ns={}",
                opts.test,
                elapsed.as_nanos()
            );
            return Err(RunError::TestFailed {
                message: format!("tiered execution did not exercise generated code: {detail}"),
            });
        }
    }
    if opts.tick_limit.is_some() {
        report!(
            opts.report,
            "CELOX_TEST_TICK_LIMIT test={} ticks={} reached={}",
            opts.test,
            ticks,
            tick_limit_reached
        );
    }

    match result {
        TestResult::Pass if tick_limit_reached => {
            report!(
                opts.report,
                "CELOX_TEST_RESULT test={} status=tick-limit elapsed_ns={}",
                opts.test,
                elapsed.as_nanos()
            );
            Err(RunError::TestFailed {
                message: format!(
                    "simulation reached --max-steps {} testbench ticks before completion",
                    opts.tick_limit.unwrap()
                ),
            })
        }
        TestResult::Pass => {
            report!(
                opts.report,
                "CELOX_TEST_RESULT test={} status=pass elapsed_ns={}",
                opts.test,
                elapsed.as_nanos()
            );
            Ok(())
        }
        TestResult::Fail(message) => {
            report!(
                opts.report,
                "CELOX_TEST_RESULT test={} status=fail elapsed_ns={}",
                opts.test,
                elapsed.as_nanos()
            );
            Err(RunError::TestFailed { message })
        }
    }
}

/// When each tier ran: the background compilations, and the time and
/// testbench tick at which generated code, then optimizing code, took over.
/// Write the settle's work counters to `path`: a summary, then per
/// partition its runs, the runs that changed something it compares, and the
/// runs that changed nothing (`--settle-profile`). Partition numbers are
/// those of `--dump-settle-plan`.
pub(super) fn write_settle_profile(
    path: Option<&std::path::Path>,
    layout: &celox::MemoryLayout,
    profile: Option<celox::SettleProfile>,
) -> std::io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let Some(profile) = profile else {
        // A loaded native image keeps the code it was compiled with.
        return fs::write(
            path,
            if layout.settle.is_some() {
                "# the partitioned settle has no run counters: its code was compiled without --settle-profile\n"
            } else {
                "# no partitioned settle\n"
            },
        );
    };
    let runs = profile.total_runs();
    let unchanged = profile.unchanged_runs();
    let mut out = format!(
        "# settles {}, partition runs {runs} ({:.2} per settle), runs changing nothing compared {unchanged} ({:.1}%)\npartition\truns\tchanged\tunchanged\n",
        profile.settles,
        runs as f64 / profile.settles.max(1) as f64,
        100.0 * unchanged as f64 / runs.max(1) as f64,
    );
    for (partition, (runs, changed)) in profile.runs.iter().zip(&profile.changed).enumerate() {
        out.push_str(&format!(
            "{partition}\t{runs}\t{changed}\t{}\n",
            runs - changed
        ));
    }
    fs::write(path, out)
}

fn print_tiered_timeline(report: bool, test: &str, timing: TieredExecutionTiming) {
    let optional = |value: Option<u128>| value.map_or_else(|| "NA".to_string(), |v| v.to_string());
    let compiles = timing
        .compiles()
        .map(|stage| {
            let kind = match stage.kind {
                TieredCompileKind::Single => "single",
                TieredCompileKind::Baseline => "baseline",
                TieredCompileKind::Optimizing => "optimizing",
                TieredCompileKind::DerivedEdges => "derived_edges",
                _ => "unknown",
            };
            let outcome = if stage.completed {
                "completed"
            } else {
                "failed"
            };
            format!("{kind}:{}:{outcome}", stage.elapsed.as_nanos())
        })
        .collect::<Vec<_>>();
    let compiles = if compiles.is_empty() {
        "none".to_string()
    } else {
        compiles.join(",")
    };
    let promotion = timing.promotion();
    let upgrade = timing.upgrade();
    report!(
        report,
        "CELOX_TIERED_TIMELINE test={test} compiles={compiles} promoted_ns={} promoted_tick={} upgraded_ns={} upgraded_tick={} finish_tick={}",
        optional(promotion.map(|t| t.elapsed.as_nanos())),
        optional(promotion.and_then(|t| t.tick).map(u128::from)),
        optional(upgrade.map(|t| t.elapsed.as_nanos())),
        optional(upgrade.and_then(|t| t.tick).map(u128::from)),
        optional(timing.finish_tick().map(u128::from)),
    );
}

fn print_tiered_stats(
    report: bool,
    test: &str,
    stats: TieredExecutionStats,
    promotion_elapsed: Option<Duration>,
) {
    let tier = match stats.tier {
        TieredExecutionTier::Interpreter => "interpreter",
        TieredExecutionTier::Compiled => "compiled",
        _ => "unknown",
    };
    let promotion = match stats.promotion {
        TieredPromotionStatus::Pending => "pending",
        TieredPromotionStatus::Failed => "failed",
        TieredPromotionStatus::Disabled => "disabled",
        TieredPromotionStatus::Promoted => "promoted",
        _ => "unknown",
    };
    let promoted_after = stats
        .promoted_after_interpreted_evaluations
        .map(|value| value.to_string())
        .unwrap_or_else(|| "NA".to_string());
    let promotion_elapsed_ns = promotion_elapsed
        .map(|elapsed| elapsed.as_nanos().to_string())
        .unwrap_or_else(|| "NA".to_string());
    report!(
        report,
        "CELOX_TIERED_STATS test={test} tier={tier} promotion={promotion} interpreted_evaluations={} compiled_evaluations={} promoted_after_interpreted_evaluations={promoted_after} promotion_elapsed_ns={promotion_elapsed_ns} safe_point_polls={} split_apply_deferrals={} threshold_deferrals={}",
        stats.interpreted_evaluations,
        stats.compiled_evaluations,
        stats.safe_point_polls,
        stats.split_apply_deferrals,
        stats.threshold_deferrals,
    );
}

fn print_celox_timing(
    report: bool,
    test: &str,
    compile_elapsed: Duration,
    execute_elapsed: Duration,
    jit_execute_elapsed: Option<Duration>,
    execute_cpu_elapsed: Option<Duration>,
) {
    let jit_execute_ns = jit_execute_elapsed
        .map(|elapsed| elapsed.as_nanos().to_string())
        .unwrap_or_else(|| "NA".to_string());
    if let Some(execute_cpu_elapsed) = execute_cpu_elapsed {
        report!(
            report,
            "CELOX_TEST_TIMING test={test} compile_ns={} execute_ns={} jit_execute_ns={jit_execute_ns} execute_cpu_ns={}",
            compile_elapsed.as_nanos(),
            execute_elapsed.as_nanos(),
            execute_cpu_elapsed.as_nanos()
        );
    } else {
        report!(
            report,
            "CELOX_TEST_TIMING test={test} compile_ns={} execute_ns={} jit_execute_ns={jit_execute_ns}",
            compile_elapsed.as_nanos(),
            execute_elapsed.as_nanos()
        );
    }
}

#[cfg(unix)]
fn process_cpu_time() -> Option<Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) };
    (result == 0).then(|| {
        Duration::new(
            time.tv_sec.try_into().unwrap_or_default(),
            time.tv_nsec.try_into().unwrap_or_default(),
        )
    })
}

#[cfg(not(unix))]
fn process_cpu_time() -> Option<Duration> {
    None
}

#[derive(Debug, thiserror::Error)]
enum CliValueError {
    #[error("invalid native memory width: {value}")]
    InvalidNativeMemoryWidth { value: String },
    #[error("invalid native profile block: {value}")]
    InvalidNativeProfileBlock { value: String },
    #[error("invalid block number in native profile block: {value}")]
    InvalidNativeProfileBlockNumber {
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("invalid sample count in native profile block: {value}")]
    InvalidNativeProfileSampleCount {
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("invalid pass override: {value}")]
    InvalidPassOverride { value: String },
    #[error("unknown SIR pass: {name}")]
    UnknownSirPass { name: String },
}

fn parse_native_memory_width(value: &str) -> Result<usize, CliValueError> {
    match value {
        "64" => Ok(64),
        "128" => Ok(128),
        _ => Err(CliValueError::InvalidNativeMemoryWidth {
            value: value.to_owned(),
        }),
    }
}

fn parse_native_profile_block(value: &str) -> Result<celox::NativeProfileBlock, CliValueError> {
    let (function_and_block, samples) =
        value
            .rsplit_once(':')
            .ok_or_else(|| CliValueError::InvalidNativeProfileBlock {
                value: value.to_owned(),
            })?;
    let (function, block) = function_and_block.rsplit_once(':').ok_or_else(|| {
        CliValueError::InvalidNativeProfileBlock {
            value: value.to_owned(),
        }
    })?;
    let block = block.strip_prefix("bb").unwrap_or(block);
    let block =
        block
            .parse::<u32>()
            .map_err(|source| CliValueError::InvalidNativeProfileBlockNumber {
                value: value.to_owned(),
                source,
            })?;
    let samples = samples.parse::<u64>().map_err(|source| {
        CliValueError::InvalidNativeProfileSampleCount {
            value: value.to_owned(),
            source,
        }
    })?;
    if function.is_empty() || samples == 0 {
        return Err(CliValueError::InvalidNativeProfileBlock {
            value: value.to_owned(),
        });
    }
    Ok(celox::NativeProfileBlock {
        function: function.to_string(),
        block,
        samples,
    })
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

fn load_sources(
    design: &super::project::Design,
) -> Result<(Vec<(String, PathBuf)>, Option<Metadata>), RunError> {
    let entry = design
        .project_root
        .as_deref()
        .or_else(|| design.sources.first().map(PathBuf::as_path))
        .unwrap_or(Path::new("."));
    let metadata = Metadata::search_from(entry)
        .ok()
        .map(|path| Metadata::load(&path))
        .transpose()?;
    let mut paths = design.sources.clone();
    if design.project_root.is_none() {
        if let Some(mut metadata) = metadata.clone() {
            for path in metadata.paths::<&Path>(&[], false, true)? {
                if !paths
                    .iter()
                    .any(|p| p.canonicalize().ok().as_ref() == Some(&path.src))
                {
                    paths.push(path.src);
                }
            }
        }
    }
    let sources = paths
        .into_iter()
        .map(|path| {
            fs::read_to_string(&path)
                .map_err(|error| {
                    std::io::Error::new(
                        error.kind(),
                        format!("failed to read {}: {error}", path.display()),
                    )
                })
                .map(|source| (source, path))
        })
        .collect::<Result<_, _>>()?;
    Ok((sources, metadata))
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
pub(super) fn print_hotspot_stats(test: &str, stats: &celox::HotspotStats) {
    report!(
        true,
        "CELOX_HOTSPOT_STATS test={test} interpreted_calls={} compiled_calls={} compiled_block_calls={} compiled_entries={} rejected_entries={} failed_entries={} code_bytes={} compile_ns={} frame_bytes_transferred={}",
        stats.interpreted_calls,
        stats.compiled_calls,
        stats.compiled_block_calls,
        stats.compiled_entries,
        stats.rejected_entries,
        stats.failed_entries,
        stats.code_bytes,
        stats.compile_time.as_nanos(),
        stats.frame_bytes_transferred
    );
}

#[cfg(test)]
mod jit_policy_tests {
    use super::*;

    #[test]
    fn auto_respects_force_targets_threads_and_explicit_backends() {
        let mut args = ExecutionArgs {
            threads: 1,
            ..Default::default()
        };
        assert_eq!(
            args.use_hotspot(false),
            Ok(cfg!(all(
                target_arch = "x86_64",
                not(feature = "arm64-codegen")
            )))
        );
        assert_eq!(args.use_hotspot(true), Ok(false));
        args.jit = JitMode::Hotspot;
        assert!(args.use_hotspot(true).is_err());
        args.threads = 4;
        assert!(args.use_hotspot(false).is_err());
        args.jit = JitMode::Auto;
        assert_eq!(args.use_hotspot(false), Ok(false));
        args.threads = 1;
        args.backend = Some(Backend::Native);
        assert_eq!(args.use_hotspot(false), Ok(false));
        args.jit = JitMode::WholeImage;
        assert!(args.use_hotspot(false).is_err());
        args.backend = Some(Backend::Tiered);
        assert_eq!(args.use_hotspot(false), Ok(false));
        assert_eq!(args.use_hotspot(true), Ok(false));
    }
}
