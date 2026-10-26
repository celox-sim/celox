//! Dynamic executions of a stackless kernel. Forks share lexical cells, but
//! each child owns its control slots and compiler temporaries.
use super::{SimulationState, wait::wake_in_region};
use crate::{
    SignalRef, SimulatorErrorCode,
    backend::SimBackend,
    timing::{Halt, Value, read},
};
use celox_design::ForkJoin;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    pub kernel: usize,
    pub parent: Option<usize>,
    pub private: Vec<Value>,
    pub locals: Vec<Option<usize>>,
    pub live: bool,
    pub pending: Vec<usize>,
    pub join: Option<(ForkJoin, Vec<usize>)>,
}
impl Activation {
    fn root(kernel: usize) -> Self {
        Self {
            kernel,
            parent: None,
            private: Vec::new(),
            locals: Vec::new(),
            live: true,
            pending: Vec::new(),
            join: None,
        }
    }
}
fn write<B: SimBackend>(backend: &mut B, signal: SignalRef, value: &Value) {
    backend.set_four_state(signal, value.0.clone(), value.1.clone());
}
impl<B: SimBackend> SimulationState<B> {
    pub(super) fn restore_activation(&mut self, backend: &mut B, process: usize) -> usize {
        if self.process_state.activations.is_empty() {
            if !self.model.processes.iter().any(|p| p.frame.is_some()) {
                return process;
            }
            self.process_state.activations = (0..self.kernel_count).map(Activation::root).collect();
        }
        let activation = &mut self.process_state.activations[process];
        if let Some(frame) = &self.model.processes[process].frame {
            if activation.private.is_empty() {
                activation.private = frame
                    .private
                    .iter()
                    .map(|&slot| read(backend, slot))
                    .collect();
                activation.locals.resize(frame.locals.len(), None);
            } else {
                for (&slot, value) in frame.private.iter().zip(&activation.private) {
                    write(backend, slot, value);
                }
            }
            for (&slot, cell) in frame.locals.iter().zip(&activation.locals) {
                if let Some(cell) = cell {
                    write(backend, slot, &self.process_state.cells[*cell]);
                }
            }
        }
        activation.kernel
    }

    pub(super) fn save_activation(&mut self, backend: &B, process: usize) {
        let Some(frame) = &self.model.processes[process].frame else {
            return;
        };
        let activation = &mut self.process_state.activations[process];
        for (&slot, value) in frame.private.iter().zip(&mut activation.private) {
            *value = read(backend, slot);
        }
        for (&slot, cell) in frame.locals.iter().zip(&activation.locals) {
            if let Some(cell) = cell {
                self.process_state.cells[*cell] = read(backend, slot);
            }
        }
    }

    pub(super) fn allocate_local(&mut self, process: usize, index: usize) -> Result<(), Halt> {
        if self.model.processes[process].frame.is_none() {
            return Ok(());
        }
        if self.process_state.cells.len() >= 1024
            && self.process_state.cells.len().is_multiple_of(1024)
        {
            self.collect_activation_cells();
        }
        let locals = &mut self.process_state.activations[process].locals;
        let target = locals
            .get_mut(index)
            .ok_or(SimulatorErrorCode::InternalError)?;
        *target = Some(self.process_state.cells.len());
        self.process_state.cells.push(Value::default());
        Ok(())
    }

    pub(super) fn fork_activation(
        &mut self,
        backend: &mut B,
        process: usize,
        encoded: u64,
    ) -> Result<bool, Halt> {
        let first = (encoded & 0xffff) as usize;
        let count = ((encoded >> 16) & 0xffff) as usize;
        let join = match encoded >> 32 {
            0 => ForkJoin::All,
            1 => ForkJoin::Any,
            2 => ForkJoin::None,
            _ => return Err(SimulatorErrorCode::InternalError.into()),
        };
        self.save_activation(backend, process);
        let frame = self.model.processes[process]
            .frame
            .as_ref()
            .ok_or(SimulatorErrorCode::InternalError)?;
        let resume = frame
            .private
            .iter()
            .position(|slot| *slot == self.model.processes[process].resume)
            .ok_or(SimulatorErrorCode::InternalError)?;
        let status = frame
            .private
            .iter()
            .position(|slot| *slot == self.model.processes[process].status)
            .ok_or(SimulatorErrorCode::InternalError)?;
        let random_slot = frame.random;
        let random = frame
            .private
            .iter()
            .position(|slot| *slot == random_slot)
            .ok_or(SimulatorErrorCode::InternalError)?;
        let raw = backend.get_as::<u64>(random_slot);
        let mut seed = if raw == 0 { frame.seed } else { raw as u32 };
        let mut children = Vec::with_capacity(count);
        for entry in first..first + count {
            let mut child = self.process_state.activations[process].clone();
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            child.private[random] = ((u64::from(seed) | (1 << 32)).into(), 0u8.into());
            child.parent = Some(process);
            child.pending.clear();
            child.join = None;
            child.private[resume] = ((entry as u64).into(), 0u8.into());
            child.private[status] = (0u8.into(), 0u8.into());
            let reusable = (self.kernel_count..self.process_state.activations.len()).find(|&id| {
                !self.process_state.activations[id].live
                    && !self
                        .process_state
                        .activations
                        .iter()
                        .any(|a| a.parent == Some(id))
            });
            let id = reusable.unwrap_or(self.process_state.activations.len());
            if id == self.process_state.activations.len() {
                self.process_state.activations.push(child);
                self.model
                    .processes
                    .push(self.model.processes[process].clone());
            } else {
                self.process_state.activations[id] = child;
                self.model.processes[id] = self.model.processes[process].clone();
            }
            children.push(id);
        }
        if count != 0 {
            backend.set(random_slot, u64::from(seed) | (1 << 32));
        }
        self.process_state.activations[process]
            .pending
            .extend(&children);
        if join == ForkJoin::None || children.is_empty() {
            return Ok(true);
        }
        self.process_state.activations[process].join = Some((join, children));
        Ok(false)
    }

