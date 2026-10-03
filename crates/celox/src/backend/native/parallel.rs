//! Experimental execution of automatically partitioned dependency graphs.
use super::*;
use crate::diagnostics::ParallelEnvironment;
// Old experimental images lack final-emission access certificates.
pub(super) const ENTRY_PREFIX: &str = "parallel-physical-v1/";
macro_rules! report {
    ($($arg:tt)*) => {
        if crate::diagnostics::ParallelEnvironment::from_env().get("CELOX_PARALLEL_DIAGNOSTICS") == Some("1") {
            crate::diagnostics::write_parallel_observation(&mut std::io::stderr().lock(), format_args!($($arg)*));
        }
    };
}
#[path = "parallel_selection.rs"]
mod selection;
#[path = "parallel_topology.rs"]
mod topology;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;

#[derive(Default)]
pub(super) struct Access {
    reads: BTreeSet<(usize, usize)>,
    writes: BTreeSet<(usize, usize)>,
    effects: bool,
}
impl Access {
    #[cfg(any(
        test,
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    ))]
    pub(super) fn from_ranges(reads: Vec<(usize, usize)>, writes: Vec<(usize, usize)>) -> Self {
        Self {
            reads: reads.into_iter().collect(),
            writes: writes.into_iter().collect(),
            effects: false,
        }
    }
}
fn overlap(a: &BTreeSet<(usize, usize)>, b: &BTreeSet<(usize, usize)>) -> bool {
    let mut left = a.iter().copied().filter(|&(_, size)| size != 0).peekable();
    let mut right = b.iter().copied().filter(|&(_, size)| size != 0).peekable();
    while let (Some(&(x, n)), Some(&(y, m))) = (left.peek(), right.peek()) {
        if x.saturating_add(n) <= y {
            left.next();
        } else if y.saturating_add(m) <= x {
            right.next();
        } else {
            return true;
        }
    }
    false
}
fn independent(a: &Access, b: &Access) -> bool {
    !a.effects
        && !b.effects
        && !overlap(&a.writes, &b.writes)
        && !overlap(&a.reads, &b.writes)
        && !overlap(&a.writes, &b.reads)
}
fn empty_group(unit: &ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    unit.blocks.len() == 1
        && unit.blocks.values().all(|block| {
            block.instructions.is_empty()
                && matches!(block.terminator, celox_sir::SIRTerminator::Return)
        })
}
// Observable effects remain ordered even if their physical writes are disjoint.
fn observable_effects(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    eu.blocks.values().any(|b| {
        matches!(b.terminator, celox_sir::SIRTerminator::Error(_))
            || b.instructions.iter().any(|i| match i {
                SIRInstruction::Store(_, _, _, _, _, sites) => !sites.is_empty(),
                SIRInstruction::RuntimeEvent { .. }
                | SIRInstruction::CombCaptureEvent { .. }
                | SIRInstruction::CombCaptureEnableIfChanged { .. } => true,
                _ => false,
            })
    })
}
fn schedule_waves(accesses: &[Access]) -> Vec<usize> {
    let mut wave_of = Vec::with_capacity(accesses.len());
    let mut wave_start = 0;
    let mut wave = 0;
    for i in 0..accesses.len() {
        if (wave_start..i).any(|j| !independent(&accesses[i], &accesses[j])) {
            wave += 1;
            wave_start = i;
        }
        wave_of.push(wave);
    }
    wave_of
}
fn rel32(code: &mut [u8], pos: usize, target: usize) {
    let delta = i32::try_from(target as i64 - (pos + 4) as i64).unwrap();
    code[pos..pos + 4].copy_from_slice(&delta.to_le_bytes());
}
fn append_chain(
    code: &mut Vec<u8>,
    offsets: &[usize],
    flags: &[usize],
    global: usize,
    bytes: usize,
) -> usize {
    let entry = code.len();
    code.extend([0x53, 0x48, 0x89, 0xfb, 0x31, 0xc0]);
    for &flag in flags {
        for b in 0..bytes {
            code.extend([0xc6, 0x83]);
            code.extend(i32::try_from(flag + b).unwrap().to_le_bytes());
            code.push(0);
        }
    }
    let mut exits = Vec::new();
    for &offset in offsets {
        code.extend([0x48, 0x89, 0xdf, 0xe8]);
        let pos = code.len();
        code.extend([0; 4]);
        rel32(code, pos, offset);
        code.extend([0x48, 0x85, 0xc0, 0x0f, 0x85]);
        exits.push(code.len());
        code.extend([0; 4]);
    }
    let end = code.len();
    for &flag in flags {
        for b in 0..bytes {
            code.extend([0x8a, 0x93]);
            code.extend(i32::try_from(flag + b).unwrap().to_le_bytes());
            code.extend([0x08, 0x93]);
            code.extend(i32::try_from(global + b).unwrap().to_le_bytes());
        }
    }
    code.extend([0x5b, 0xc3]);
    for pos in exits {
        rel32(code, pos, end);
    }
    entry
}
pub(super) fn compile(
    units: &[&ExecutionUnit<RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    options: &crate::backend::X86BackendOptions,
    diagnostics: &crate::optimizer::SirDiagnostics,
    cancel: Option<&CompileCancel>,
) -> Result<CompiledNativeFunction, SimulatorError> {
    if !cfg!(all(
        target_arch = "x86_64",
        target_os = "linux",
        not(feature = "arm64-codegen")
    )) || four_state
        || layout.trace.is_some()
    {
        return Err(codegen_message(
            "parallel native code requires Linux x86-64, two-state mode, and tracing disabled",
        ));
    }
    // Optimization can erase a partition completely. Such return-only groups
    // must not inflate measured wave width or allocate private scratch arenas.
    let nonempty = units
        .iter()
        .copied()
        .filter(|unit| !empty_group(unit))
        .collect::<Vec<_>>();
    let units = if nonempty.is_empty() {
        // Keep one callable no-op for an entirely empty event.
        &units[..units.len().min(1)]
    } else {
        nonempty.as_slice()
    };
    for (i, unit) in units.iter().enumerate() {
        unit.verify_result().map_err(|error| {
            codegen_message(format!(
                "invalid parallel group {i} before native lowering: {error}"
            ))
        })?;
    }
    // No concurrency decision is made from pre-codegen SIR. The final emitter
    // must supply a physical footprint; missing certificates become singleton waves.
    let mut accesses = Vec::with_capacity(units.len());
    let mut code_metrics = Vec::with_capacity(units.len());
    use std::io::Write;
    let config = ParallelEnvironment::from_env();
    let mut out = config.get("CELOX_PARALLEL_PARTITION").map(|dir| {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("create parallel diagnostics directory");
        let mut out = std::fs::File::create(dir.join(format!("native-groups-{}.tsv", units.len())))
            .expect("create parallel group diagnostics");
        writeln!(
            out,
            "group\twave\treads\twrites\teffects\tcodebytes\tstatebytes"
        )
        .unwrap();
        out
    });
    let mut code = vec![0xe9, 0, 0, 0, 0];
    let mut symbols = Vec::new();
    let mut offsets = Vec::new();
    let mut flags = Vec::new();
    let mut required = 0;
    let mut features = 0;
    // Index cross-group reads once. Avoid scanning every other group's entire
    // instruction stream separately for each group on large designs.
    // Store one owner per distinct address; MAX means multiple readers. This
    // avoids retaining a second HashSet of reads for every partition.
    let mut readers = crate::HashMap::<_, usize>::default();
    for (i, unit) in units.iter().enumerate() {
        for ins in unit.blocks.values().flat_map(|b| &b.instructions) {
            let addr = match ins {
                SIRInstruction::Load(_, a, ..)
                | SIRInstruction::Commit(a, ..)
                | SIRInstruction::Store(a, _, 0, ..) => a.absolute_addr(),
                _ => continue,
            };
            readers
                .entry(addr)
                .and_modify(|owner| {
                    if *owner != i {
                        *owner = usize::MAX;
                    }
                })
                .or_insert(i);
        }
    }
    // StateSSA keys slots by logical address, while the layout can give aliases
    // the same physical home. Keep such homes published even for readers in the
    // same group: promoting one spelling does not forward loads of another.
    let mut homes: Vec<_> = layout.offsets.values().copied().collect();
    homes.sort_unstable();
    let aliased_homes: crate::HashSet<_> = homes
        .windows(2)
        .filter_map(|pair| (pair[0] == pair[1]).then_some(pair[0]))
        .collect();
    drop(homes);
    let mut canonical_homes = crate::HashMap::default();
    for (&addr, &home) in &layout.offsets {
        if aliased_homes.contains(&home) && !layout.unpacked_arrays.contains_key(&addr) {
            canonical_homes
                .entry((home, layout.widths[&addr]))
                .and_modify(|canonical: &mut AbsoluteAddr| *canonical = (*canonical).min(addr))
                .or_insert(addr);
        }
    }
    let mut next_private = layout.merged_total_size + layout.triggered_bits_total_size;
    // Only the arena/trigger offsets differ. Reuse one layout copy instead of
    // cloning every address map for each group in a large partition graph.
    let mut local = layout.clone();
    for (i, unit) in units.iter().enumerate() {
        let flag = next_private.div_ceil(64) * 64;
        flags.push(flag);
        local.triggered_bits_offset = flag;
        local.merged_total_size = flag + layout.triggered_bits_total_size.div_ceil(64) * 64;
        let label = format!("parallel_group/{i}");
        let mut promoted = (*unit).clone();
        // Block scheduling also reasons about logical addresses. Give equal
        // scalar homes one spelling so it cannot hoist an alias load above the
        // store it observes. Working/sparse storage has its own address maps.
        for ins in promoted
            .blocks
            .values_mut()
            .flat_map(|b| &mut b.instructions)
        {
            *ins = ins.map_addr(|addr| {
                let absolute = addr.absolute_addr();
                if addr.region == 0
                    && !layout.unpacked_arrays.contains_key(&absolute)
                    && let Some(canonical) = layout
                        .offsets
                        .get(&absolute)
                        .and_then(|home| canonical_homes.get(&(*home, layout.widths[&absolute])))
                {
                    return RegionedAbsoluteAddr::from_absolute_addr(0, *canonical);
                }
                *addr
            });
        }
        let external_reads = promoted
            .blocks
            .values()
            .flat_map(|b| &b.instructions)
            .filter_map(|ins| match ins {
                SIRInstruction::Store(a, ..) => Some(a.absolute_addr()),
                _ => None,
            })
            .filter(|addr| {
                readers.get(addr).is_some_and(|&owner| owner != i)
                    || layout
                        .offsets
                        .get(addr)
                        .is_some_and(|home| aliased_homes.contains(home))
            })
            .collect();
        celox_sir_opt::optimizer::promote_partition_comb_static_slots(
            &mut promoted,
            &external_reads,
        )
        .map_err(|e| codegen_message(format!("parallel fused promotion: {e}")))?;
        promoted.verify_result().map_err(|error| {
            codegen_message(format!(
                "invalid parallel group {i} after local promotion: {error}"
            ))
        })?;
        let mut compiled = compile_unit_refs(
            &[&promoted],
            &local,
            false,
            &label,
            None,
            options,
            false,
            diagnostics,
            cancel,
        )?;
        let mut access = compiled.parallel_access.take().unwrap_or_else(|| Access {
            effects: true,
            ..Access::default()
        });
        access.effects |= observable_effects(unit);
        accesses.push(access);
        code_metrics.push((compiled.code.len(), compiled.required_state_size));
        // The next group starts beyond this group's actual scratch allocation;
        // no fixed megabyte-per-group arena or overlapping spill storage.
        next_private = compiled.required_state_size.max(local.merged_total_size);
        required = required.max(compiled.required_state_size);
        features |= compiled.required_native_features;
        while code.len() % 16 != 0 {
            code.push(0x90);
        }
        let offset = code.len();
        offsets.push(offset);
        symbols.push(jit_mem::JitSymbol {
            offset,
            size: compiled.code.len(),
            name: String::new(),
        });
        code.extend(compiled.code);
    }
    let wave_of = schedule_waves(&accesses);
    for (i, symbol) in symbols.iter_mut().enumerate() {
        symbol.name = format!("parallel-group/{i}/{}/{}", wave_of[i], flags[i]);
        if let Some(out) = &mut out {
            writeln!(
                out,
                "{i}\t{}\t{}\t{}\t{}\t{}\t{}",
                wave_of[i],
                accesses[i].reads.len(),
                accesses[i].writes.len(),
                accesses[i].effects,
                code_metrics[i].0,
                code_metrics[i].1
            )
            .unwrap();
        }
    }
    drop(accesses);
    let chain = append_chain(
        &mut code,
        &offsets,
        &flags,
        layout.triggered_bits_offset,
        layout.triggered_bits_total_size,
    );
    rel32(&mut code, 1, chain);
    Ok(CompiledNativeFunction {
        parallel_access: None,
        code,
        symbols,
        trace: None,
        required_state_size: required,
        required_native_features: features,
    })
}

#[repr(align(64))]
struct Counter(AtomicUsize);
struct Barrier {
    count: Counter,
    generation: Counter,
    n: usize,
}
impl Barrier {
    fn wait(&self) {
        let generation = self.generation.0.load(Ordering::Acquire);
        if self.count.0.fetch_add(1, Ordering::AcqRel) == self.n - 1 {
            self.count.0.store(0, Ordering::Relaxed);
            self.generation.0.fetch_add(1, Ordering::Release);
        } else {
            let mut spins = 0;
            while self.generation.0.load(Ordering::Acquire) == generation {
                std::hint::spin_loop();
                spins += 1;
                // A descheduled participant must be able to run even when all
                // admitted CPUs have spinning peers. Keep the short rendezvous
                // path local, but yield during prolonged contention.
                if spins == 256 {
                    std::thread::yield_now();
                    spins = 0;
                }
            }
        }
    }
}
struct Shared {
    barrier: Barrier,
    ptr: AtomicUsize,
    wave: AtomicUsize,
    stop: AtomicBool,
    errors: Vec<AtomicUsize>,
    waves: Vec<Vec<NativeSimFunc>>,
    flags: Vec<usize>,
    flag_global: usize,
    flag_bytes: usize,
    _code: Arc<SharedNativeCode>,
}
pub(super) struct Pool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    width: usize,
}
impl Pool {
    fn new(
        code: Arc<SharedNativeCode>,
        entry: usize,
        width: usize,
        placement: &topology::Placement,
    ) -> Option<Self> {
        let image = &code.program_image;
        let blob = image.code_entries.iter().find(|b| b.offset == entry)?;
        let mut groups = image
            .symbols
            .iter()
            .filter(|s| s.offset >= entry && s.offset < entry + blob.size)
            .filter_map(|s| {
                let fields = s
                    .name
                    .split_once("parallel-group/")?
                    .1
                    .split('/')
                    .collect::<Vec<_>>();
                Some((
                    fields[0].parse::<usize>().unwrap(),
                    fields[1].parse::<usize>().unwrap(),
                    s.offset,
                    fields[2].parse::<usize>().unwrap(),
                ))
            })
            .collect::<Vec<_>>();
        if groups.is_empty() {
            return None;
        }
        groups.sort();
        let mut waves = Vec::<Vec<NativeSimFunc>>::new();
        let mut flags = Vec::new();
        for (_, w, offset, flag) in groups {
            flags.push(flag);
            while waves.len() <= w {
                waves.push(Vec::new());
            }
            waves[w].push(native_function_at(&code._jit_image, offset).unwrap());
        }
        let shared = Arc::new(Shared {
            barrier: Barrier {
                count: Counter(AtomicUsize::new(0)),
                generation: Counter(AtomicUsize::new(0)),
                n: width,
            },
            ptr: AtomicUsize::new(0),
            wave: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            errors: (0..width).map(|_| AtomicUsize::new(0)).collect(),
            waves,
            flags,
            flag_global: code.layout.triggered_bits_offset,
            flag_bytes: code.layout.triggered_bits_total_size,
            _code: code,
        });
        let mut threads = Vec::new();
        let mut launches = Vec::new();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        for worker in 1..width {
            let s = shared.clone();
            let cpu = placement.target(worker);
            let (launch_tx, launch_rx) = std::sync::mpsc::channel();
            launches.push(launch_tx);
            let ready_tx = ready_tx.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(format!("celox-parallel-{worker}"))
                    .spawn(move || {
                        // No worker may enter the non-cancellable spin barrier until
                        // every spawn and affinity operation has succeeded. On startup
                        // failure, dropping the launch senders releases all waiters.
                        let pinned = std::panic::catch_unwind(|| topology::pin(cpu)).is_ok();
                        let _ = ready_tx.send(pinned);
                        drop(ready_tx);
                        if launch_rx.recv().is_err() || !pinned {
                            return;
                        }
                        loop {
                            s.barrier.wait();
                            if s.stop.load(Ordering::Relaxed) {
                                break;
                            }
                            execute(&s, worker, width);
                            s.barrier.wait();
                        }
                    })
                    .expect("cannot start parallel simulation worker"),
            );
        }
        drop(ready_tx);
        assert!(
            ready_rx.into_iter().all(|ready| ready),
            "parallel worker affinity setup failed"
        );
        for launch in launches {
            launch
                .send(())
                .expect("parallel worker exited during startup");
        }
        Some(Self {
            shared,
            threads,
            width,
        })
    }
    fn run(&self, ptr: *mut u8, profile: bool, timings: &mut Vec<u128>) -> i64 {
        let s = &self.shared;
        s.ptr.store(ptr as usize, Ordering::Relaxed);
        timings.resize(s.waves.len(), 0);
        // No worker is executing between rendezvous; clear before dispatch.
        for &flag in &s.flags {
            unsafe {
                std::ptr::write_bytes(ptr.add(flag), 0, s.flag_bytes);
            }
        }
        let merge_flags = || {
            for &flag in &s.flags {
                for b in 0..s.flag_bytes {
                    unsafe {
                        *ptr.add(s.flag_global + b) |= *ptr.add(flag + b);
                    }
                }
            }
        };
        for (w, group) in s.waves.iter().enumerate() {
            let start = profile.then(Instant::now);
            if self.width == 1 || group.len() == 1 {
                for &f in group {
                    let result = unsafe { f(ptr) };
                    if result != 0 {
                        merge_flags();
                        return result;
                    }
                }
            } else {
                s.wave.store(w, Ordering::Relaxed);
                s.barrier.wait();
                execute(s, 0, self.width);
                s.barrier.wait();
                for e in &s.errors {
                    let r = e.load(Ordering::Relaxed) as i64;
                    if r != 0 {
                        merge_flags();
                        return r;
                    }
                }
            }
            if let Some(start) = start {
                timings[w] += start.elapsed().as_nanos();
            }
        }
        merge_flags();
        0
    }
}
fn execute(s: &Shared, worker: usize, width: usize) {
    let group = &s.waves[s.wave.load(Ordering::Relaxed)];
    let ptr = s.ptr.load(Ordering::Relaxed) as *mut u8;
    let mut error = 0;
    for &f in group.iter().skip(worker).step_by(width) {
        error = unsafe { f(ptr) };
        if error != 0 {
            break;
        }
    }
    s.errors[worker].store(error as usize, Ordering::Relaxed);
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.barrier.wait();
        for t in self.threads.drain(..) {
            t.join().unwrap();
        }
    }
}
#[derive(Default)]
pub(super) struct Runtime {
    config: ParallelEnvironment,
    placement: Option<topology::Placement>,
    pools: std::collections::BTreeMap<usize, Pool>,
    timings: Vec<u128>,
    calls: u64,
    selector: Option<selection::Selector>,
    selector_entry: Option<usize>,
    entries: std::collections::BTreeMap<usize, Option<usize>>,
}
impl Runtime {
    pub(super) fn reset_code(&mut self) {
        // Main is already pinned. Preserve the admission mask discovered before
        // pinning so a tier replacement does not mistake it for a one-CPU host.
        let placement = self.placement.take();
        let config = self.config.clone();
        *self = Self {
            config,
            placement,
            ..Self::default()
        };
    }
}

