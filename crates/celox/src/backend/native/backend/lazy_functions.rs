//! Function units compiled at their first call.
//!
//! A program loaded in the process that compiled it keeps its function units
//! ([`crate::ir::SirProgram::functions`]) out of the image: each one's extern
//! address table slot first reaches a stub, which has the unit compiled with
//! the pipeline and options the image would have compiled it with, installs
//! the code, points the slot at it and continues the call. Later calls go
//! straight to the code. Meanwhile the units the called unit calls compile
//! too; a unit that neither the simulation nor a called unit calls is never
//! compiled.
//!
//! The image then holds [`LAZY_FUNCTION_OFFSET`] in those slots' kernel
//! offsets. It cannot be loaded again without the program it came from:
//! [`NativeProgramImage::validate`] rejects it. Images that are written out
//! or loaded again ([`NativeBackend::compile_image`]) compile every unit.
//!
//! A failed compile returns [`NATIVE_FUNCTION_COMPILE_FAILED`] as the call's
//! status, which propagates to the entry the simulation called;
//! [`LazyFunctions::failure`] gives the error.

use super::*;

use std::collections::VecDeque;
use std::sync::Condvar;
#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: the compile requests that threads running generated code hand the compile thread"
)]
use std::sync::Mutex;
#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: the compile thread fills a unit's code while generated code on the threads that called it waits for it"
)]
use std::sync::OnceLock;
#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: compile statistics the compile thread sums and the simulation reads"
)]
use std::sync::atomic::AtomicU64;

/// The bytes of one stub: `mov edx, index; mov rcx, functions; mov rax,
/// entry; jmp rax`, padded with `int3`.
const STUB_SIZE: usize = 32;

struct LazyCode {
    /// Keeps the executable mapping alive while the table points into it.
    _code: jit_mem::JitCode,
    entry: usize,
}

/// The function units of a loaded program that compile at their first
/// call, and the code of those that did.
///
/// Compile threads, started at the first call, compile them: the caller's
/// stack may be deep in nested generated calls, and a compile thread has
/// the stack an eager compile worker has. The caller hands one the unit and
/// waits for its code. The other threads meanwhile compile the units the
/// called unit calls, which its caller is likely to call next; a call of a
/// unit such a thread is compiling waits for that compile.
pub(crate) struct LazyFunctions {
    compiler: Arc<Compiler>,
    /// The first extern address table slot of the function units.
    first_slot: usize,
}

/// What the compile threads share with the threads that call units.
#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: generated code on any thread, lane workers included, requests units of the compile threads and waits for their code"
)]
struct Compiler {
    program: Arc<LaidOutProgram>,
    /// The options the image compiled with, with the units' frames.
    options: SimulatorOptions,
    native_memory_size: usize,
    units: Box<[OnceLock<Result<LazyCode, String>>]>,
    requests: Mutex<Requests>,
    /// Signalled when a request arrives or the compile threads are to stop.
    requested: Condvar,
    /// Stops the compiles running when the functions are dropped.
    cancel: CompileCancel,
    compiled: AtomicU64,
    compile_nanos: AtomicU64,
    /// How long callers waited for code, in all.
    wait_nanos: AtomicU64,
    #[cfg(test)]
    fail_compiles: bool,
}

struct Requests {
    /// The units callers wait for, in the order the threads take them.
    called: VecDeque<usize>,
    /// The units called units call, the latest at the back, where the
    /// threads take them: the callees of the latest call are the likeliest
    /// to be called next.
    callees: Vec<usize>,
    /// What a unit's requests were so far.
    states: Box<[UnitState]>,
    stopped: bool,
    threads: Vec<std::thread::JoinHandle<()>>,
}

#[derive(Clone, Copy, Default)]
struct UnitState {
    /// A unit that called it was called.
    callee: bool,
    called: bool,
    /// A compile of the callees took it before it was called.
    speculative: bool,
}