    /// A real HDL suspension or termination starts the children created since
    /// the last such boundary. Host requests, NBA staging and budget yields do not.
    pub(super) fn release_children(&mut self, process: usize) {
        if self.process_state.activations.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.process_state.activations[process].pending);
        for child in pending {
            let state = &mut self.process_state;
            wake_in_region(
                &self.model.processes,
                &mut state.ready,
                &mut state.reactive,
                child,
            );
        }
    }

    pub(super) fn end_activation(&mut self, process: usize) {
        if self.process_state.activations.is_empty()
            || self.model.processes[process].frame.is_none()
        {
            return;
        }
        self.process_state.activations[process].live = false;
        if let Some(parent) = self.process_state.activations[process].parent {
            let parent = &mut self.process_state.activations[parent];
            if let Some((join, children)) = &mut parent.join {
                let member = children.contains(&process);
                children.retain(|&child| child != process);
                if member && (*join == ForkJoin::Any || children.is_empty()) {
                    parent.join = None;
                    let id = self.process_state.activations[process].parent.unwrap();
                    let state = &mut self.process_state;
                    wake_in_region(
                        &self.model.processes,
                        &mut state.ready,
                        &mut state.reactive,
                        id,
                    );
                }
            }
        }
    }

    pub(super) fn retire_activation(&mut self, process: usize) {
        let mut next = Some(process);
        while let Some(id) = next {
            let has_children = self
                .process_state
                .activations
                .iter()
                .any(|child| child.parent == Some(id));
            let Some(activation) = self.process_state.activations.get_mut(id) else {
                break;
            };
            if activation.live {
                break;
            }
            activation.private.clear();
            activation.locals.clear();
            if has_children {
                break;
            }
            next = activation.parent.take();
        }
    }

    fn collect_activation_cells(&mut self) {
        let state = &mut self.process_state;
        let mut mapping = vec![None; state.cells.len()];
        let mut cells = Vec::new();
        for activation in &mut state.activations {
            for cell in activation.locals.iter_mut().flatten() {
                let new = *mapping[*cell].get_or_insert_with(|| {
                    cells.push(state.cells[*cell].clone());
                    cells.len() - 1
                });
                *cell = new;
            }
        }
        state.cells = cells;
    }

    /// Keep heap handles stored outside the backend memory alive at host safepoints.
    pub fn retain_activation_roots<E: super::SimulationExecutor<Backend = B>>(
        &self,
        executor: &mut E,
    ) {
        let mut values = Vec::new();
        for (id, activation) in self.process_state.activations.iter().enumerate() {
            if !activation.live {
                continue;
            }
            if let Some(frame) = &self.model.processes[id].frame {
                values.extend(
                    frame
                        .private
                        .iter()
                        .copied()
                        .zip(activation.private.iter().cloned()),
                );
                values.extend(frame.locals.iter().zip(&activation.locals).filter_map(
                    |(&slot, cell)| cell.map(|cell| (slot, self.process_state.cells[cell].clone())),
                ));
            }
        }
        executor.retain_process_values(&values);
    }

    pub(super) fn rebuild_activation_models(&mut self) {
        self.model.processes.truncate(self.kernel_count);
        for index in 0..self.process_state.activations.len() {
            let kernel = self.process_state.activations[index].kernel;
            if index == self.model.processes.len() {
                self.model
                    .processes
                    .push(self.model.processes[kernel].clone());
            }
        }
    }
}

pub(super) fn activation_value<'a>(
    activations: &'a [Activation],
    cells: &'a [Value],
    refs: &crate::timing::ProcessRefs,
    process: usize,
    signal: SignalRef,
) -> Option<&'a Value> {
    let frame = refs.frame.as_ref()?;
    let activation = activations.get(process)?;
    if let Some(index) = frame.locals.iter().position(|&slot| slot == signal) {
        return activation.locals.get(index)?.map(|cell| &cells[cell]);
    }
    frame
        .private
        .iter()
        .position(|&slot| slot == signal)
        .and_then(|index| activation.private.get(index))
}
