use crate::{
    EventRef, IOContext, RuntimeErrorCode,
    backend::{JitBackend, MemoryLayout},
    ir::{InstancePath, Program, SignalRef, VariableInfo},
};
use malachite_bigint::BigUint;

mod builder;
mod error;

pub use builder::{SimulatorBuilder, SimulatorOptions};
pub use error::SimulatorError;

/// A named signal with its resolved memory reference and metadata.
#[derive(Debug, Clone)]
pub struct NamedSignal {
    pub name: String,
    pub signal: SignalRef,
    pub info: VariableInfo,
    /// For reset signals, the name of the associated clock (from FfDeclaration).
    pub associated_clock: Option<String>,
}

/// A named event with its resolved ID and event reference.
#[derive(Debug, Clone)]
pub struct NamedEvent {
    pub name: String,
    pub id: usize,
    pub event_ref: EventRef,
}

/// The core logic evaluation engine.
///
/// Encapsulates the JIT-compiled backend, the original SIR program,
/// and an optional VCD writer. Provides low-level, event-driven control.
pub struct Simulator {
    pub(crate) backend: JitBackend,
    pub(crate) program: Program,
    pub(crate) vcd_writer: Option<crate::vcd::VcdWriter>,
    pub(crate) dirty: bool,
}

impl std::fmt::Debug for Simulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Simulator").finish()
    }
}

impl Simulator {
    pub fn builder<'a>(code: &'a str, top: &'a str) -> SimulatorBuilder<'a, Simulator> {
        SimulatorBuilder::<Simulator>::new(code, top)
    }

    pub(crate) fn with_backend_and_program(backend: JitBackend, program: Program) -> Self {
        Self {
            backend,
            program,
            vcd_writer: None,
            dirty: false,
        }
    }

    /// Captures the current state of all signals and writes them to the VCD file.
    pub fn dump(&mut self, timestamp: u64) {
        if self.dirty {
            self.backend.eval_comb().unwrap();
            self.dirty = false;
        }
        if let Some(ref mut writer) = self.vcd_writer {
            let backend = &self.backend;
            writer
                .dump(timestamp, |addr| {
                    let signal = backend.resolve_signal(addr);
                    backend.get(signal)
                })
                .unwrap();
        }
    }

    /// Modifies internal state via a callback and marks combinational logic as dirty.
    pub fn modify<F>(&mut self, f: F) -> Result<(), RuntimeErrorCode>
    where
        F: FnOnce(&mut IOContext),
    {
        let mut ctx = IOContext {
            backend: &mut self.backend,
        };
        f(&mut ctx);
        self.dirty = true;
        Ok(())
    }

    /// Manually triggers a clock or event to process sequential logic.
    pub fn tick(&mut self, event: EventRef) -> Result<(), RuntimeErrorCode> {
        if self.dirty {
            self.backend.eval_comb()?;
        }
        self.backend.eval_apply_ff_at(event)?;
        self.backend.eval_comb()?;
        self.dirty = false;
        Ok(())
    }

    /// Resolves a signal path into a performance-optimized [`SignalRef`].
    /// This handle allows for direct memory access without `HashMap` lookups.
    pub fn signal(&self, path: &str) -> SignalRef {
        let addr = self.program.get_addr(&[], &[path]);
        self.backend.resolve_signal(&addr)
    }

    /// Resolve a port name to an [`EventRef`] handle.
    pub fn event(&self, port: &str) -> EventRef {
        let addr = self.program.get_addr(&[], &[port]);
        self.backend.resolve_event(&addr)
    }

    /// Retrieves the current value of a variable using a pre-resolved [`SignalRef`] handle.
    /// Lazily evaluates combinational logic if the state is dirty.
    pub fn get(&mut self, signal: SignalRef) -> BigUint {
        if self.dirty {
            self.backend.eval_comb().unwrap();
            self.dirty = false;
        }
        self.backend.get(signal)
    }

    /// Retrieves the current 4-state value (value, mask) of a variable using a [`SignalRef`] handle.
    /// Lazily evaluates combinational logic if the state is dirty.
    pub fn get_four_state(&mut self, signal: SignalRef) -> (BigUint, BigUint) {
        if self.dirty {
            self.backend.eval_comb().unwrap();
            self.dirty = false;
        }
        self.backend.get_four_state(signal)
    }

    /// Directly execute combinational logic evaluation.
    pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        self.backend.eval_comb()?;
        self.dirty = false;
        Ok(())
    }

    /// Returns a raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        self.backend.memory_as_ptr()
    }

    /// Returns a mutable raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        self.backend.memory_as_mut_ptr()
    }

    /// Returns the stable region size in bytes.
    pub fn stable_region_size(&self) -> usize {
        self.backend.stable_region_size()
    }

    /// Returns a reference to the memory layout.
    pub fn layout(&self) -> &MemoryLayout {
        self.backend.layout()
    }

    /// Returns all ports of the top-level module with their resolved signal references.
    pub fn named_signals(&self) -> Vec<NamedSignal> {
        let top_instance_id = self
            .program
            .instance_ids
            .get(&InstancePath(vec![]))
            .expect("top-level instance not found");
        let module_name = &self.program.instance_module[top_instance_id];
        let module_vars = &self.program.module_variables[module_name];

        let mut result = Vec::new();
        for (var_path, info) in module_vars {
            let name = var_path
                .0
                .iter()
                .map(|s| {
                    veryl_parser::resource_table::get_str_value(*s)
                        .unwrap()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(".");
            let addr = crate::ir::AbsoluteAddr {
                instance_id: *top_instance_id,
                var_id: info.id,
            };
            let signal = self.backend.resolve_signal(&addr);

            // Resolve associated clock for reset signals
            let associated_clock = self
                .program
                .reset_clock_map
                .get(&addr)
                .map(|clock_addr| self.program.get_path(clock_addr));

            result.push(NamedSignal {
                name,
                signal,
                info: info.clone(),
                associated_clock,
            });
        }
        result
    }

    /// Returns all events (clock/reset signals) with their IDs and event references.
    pub fn named_events(&self) -> Vec<NamedEvent> {
        let mut result = Vec::new();
        for (id, addr) in self.backend.id_to_addr.iter().enumerate() {
            let name = self.program.get_path(addr);
            if let Some(ev) = self.backend.resolve_event_opt(addr) {
                result.push(NamedEvent {
                    name,
                    id,
                    event_ref: ev,
                });
            }
        }
        result
    }

    /// Triggers a clock/event by its numeric ID.
    pub fn tick_by_id(&mut self, event_id: usize) -> Result<(), RuntimeErrorCode> {
        let event = self.backend.id_to_event[event_id];
        self.tick(event)
    }

    /// Triggers a clock/event N times by its numeric ID.
    /// Avoids repeated cross-boundary calls when used from FFI.
    pub fn tick_by_id_n(&mut self, event_id: usize, count: u32) -> Result<(), RuntimeErrorCode> {
        let event = self.backend.id_to_event[event_id];
        for _ in 0..count {
            self.tick(event)?;
        }
        Ok(())
    }
}