impl LazyFunctions {
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the state of `LazyFunctions`, which threads running generated code share with its compile threads"
    )]
    pub(super) fn new(
        program: Arc<LaidOutProgram>,
        options: SimulatorOptions,
        native_memory_size: usize,
    ) -> Self {
        let first_slot = program.runtime().runtime_schema.extern_functions.len();
        let len = program.sir.functions.len();
        let units = (0..len).map(|_| OnceLock::new()).collect();
        Self {
            compiler: Arc::new(Compiler {
                program,
                options,
                native_memory_size,
                units,
                requests: Mutex::new(Requests {
                    called: VecDeque::new(),
                    callees: Vec::new(),
                    states: vec![UnitState::default(); len].into_boxed_slice(),
                    stopped: false,
                    threads: Vec::new(),
                }),
                requested: Condvar::new(),
                cancel: CompileCancel::new(),
                compiled: AtomicU64::new(0),
                compile_nanos: AtomicU64::new(0),
                wait_nanos: AtomicU64::new(0),
                #[cfg(test)]
                fail_compiles: false,
            }),
            first_slot,
        }
    }

    /// Make every compile fail, for tests of how that failure surfaces.
    #[cfg(test)]
    pub(super) fn fail_compiles(&mut self) {
        Arc::get_mut(&mut self.compiler)
            .expect("no compile thread before the first call")
            .fail_compiles = true;
    }

    /// The number of function units, which take the image's first kernel
    /// slots.
    pub(crate) fn len(&self) -> usize {
        self.compiler.units.len()
    }

    /// The address the table slot of unit `index` holds before its first
    /// call: its stub in `stubs`, from [`Self::stubs`].
    pub(super) fn stub(stubs: &jit_mem::JitCode, index: usize) -> Result<usize, SimulatorError> {
        index
            .checked_mul(STUB_SIZE)
            .and_then(|offset| stubs.entry_ptr(offset))
            .map(|entry| entry as usize)
            .ok_or_else(|| codegen_message("function unit stub outside its code"))
    }

    /// The machine code of unit `index`, once it compiled.
    #[cfg(test)]
    pub(crate) fn code(&self, index: usize) -> Option<&[u8]> {
        let code = self.compiler.units.get(index)?.get()?.as_ref().ok()?;
        Some(code._code.image())
    }

    /// The number of units compiled so far.
    pub(crate) fn compiled(&self) -> u64 {
        self.compiler.compiled.load(Ordering::Relaxed)
    }

    /// The number of compile threads started.
    #[cfg(test)]
    pub(crate) fn compile_threads(&self) -> usize {
        self.compiler.lock().threads.len()
    }

    /// The error of the first called unit that failed to compile. A unit
    /// compiled for its caller that fails holds its error until it is
    /// called.
    pub(crate) fn failure(&self) -> Option<String> {
        let requests = self.compiler.lock();
        self.compiler
            .units
            .iter()
            .zip(&requests.states)
            .enumerate()
            .filter(|(_, (_, state))| state.called)
            .find_map(|(index, (unit, _))| {
                unit.get()?.as_ref().err().map(|error| {
                    format!("native code generation of function[{index}] failed: {error}")
                })
            })
    }

    /// The stubs of the units, one per unit in slot order.
    pub(super) fn stubs(
        self: &Arc<Self>,
        perf_map: bool,
    ) -> Result<jit_mem::JitCode, SimulatorError> {
        let context = Arc::as_ptr(self) as u64;
        let entry = enter_lazy_function as *const () as u64;
        let mut code = Vec::with_capacity(self.len() * STUB_SIZE);
        for index in 0..self.len() {
            let index =
                u32::try_from(index).map_err(|_| codegen_message("too many function units"))?;
            let start = code.len();
            // mov edx, imm32
            code.push(0xba);
            code.extend_from_slice(&index.to_le_bytes());
            // mov rcx, imm64
            code.extend_from_slice(&[0x48, 0xb9]);
            code.extend_from_slice(&context.to_le_bytes());
            // mov rax, imm64
            code.extend_from_slice(&[0x48, 0xb8]);
            code.extend_from_slice(&entry.to_le_bytes());
            // jmp rax
            code.extend_from_slice(&[0xff, 0xe0]);
            code.resize(start + STUB_SIZE, 0xcc);
        }
        jit_mem::JitCode::new_named_profiled(&code, "celox_lazy_function_stubs", perf_map)
            .map_err(|source| codegen_err(CodegenError::NativeMemory { source }))
    }

    /// The entry of unit `index`, which a compile thread compiles now unless
    /// one already did.
    fn entry(&self, index: usize) -> Option<usize> {
        let unit = self.compiler.units.get(index)?;
        Compiler::call(&self.compiler, index);
        let code = match unit.get() {
            Some(code) => code,
            None => {
                let start = Instant::now();
                let code = unit.wait();
                self.compiler.wait_nanos.fetch_add(
                    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                code
            }
        };
        code.as_ref().ok().map(|code| code.entry)
    }
}