fn parse_width(mode: &str) -> Result<Option<usize>, String> {
    match mode {
        "auto" => Ok(None),
        "seq" | "profile" => Ok(Some(1)),
        _ => mode.strip_prefix("spin").unwrap_or(mode).parse::<usize>().ok()
            .filter(|&n| n > 0).map(Some)
            .ok_or_else(|| format!("invalid CELOX_PARALLEL_RUNTIME={mode:?}; use off, auto, seq, profile, or spinN (N > 0)")),
    }
}

fn useful_width(code: &SharedNativeCode, entry: usize) -> usize {
    let Some(blob) = code
        .program_image
        .code_entries
        .iter()
        .find(|b| b.offset == entry)
    else {
        return 1;
    };
    let mut waves = std::collections::BTreeMap::<usize, usize>::new();
    for symbol in &code.program_image.symbols {
        if symbol.offset < entry || symbol.offset >= entry + blob.size {
            continue;
        }
        if let Some(wave) = symbol
            .name
            .split_once("parallel-group/")
            .and_then(|(_, s)| s.split('/').nth(1))
            .and_then(|s| s.parse::<usize>().ok())
        {
            *waves.entry(wave).or_default() += 1;
        }
    }
    waves.into_values().max().unwrap_or(1)
}

impl NativeBackend {
    pub(super) fn parallel_run_many(
        &mut self,
        event: NativeEventRef,
        count: u64,
    ) -> Option<(u64, Result<(), SimulatorErrorCode>)> {
        if count == 0 {
            return Some((0, Ok(())));
        }
        let mode = self
            .parallel
            .config
            .get("CELOX_PARALLEL")
            .or_else(|| self.parallel.config.get("CELOX_PARALLEL_RUNTIME"))
            .unwrap_or("auto");
        if matches!(mode, "off" | "baseline") {
            self.parallel.pools.clear();
            return None;
        }
        let requested = parse_width(mode).unwrap_or_else(|e| panic!("{e}"));
        let serial_entry = self
            .compiled
            .program_image
            .event_map
            .get(&event.addr)?
            .comb_apply_offset;
        let parallel_entry = *self
            .parallel
            .entries
            .entry(serial_entry)
            .or_insert_with(|| {
                let marker = format!("{ENTRY_PREFIX}{serial_entry}");
                self.compiled
                    .program_image
                    .symbols
                    .iter()
                    .find(|s| s.name == marker)
                    .map(|s| s.offset)
            });
        let entry = parallel_entry.unwrap_or(serial_entry);
        if parallel_entry.is_none() {
            self.parallel.pools.clear();
            assert!(
                mode == "auto"
                    || self
                        .compiled
                        .program_image
                        .symbols
                        .iter()
                        .any(|s| s.name.starts_with(ENTRY_PREFIX)),
                "fixed parallel execution requires a native image containing the parallel alternative"
            );
            // An image can contain event-local alternatives for only some clocks.
            // Other events still use their original serial entry.
            return None;
        }
        if self.parallel.selector_entry != Some(serial_entry) {
            self.parallel.selector = None;
            self.parallel.selector_entry = Some(serial_entry);
        }
        let auto = mode == "auto";
        let placement = self.parallel.placement.get_or_insert_with(|| {
            let placement = topology::Placement::discover(&self.parallel.config);
            topology::pin(placement.target(0));
            placement
        });
        let cpu_limit = placement.cpus.len();
        let width = if auto {
            self.parallel
                .selector
                .get_or_insert_with(|| {
                    let param = |name, default| {
                        self.parallel.config.number(name, default)
                    };
                    let useful_width = useful_width(&self.compiled, entry).min(cpu_limit).max(1);
                    report!("CELOX_PARALLEL_AUTO max_useful_width={useful_width}");
                    let mut selector = selection::Selector::new(
                        param("CELOX_PARALLEL_WINDOW", 2048),
                        param("CELOX_PARALLEL_COOLDOWN", 1_000_000),
                    )
                    .configure(
                        (param("CELOX_PARALLEL_MAX_WORKERS", useful_width as u64) as usize)
                            .min(useful_width),
                        u128::from(param("CELOX_PARALLEL_MIN_GAIN_PERCENT", 10)),
                        self.parallel.config.get("CELOX_PARALLEL_EXPECTED_CALLS")
                            .and_then(|s| s.parse().ok()),
                        u128::from(param("CELOX_PARALLEL_INVESTMENT_NS", 0)),
                    )
                    // Reprobing rebuilds pools and advances real cycles. Keep
                    // early slowdown probes opt-in; a slowdown does not itself
                    // establish that a different width would be faster.
                    .with_hold_guard(u128::from(param("CELOX_PARALLEL_HOLD_SLOWDOWN_PERCENT", 0)))
                    .with_warmup(param("CELOX_PARALLEL_WARMUP", 64));
                    if let Some(value) = self.parallel.config.get("CELOX_PARALLEL_CANDIDATES") {
                        let widths = value.split(',').map(|w| w.trim().parse::<usize>())
                            .collect::<Result<Vec<_>, _>>()
                            .expect("CELOX_PARALLEL_CANDIDATES requires comma-separated positive thread counts");
                        selector = selector.with_candidates(&widths);
                    }
                    report!("CELOX_PARALLEL_AUTO configured={selector:?}");
                    selector
                })
                .width()
        } else {
            requested.expect("fixed mode has a width")
        };
        assert!(
            width <= cpu_limit,
            "requested {width} threads but only {cpu_limit} CPUs admitted"
        );
        let measure_auto = auto
            && self
                .parallel
                .selector
                .as_ref()
                .is_some_and(selection::Selector::needs_timing);
        // Never leave spin workers alive during an optimized-serial sample.
        if auto && width == 1 {
            let start = measure_auto.then(Instant::now);
            self.parallel.pools.clear();
            let limit = count.min(self.parallel.selector.as_ref().unwrap().batch_limit());
            let (completed, result) = if self.compiled.options.native_tick_loop {
                self.call_func_many_timed(event.comb_apply_func, limit)
            } else {
                (1, self.call_func_timed(event.comb_apply_func))
            };
            if let Some(selected) = self.parallel.selector.as_mut().unwrap().observe_many(
                start.map_or(0, |start| start.elapsed().as_nanos()),
                completed,
            ) {
                report!(
                    "parallel auto: selected {selected} worker(s), profile {:?}",
                    self.parallel.selector.as_ref().unwrap()
                );
            }
            return Some((completed, result));
        }
        let start = measure_auto.then(Instant::now);
        if self
            .parallel
            .pools
            .get(&entry)
            .is_none_or(|p| p.width != width)
        {
            self.parallel.pools.clear();
            let placement = self.parallel.placement.as_ref().unwrap();
            topology::pin(placement.target(0));
            report!(
                "CELOX_PARALLEL_POOL width={width} cpus={:?}",
                &placement.cpus[..width]
            );
            let pool = Pool::new(self.compiled.clone(), entry, width, placement)
                .expect("parallel alternative has no executable parallel groups");
            self.parallel.pools.insert(entry, pool);
        }
        let pool = &self.parallel.pools[&entry];
        self.parallel.calls += 1;
        let kernel_start = self.execution_timing.is_some().then(Instant::now);
        let result = pool.run(
            self.memory.as_mut_slice().as_mut_ptr() as *mut u8,
            mode == "profile",
            &mut self.parallel.timings,
        );
        if let Some(kernel_start) = kernel_start {
            let timing = self.execution_timing.as_mut().unwrap();
            timing.elapsed = timing.elapsed.saturating_add(kernel_start.elapsed());
            timing.calls = timing.calls.saturating_add(1);
        }
        if auto {
            if let Some(selected) = self
                .parallel
                .selector
                .as_mut()
                .unwrap()
                .observe(start.map_or(0, |start| start.elapsed().as_nanos()))
            {
                report!(
                    "parallel auto: selected {selected} worker(s), profile {:?}",
                    self.parallel.selector.as_ref().unwrap()
                );
            }
        }
        Some((
            1,
            match result {
                0 => Ok(()),
                c if c > 0 => Err(SimulatorErrorCode::DetectedTrueLoopCode(c)),
                _ => Err(SimulatorErrorCode::InternalError),
            },
        ))
    }
}
impl Drop for NativeBackend {
    fn drop(&mut self) {
        // Workers must have stopped before memory or JIT allocations disappear.
        self.parallel.pools.clear();
        if let Some(path) = self.parallel.config.get("CELOX_PARALLEL_TIMINGS") {
            let lines = self
                .parallel
                .timings
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{i}\t{}\t{t}", self.parallel.calls))
                .collect::<Vec<_>>();
            std::fs::write(path, lines.join("\n")).unwrap();
        }
        if let Some(path) = self
            .parallel
            .config
            .get("CELOX_PARALLEL_SNAPSHOT")
            .map(str::to_owned)
        {
            if self.eval_comb().is_err() {
                return;
            }
            let mut lines = self
                .compiled
                .program_image
                .reflection
                .signals()
                .iter()
                .filter(|s| {
                    s.direction == celox_runtime::SignalDirection::Output
                        || s.full_name.matches('.').count() == 1
                })
                .map(|s| format!("{}\t{:x}", s.full_name, self.get(s.signal)))
                .collect::<Vec<_>>();
            lines.sort();
            std::fs::write(path, lines.join("\n")).unwrap();
        }
    }
}

