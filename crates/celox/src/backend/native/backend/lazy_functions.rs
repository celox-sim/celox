//! Function units compiled at their first call.
//!
//! A program loaded in the process that compiled it keeps its function units
//! ([`crate::ir::SirProgram::functions`]) out of the image: each one's extern
//! address table slot first reaches a stub, which compiles the unit with the
//! pipeline and options the image would have compiled it with, installs the
//! code, points the slot at it and continues the call. Later calls go
//! straight to the code, so a unit that is never called is never compiled.
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
/// One thread compiles them, started at the first call: the caller's stack
/// may be deep in nested generated calls, and the compile thread has the
/// stack an eager compile worker has. The caller hands it the unit and waits
/// for the code.
pub(crate) struct LazyFunctions {
    compiler: Arc<Compiler>,
    /// The first extern address table slot of the function units.
    first_slot: usize,
}

/// What the compile thread shares with the threads that call units.
#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: generated code on any thread, lane workers included, requests units of the compile thread and waits for their code"
)]
struct Compiler {
    program: Arc<LaidOutProgram>,
    /// The options the image compiled with, with the units' frames.
    options: SimulatorOptions,
    native_memory_size: usize,
    units: Box<[OnceLock<Result<LazyCode, String>>]>,
    requests: Mutex<Requests>,
    /// Signalled when a request arrives or the compile thread is to stop.
    requested: Condvar,
    compiled: AtomicU64,
    compile_nanos: AtomicU64,
    #[cfg(test)]
    fail_compiles: bool,
}

#[derive(Default)]
struct Requests {
    /// The units callers wait for, in the order the thread takes them.
    units: VecDeque<usize>,
    stopped: bool,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LazyFunctions {
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the state of `LazyFunctions`, which threads running generated code share with its compile thread"
    )]
    pub(super) fn new(
        program: Arc<LaidOutProgram>,
        options: SimulatorOptions,
        native_memory_size: usize,
    ) -> Self {
        let first_slot = program.runtime().runtime_schema.extern_functions.len();
        let units = (0..program.sir.functions.len())
            .map(|_| OnceLock::new())
            .collect();
        Self {
            compiler: Arc::new(Compiler {
                program,
                options,
                native_memory_size,
                units,
                requests: Mutex::new(Requests::default()),
                requested: Condvar::new(),
                compiled: AtomicU64::new(0),
                compile_nanos: AtomicU64::new(0),
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

    /// The error of the first unit that failed to compile.
    pub(crate) fn failure(&self) -> Option<String> {
        self.compiler
            .units
            .iter()
            .enumerate()
            .find_map(|(index, unit)| {
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

    /// The entry of unit `index`, which the compile thread compiles now
    /// unless it already did.
    fn entry(&self, index: usize) -> Option<usize> {
        let unit = self.compiler.units.get(index)?;
        let code = match unit.get() {
            Some(code) => code,
            None => {
                Compiler::request(&self.compiler, index);
                unit.wait()
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

    /// Have the compile thread compile unit `index`, starting the thread at
    /// the first request. The unit holds an error if the thread cannot
    /// start.
    fn request(this: &Arc<Self>, index: usize) {
        let mut requests = this.lock();
        if requests.thread.is_none() {
            let compiler = Arc::clone(this);
            match std::thread::Builder::new()
                .name("celox-lazy-compile".into())
                .spawn(move || compiler.work())
            {
                Ok(thread) => requests.thread = Some(thread),
                Err(error) => {
                    drop(requests);
                    let _ = this.units[index]
                        .set(Err(format!("the compile thread did not start: {error}")));
                    return;
                }
            }
        }
        requests.units.push_back(index);
        drop(requests);
        this.requested.notify_one();
    }

    /// The compile thread: compile requested units until stopped. It keeps
    /// a copy of the program's layout for the units to place their frame
    /// slots in.
    fn work(&self) {
        let mut layout = None;
        while let Some(index) = self.next_request() {
            self.units[index].get_or_init(|| self.compile(index, &mut layout));
        }
    }

    fn next_request(&self) -> Option<usize> {
        let mut requests = self.lock();
        loop {
            if requests.stopped {
                return None;
            }
            if let Some(index) = requests.units.pop_front() {
                return Some(index);
            }
            requests = self
                .requested
                .wait(requests)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Stop the compile thread and wait for it.
    fn stop(&self) {
        let thread = {
            let mut requests = self.lock();
            requests.stopped = true;
            requests.units.clear();
            requests.thread.take()
        };
        self.requested.notify_all();
        if let Some(thread) = thread {
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
        let compiled = compile_function_unit(&self.program, layout, &self.options, index, None)
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
            tracing::debug!(
                "[phase-timing] lazy_function_compile: {}/{} units, {:?}",
                self.compiled(),
                self.len(),
                Duration::from_nanos(self.compiler.compile_nanos.load(Ordering::Relaxed))
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