impl Compiler {
    fn lock(&self) -> std::sync::MutexGuard<'_, Requests> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The units unit `index` calls, each once, in the order of its blocks.
    fn callees(&self, index: usize) -> Vec<usize> {
        let unit = &self.program.sir.functions[index].unit;
        let mut blocks = unit.blocks.iter().collect::<Vec<_>>();
        blocks.sort_unstable_by_key(|(id, _)| id.0);
        let mut callees = Vec::new();
        for (_, block) in blocks {
            for instruction in &block.instructions {
                if let celox_sir::SIRInstruction::Call { function, .. } = instruction {
                    let callee = function.0 as usize;
                    if callee < self.units.len() && !callees.contains(&callee) {
                        callees.push(callee);
                    }
                }
            }
        }
        callees
    }

    /// Record a call of unit `index`: have a compile thread compile it
    /// unless it is compiled, and the other threads the units it calls. The
    /// threads start at the first call; the unit holds an error if none
    /// starts.
    fn call(this: &Arc<Self>, index: usize) {
        let mut requests = this.lock();
        if std::mem::replace(&mut requests.states[index].called, true) {
            return;
        }
        if requests.threads.is_empty() {
            let threads = this.threads();
            for speculates in std::iter::once(false).chain((1..threads).map(|_| true)) {
                let compiler = Arc::clone(this);
                match std::thread::Builder::new()
                    .name("celox-lazy-compile".into())
                    .spawn(move || compiler.work(speculates))
                {
                    Ok(thread) => requests.threads.push(thread),
                    Err(error) if requests.threads.is_empty() => {
                        drop(requests);
                        let _ = this.units[index]
                            .set(Err(format!("no compile thread started: {error}")));
                        return;
                    }
                    Err(_) => break,
                }
            }
        }
        let compiled = this.units[index].get().is_some();
        if !compiled {
            requests.called.push_back(index);
        }
        if requests.threads.len() > 1 {
            // The callees of this call before those of earlier ones; its
            // first callee first.
            for callee in this.callees(index).into_iter().rev() {
                let state = &mut requests.states[callee];
                if !state.callee && !state.called && this.units[callee].get().is_none() {
                    state.callee = true;
                    requests.callees.push(callee);
                }
            }
        }
        drop(requests);
        this.requested.notify_all();
    }

    /// How many compile threads to start: those an eager compile of the
    /// units would use, one of which compiles only called units.
    fn threads(&self) -> usize {
        let functions = &self.program.sir.functions;
        let (blocks, instructions) = functions
            .iter()
            .map(|function| {
                (
                    function.unit.blocks.len(),
                    function
                        .unit
                        .blocks
                        .values()
                        .map(|block| block.instructions.len())
                        .sum::<usize>(),
                )
            })
            .fold((0, 0), |(blocks, instructions), (b, i)| {
                (blocks.max(b), instructions.max(i))
            });
        super::super::compile_resources::workers_with_state(
            self.options.optimize_options.compile_threads(),
            blocks,
            instructions,
            functions.len(),
            super::super::compile_resources::layout_bytes(self.program.layout()),
        )
        .max(1)
    }

    /// A compile thread: compile requested units until stopped. It keeps a
    /// copy of the program's layout for the units to place their frame
    /// slots in. One that `speculates` also compiles the callees of called
    /// units while no called unit waits.
    fn work(&self, speculates: bool) {
        let mut layout = None;
        while let Some(index) = self.next_request(speculates) {
            self.units[index].get_or_init(|| self.compile(index, &mut layout));
        }
    }

    fn next_request(&self, speculates: bool) -> Option<usize> {
        let mut requests = self.lock();
        loop {
            if requests.stopped {
                return None;
            }
            if let Some(index) = requests.called.pop_front() {
                return Some(index);
            }
            if speculates {
                while let Some(index) = requests.callees.pop() {
                    let state = &mut requests.states[index];
                    if !state.called && self.units[index].get().is_none() {
                        state.speculative = true;
                        return Some(index);
                    }
                }
            }
            requests = self
                .requested
                .wait(requests)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Stop the compile threads and wait for them; compiles running stop.
    fn stop(&self) {
        let threads = {
            let mut requests = self.lock();
            requests.stopped = true;
            requests.called.clear();
            requests.callees.clear();
            std::mem::take(&mut requests.threads)
        };
        self.cancel.cancel();
        self.requested.notify_all();
        for thread in threads {
            let _ = thread.join();
        }
    }

    fn compile(&self, index: usize, layout: &mut Option<MemoryLayout>) -> Result<LazyCode, String> {
        let start = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.compile_code(index, layout)
        }))
        .unwrap_or_else(|_| {
            // The layout may still hold the unit's frame slots.
            *layout = None;
            Err("native function compile thread panicked".into())
        });
        self.compile_nanos.fetch_add(
            u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.compiled.fetch_add(1, Ordering::Relaxed);
        result
    }