#[cfg(test)]
mod runtime_options_tests {
    use super::{empty_group, overlap, parse_width};

    #[test]
    fn final_access_waves_preserve_conflicts_and_unknown_fences() {
        use super::{Access, schedule_waves};
        let known = |reads, writes| Access::from_ranges(reads, writes);
        let accesses = [
            known(vec![], vec![(0, 8)]),
            known(vec![], vec![(8, 8)]),
            known(vec![(7, 2)], vec![]),
            Access {
                effects: true,
                ..Access::default()
            },
            known(vec![], vec![(16, 8)]),
            known(vec![], vec![(24, 8)]),
        ];
        assert_eq!(schedule_waves(&accesses), [0, 0, 1, 2, 3, 3]);
    }

    #[test]
    fn barrier_publishes_all_participants_across_generations() {
        use super::{Barrier, Counter};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let barrier = Barrier {
            count: Counter(AtomicUsize::new(0)),
            generation: Counter(AtomicUsize::new(0)),
            n: 16,
        };
        let slots = std::array::from_fn::<_, 16, _>(|_| AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for worker in 0..16 {
                let barrier = &barrier;
                let slots = &slots;
                scope.spawn(move || {
                    for round in 1..=128 {
                        slots[worker].store(round, Ordering::Relaxed);
                        barrier.wait();
                        assert!(slots.iter().all(|s| s.load(Ordering::Relaxed) == round));
                        barrier.wait();
                    }
                });
            }
        });
    }

    #[test]
    #[cfg(all(
        target_os = "linux",
        target_arch = "x86_64",
        not(feature = "arm64-codegen")
    ))]
    fn aliased_publications_survive_local_promotion() {
        use super::*;
        use celox_design::{InstanceId, StateObjectId};
        use celox_sir::{BasicBlock, BinaryOp, RegisterType, SIRValue};
        use celox_state_layout::{
            LayoutInput, LayoutRequirements, LayoutSource, MemoryLayoutMode, StateObjectLayout,
        };
        let addr = |id| RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(0),
            var_id: StateObjectId(id),
        };
        struct Source([AbsoluteAddr; 2]);
        impl LayoutSource<AbsoluteAddr> for Source {
            fn layout_input(&self, _: MemoryLayoutMode) -> LayoutInput<AbsoluteAddr> {
                let mut requirements = LayoutRequirements::default();
                requirements
                    .state_aliases_mut()
                    .insert(self.0[1], self.0[0]);
                LayoutInput {
                    state_objects: self
                        .0
                        .iter()
                        .map(|&address| StateObjectLayout {
                            address,
                            width: 64,
                            is_4state: false,
                        })
                        .collect(),
                    working_addresses: vec![],
                    sparse_addresses: vec![],
                    unpacked_arrays: Default::default(),
                    requirements,
                    ff_referenced_addresses: Default::default(),
                    num_events: 0,
                    runtime_event_sites: vec![],
                }
            }
        }
        let layout = MemoryLayout::build(
            &Source([addr(0).absolute_addr(), addr(1).absolute_addr()]),
            false,
            MemoryLayoutMode::Packed,
        );
        assert_eq!(
            layout.offsets[&addr(0).absolute_addr()],
            layout.offsets[&addr(1).absolute_addr()]
        );
        for separate in [true, false] {
            let writer = vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(42u8)),
                SIRInstruction::Store(
                    addr(0),
                    SIROffset::Static(0),
                    64,
                    RegisterId(0),
                    vec![],
                    vec![],
                ),
                // This logical load makes the slot eligible for local promotion.
                SIRInstruction::Load(RegisterId(1), addr(0), SIROffset::Static(0), 64),
            ];
            let mut check = if separate { vec![] } else { writer.clone() };
            check.extend([
                SIRInstruction::Load(RegisterId(2), addr(1), SIROffset::Static(0), 64),
                SIRInstruction::Imm(RegisterId(3), SIRValue::new(42u8)),
                SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::Eq, RegisterId(3)),
            ]);
            let unit = |instructions, verify| ExecutionUnit {
                entry_block_id: BlockId(0),
                register_map: (0..5)
                    .map(|i| {
                        (
                            RegisterId(i),
                            RegisterType::Bit {
                                width: if i == 4 { 1 } else { 64 },
                                signed: false,
                            },
                        )
                    })
                    .collect(),
                blocks: [
                    BasicBlock {
                        id: BlockId(0),
                        params: vec![],
                        instructions,
                        terminator: if verify {
                            SIRTerminator::Branch {
                                cond: RegisterId(4),
                                true_block: (BlockId(1), vec![]),
                                false_block: (BlockId(2), vec![]),
                            }
                        } else {
                            SIRTerminator::Return
                        },
                    },
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![],
                        instructions: vec![],
                        terminator: SIRTerminator::Return,
                    },
                    BasicBlock {
                        id: BlockId(2),
                        params: vec![],
                        instructions: vec![],
                        terminator: SIRTerminator::Error(7),
                    },
                ]
                .into_iter()
                .filter(|b| verify || b.id == BlockId(0))
                .map(|b| (b.id, b))
                .collect(),
            };
            let writer = unit(writer, false);
            let reader = unit(check, true);
            let units = if separate {
                vec![&writer, &reader]
            } else {
                vec![&reader]
            };
            let compiled = compile(
                &units,
                &layout,
                false,
                &Default::default(),
                &Default::default(),
                None,
            )
            .unwrap();
            let jit = jit_mem::JitCode::new(&compiled.code).unwrap();
            let mut state =
                vec![0u8; compiled.required_state_size.max(layout.merged_total_size) + 64];
            assert_eq!(
                unsafe { jit.call(&mut state) },
                0,
                "alias load must see the publication (separate={separate})"
            );
        }
    }

    #[test]
    fn error_only_group_is_not_discarded_as_empty() {
        use crate::ir::RegionedAbsoluteAddr;
        use celox_sir::{BasicBlock, BlockId, ExecutionUnit, SIRTerminator};
        let mut unit = ExecutionUnit::<RegionedAbsoluteAddr> {
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            entry_block_id: BlockId(0),
            register_map: Default::default(),
        };
        assert!(empty_group(&unit));
        unit.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Error(7);
        assert!(!empty_group(&unit));
    }

    #[test]
    fn sorted_access_intersection_matches_pairwise_reference() {
        for seed in 0..128usize {
            let a: std::collections::BTreeSet<(usize, usize)> = (0..17)
                .map(|i| ((i * 13 + seed) % 71, (i + seed) % 9))
                .collect();
            let b: std::collections::BTreeSet<(usize, usize)> = (0..23)
                .map(|i| ((i * 7 + seed * 3) % 79, (i * 3 + seed) % 11))
                .collect();
            let expected = a
                .iter()
                .any(|&(x, n)| n > 0 && b.iter().any(|&(y, m)| m > 0 && x < y + m && y < x + n));
            assert_eq!(overlap(&a, &b), expected);
        }
    }

    #[test]
    fn explicit_thread_counts_do_not_silently_fall_back_to_serial() {
        assert_eq!(parse_width("spin8").unwrap(), Some(8));
        assert_eq!(parse_width("spin16").unwrap(), Some(16));
        assert_eq!(parse_width("spin6").unwrap(), Some(6));
        assert_eq!(parse_width("auto").unwrap(), None);
        for mode in ["spin0", "spin", "spin-2", "spni8", ""] {
            assert!(parse_width(mode).is_err());
        }
    }
}