    fn compile_code(
        &self,
        index: usize,
        layout: &mut Option<MemoryLayout>,
    ) -> Result<LazyCode, String> {
        #[cfg(test)]
        if self.fail_compiles {
            return Err("compile failure injected by a test".into());
        }
        let layout = layout.get_or_insert_with(|| self.program.layout().clone());
        let compiled = compile_function_unit(
            &self.program,
            layout,
            &self.options,
            index,
            Some(&self.cancel),
        )
        .map_err(|error| error.to_string())?;
        if compiled.required_state_size > self.native_memory_size {
            return Err(format!(
                "the unit needs {} bytes of native state, the image allocates {}",
                compiled.required_state_size, self.native_memory_size
            ));
        }
        let unavailable = compiled.required_native_features & !current_native_feature_bits();
        if unavailable != 0 {
            return Err(format!(
                "the unit requires unavailable host features: {}",
                format_native_feature_bits(unavailable)
            ));
        }
        let code = jit_mem::JitCode::new_named_with_symbols_profiled(
            &compiled.code,
            &format!("function[{index}]"),
            &compiled.symbols,
            self.options.x86_options.diagnostics.perf_map,
        )
        .map_err(|error| format!("executable allocation: {error}"))?;
        let entry = code.fn_ptr as usize;
        Ok(LazyCode { _code: code, entry })
    }
}

impl Drop for LazyFunctions {
    fn drop(&mut self) {
        self.compiler.stop();
        if self.compiler.options.x86_options.diagnostics.phase_timing {
            let requests = self.compiler.lock();
            let speculative = requests.states.iter().filter(|state| state.speculative);
            let unused = speculative.clone().filter(|state| !state.called).count();
            tracing::debug!(
                "[phase-timing] lazy_function_compile: {}/{} units, {:?}, {} for callers before their call ({unused} never called), callers waited {:?}",
                self.compiled(),
                self.len(),
                Duration::from_nanos(self.compiler.compile_nanos.load(Ordering::Relaxed)),
                speculative.count(),
                Duration::from_nanos(self.compiler.wait_nanos.load(Ordering::Relaxed)),
            );
        }
    }
}

/// Where a function unit's stub jumps: compile unit `index` of `functions`,
/// point the slot of the table `state` uses at it, and run the call.
///
/// # Safety
///
/// Generated code calls it through a stub of [`LazyFunctions::stubs`], with
/// the state pointer and the frame address of a function call, while the
/// [`LazyFunctions`] the stub names is alive.
#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: the extern address table's slots, which generated code on any thread reads"
)]
unsafe extern "sysv64" fn enter_lazy_function(
    state: *mut u8,
    frame: *mut u8,
    index: u64,
    functions: *const LazyFunctions,
) -> i64 {
    // SAFETY: the stub passes the live functions of its image.
    let functions = unsafe { &*functions };
    let entry = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        functions.entry(usize::try_from(index).ok()?)
    }));
    let Ok(Some(entry)) = entry else {
        return NATIVE_FUNCTION_COMPILE_FAILED;
    };
    // SAFETY: the header holds the extern address table the caller called
    // through, whose function unit slots are atomic words.
    unsafe {
        let table = state
            .add(celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET)
            .cast::<*const AtomicUsize>()
            .read_unaligned();
        (*table.add(functions.first_slot + index as usize)).store(entry, Ordering::Release);
    }
    // SAFETY: the entry is the unit's code, compiled for this ABI: the state
    // pointer and the frame address, and the status as the result.
    let code = unsafe {
        std::mem::transmute::<usize, unsafe extern "sysv64" fn(*mut u8, *mut u8) -> i64>(entry)
    };
    unsafe { code(state, frame) }
}
