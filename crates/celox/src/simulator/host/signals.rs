//! Signal and event lookup, host reads and writes, and the instance hierarchy.

use super::*;

/// Hierarchical instance tree with resolved signals.
#[derive(Debug, Clone)]
pub struct InstanceHierarchy {
    pub module_name: String,
    /// The index of this instance under its name: its element index in
    /// an instance array or generate loop, 0 otherwise.
    pub index: usize,
    /// Whether this instance's name takes an index (an instance array or
    /// generate loop), even when it is the only instance under the name.
    pub indexed: bool,
    pub signals: Vec<NamedSignal>,
    pub children: Vec<(String, Vec<InstanceHierarchy>)>,
}

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
pub struct NamedEvent<B: SimBackend = crate::DefaultBackend> {
    pub name: String,
    pub id: usize,
    pub event_ref: B::Event,
}

/// The host's write of an event signal's level, observed by a timed
/// simulation's waits and counters as [`IOContext`] writes are.
pub(super) fn watched_write<B: SimBackend>(
    watch: &mut celox_runtime::HostWatch,
) -> impl FnMut(&mut B, SignalRef, u8) + '_ {
    move |backend, signal, level| {
        if watch.is_empty() {
            backend.set(signal, level);
        } else {
            watch.write(backend, signal, |backend| backend.set(signal, level));
        }
    }
}

impl<B: SimBackend> Simulator<B> {
    /// Sets a signal value and marks combinational logic as dirty.
    /// Copies the value's little-endian bytes, zero-extending shorter
    /// values and truncating bits beyond the signal width. Integer type
    /// size does not need to match the signal storage size.
    pub fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        self.backend.set(signal, val);
        self.dirty = true;
        self.settle_dirty_for_runtime_event_drain();
    }

    /// Sets a wide signal value and marks combinational logic as dirty.
    pub fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        self.backend.set_wide(signal, val);
        self.dirty = true;
        self.settle_dirty_for_runtime_event_drain();
    }

    /// Sets a four-state signal value and marks combinational logic as dirty.
    pub fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
        self.backend.set_four_state(signal, val, mask);
        self.dirty = true;
        self.settle_dirty_for_runtime_event_drain();
    }

    /// Modifies internal state via a callback and marks combinational logic as dirty.
    pub fn modify<F>(&mut self, f: F) -> Result<(), RuntimeErrorCode>
    where
        F: FnOnce(&mut IOContext<B>),
    {
        if self.frozen {
            return Ok(());
        }
        let mut ctx = IOContext {
            backend: &mut self.backend,
            watch: &mut self.host_watch,
        };
        f(&mut ctx);
        self.dirty = true;
        // Without a scheduler, nothing else settles the write for a
        // drain on another thread; a timed simulation's next step does,
        // and handles a fatal assertion there.
        if !self.timed && self.runtime_event_drain_active.load(Ordering::Acquire) {
            self.eval_comb_for_runtime_event_drain()?;
            self.end_untimed_slot()?;
        }
        Ok(())
    }

    /// Resolves a signal path into a performance-optimized [`SignalRef`].
    /// This handle allows for direct memory access without `HashMap` lookups.
    ///
    /// # Panics
    ///
    /// Panics when the path does not resolve, including when the dead
    /// store elimination policy does not guarantee the signal; see
    /// [`Self::try_signal`].
    pub fn signal(&self, path: &str) -> SignalRef {
        self.try_signal(path)
            .unwrap_or_else(|error| panic!("cannot resolve signal: {error}"))
    }

    /// Resolve a port name to an event handle.
    pub fn event(&self, port: &str) -> B::Event {
        let addr = self.program.get_addr(&[], &[port]).unwrap();
        self.backend.resolve_event(&addr)
    }

    /// Try to resolve a signal path. Returns `Err` if the path is not found or ambiguous.
    pub fn try_signal(&self, path: &str) -> Result<SignalRef, crate::ir::AddrLookupError> {
        self.try_child_signal(&[], path)
    }

    /// Try to resolve a port name to an event handle.
    /// Returns an error if the path is missing, ambiguous, or has no registered event.
    pub fn try_event(&self, port: &str) -> Result<B::Event, crate::ir::AddrLookupError> {
        let addr = self.program.get_addr(&[], &[port])?;
        self.backend.resolve_event_opt(&addr).ok_or_else(|| {
            crate::ir::AddrLookupError::NotAnEvent {
                path: port.to_string(),
            }
        })
    }

    /// Retrieves the current value as a fixed-size type without `BigUint` allocation.
    /// Lazily evaluates combinational logic if the state is dirty.
    pub fn get_as<T: Default + Copy>(&mut self, signal: SignalRef) -> T {
        if self.dirty {
            self.eval_comb_checked().unwrap();
            self.dirty = false;
        }
        self.backend.get_as(signal)
    }

    /// Retrieves the current value of a variable using a pre-resolved [`SignalRef`] handle.
    /// Lazily evaluates combinational logic if the state is dirty.
    pub fn get(&mut self, signal: SignalRef) -> BigUint {
        if self.dirty {
            self.eval_comb_checked().unwrap();
            self.dirty = false;
        }
        self.backend.get(signal)
    }

    /// Retrieves the current 4-state value (value, mask) of a variable using a [`SignalRef`] handle.
    /// Lazily evaluates combinational logic if the state is dirty.
    pub fn get_four_state(&mut self, signal: SignalRef) -> (BigUint, BigUint) {
        if self.dirty {
            self.eval_comb_checked().unwrap();
            self.dirty = false;
        }
        self.backend.get_four_state(signal)
    }

    /// Returns a raw pointer to the backend memory and its total size in bytes.
    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        self.backend.memory_as_ptr()
    }

    /// Returns a mutable raw pointer to the backend memory and its total size in bytes.
    pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        self.backend.memory_as_mut_ptr()
    }

    /// Returns an opaque owner that keeps the backend memory allocation alive.
    pub fn memory_owner(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.backend.memory_owner()
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
        let top_instance = self
            .program
            .design
            .root_instance()
            .expect("top-level instance not found");
        self.build_signals_for_instance(top_instance.id)
    }

    /// Returns all signals for the instance at the given hierarchical path.
    ///
    /// The path is specified as a slice of `(instance_name, index)` pairs.
    /// Returns an empty `Vec` if the path does not exist.
    pub fn instance_signals(&self, instance_path: &[(&str, usize)]) -> Vec<NamedSignal> {
        let path: Vec<_> = instance_path
            .iter()
            .map(|(name, idx)| ((*name).to_string(), *idx))
            .collect();
        match self.program.design.instance_at_path(&InstancePath(path)) {
            Some(instance) => self.build_signals_for_instance(instance.id),
            None => Vec::new(),
        }
    }

    /// Builds the list of named signals for a given instance.
    ///
    /// Variables with ambiguous VarPaths (multiple scoped locals sharing the
    /// same name) are excluded — they cannot be addressed by path and would
    /// cause silent overwrites in name-keyed maps such as the layout JSON.
    pub(super) fn build_signals_for_instance(
        &self,
        instance_id: crate::ir::InstanceId,
    ) -> Vec<NamedSignal> {
        let instance = self.program.design.instance(instance_id).unwrap();
        let in_root = self
            .program
            .design
            .root_instance()
            .is_some_and(|root| root.id == instance.id);

        let mut result = Vec::new();
        for address in instance.state_addresses() {
            let variable = self.program.design.variable(address).unwrap();
            // Skip variables whose VarPath is ambiguous (None in the path index).
            if !instance.resolves_path_to(&variable.path, *address) {
                continue;
            }
            // Enumeration exposes the same handles path lookup would.
            if !self.observable_under_dead_store_policy(*address, in_root) {
                continue;
            }
            let name = variable.path.join(".");
            let signal = self.backend.resolve_signal(address);

            // Resolve associated clock for reset signals
            let associated_clock = self
                .program
                .design
                .events
                .reset_clocks
                .get(address)
                .map(|clock_addr| self.program.get_path(clock_addr));

            result.push(NamedSignal {
                name,
                signal,
                info: self.program.design.variable_info(address).unwrap(),
                associated_clock,
            });
        }
        result
    }

    /// Returns all events (clock/reset signals) with their IDs and event references.
    pub fn named_events(&self) -> Vec<NamedEvent<B>> {
        let mut result = Vec::new();
        for (id, addr) in self.backend.id_to_addr_slice().iter().enumerate() {
            // A hidden event, such as the copy of a clock sensed at its
            // other edge, has no name to list.
            if !self.event_is_named(addr) {
                continue;
            }
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

    /// Whether the event at `addr` is a variable a path names.
    pub(super) fn event_is_named(&self, addr: &crate::ir::AbsoluteAddr) -> bool {
        self.program
            .design
            .instance(addr.instance_id)
            .is_some_and(|instance| {
                self.program
                    .design
                    .variable(addr)
                    .is_some_and(|variable| instance.resolves_path_to(&variable.path, *addr))
            })
    }

    /// Resolves a signal inside a child instance.
    pub fn child_signal(&self, instance_path: &[(&str, usize)], var: &str) -> SignalRef {
        self.try_child_signal(instance_path, var)
            .unwrap_or_else(|error| panic!("cannot resolve signal: {error}"))
    }

    /// Try to resolve a signal inside a child instance.
    ///
    /// [`Self::named_signals`], [`Self::instance_signals`] and
    /// [`Self::named_hierarchy`] omit the same signals.
    ///
    /// Under a dead store elimination policy, only state the policy
    /// guarantees to keep resolves: ports the policy preserves, signals
    /// listed with `live_signal()`, and state no combinational logic
    /// writes. Anything else may have lost its stores, and a handle to it
    /// would only read memory that never updates.
    pub fn try_child_signal(
        &self,
        instance_path: &[(&str, usize)],
        var: &str,
    ) -> Result<SignalRef, crate::ir::AddrLookupError> {
        let addr = self.program.get_addr(instance_path, &[var])?;
        let in_root = self
            .program
            .design
            .root_instance()
            .is_some_and(|root| root.state_addresses().contains(&addr));
        if !self.observable_under_dead_store_policy(addr, in_root) {
            let path = instance_path
                .iter()
                .map(|(name, index)| format!("{name}[{index}]."))
                .chain(std::iter::once(var.to_string()))
                .collect::<String>();
            return Err(crate::ir::AddrLookupError::DeadStoreEliminated { path });
        }
        Ok(self.backend.resolve_signal(&addr))
    }

    /// Whether the dead store elimination policy guarantees that `addr`
    /// stays current. `in_root` says whether it belongs to the top
    /// instance, whose ports `PreserveTopPorts` keeps.
    pub(super) fn observable_under_dead_store_policy(
        &self,
        addr: crate::ir::AbsoluteAddr,
        in_root: bool,
    ) -> bool {
        use crate::DeadStorePolicy;
        let schema = &self.program.runtime_schema;
        if schema.dead_store_policy == DeadStorePolicy::Off
            || !schema.comb_writes.contains(&addr)
            || schema.live_signals.contains(&addr)
            || schema.testbench_read_roots.contains(&addr)
        {
            return true;
        }
        let is_port = self
            .program
            .design
            .variable(&addr)
            .is_some_and(|variable| variable.var_kind.is_port());
        match schema.dead_store_policy {
            DeadStorePolicy::Off => true,
            DeadStorePolicy::PreserveListedSignals => false,
            DeadStorePolicy::PreserveTopPorts => in_root && is_port,
            DeadStorePolicy::PreserveAllPorts => is_port,
        }
    }

    /// Returns the full instance hierarchy starting from the top module.
    pub fn named_hierarchy(&self) -> InstanceHierarchy {
        self.build_hierarchy(&[])
    }

    pub(super) fn build_hierarchy(&self, current_path: &[(String, usize)]) -> InstanceHierarchy {
        let instance = self
            .program
            .design
            .instance_at_path(&InstancePath(current_path.to_vec()))
            .expect("instance not found");
        let module_name = instance.module_name.clone();
        let indexed = instance
            .display_path
            .last()
            .is_some_and(|segment| segment.ends_with(']'));

        let signals = self.build_signals_for_instance(instance.id);

        // Find direct children: instance paths that extend current by exactly 1 segment
        let current_len = current_path.len();
        let mut children_map: crate::HashMap<String, Vec<(usize, InstanceHierarchy)>> =
            crate::HashMap::default();

        for child in self.program.design.instances() {
            if child.path.0.len() == current_len + 1 && child.path.0.starts_with(current_path) {
                let (child_name, child_index) = &child.path.0[current_len];
                let child_hierarchy = self.build_hierarchy(&child.path.0);
                children_map
                    .entry(child_name.clone())
                    .or_default()
                    .push((*child_index, child_hierarchy));
            }
        }

        // Sort children by index within each group
        let mut children: Vec<(String, Vec<InstanceHierarchy>)> = children_map
            .into_iter()
            .map(|(name, mut instances)| {
                instances.sort_by_key(|(idx, _)| *idx);
                let sorted = instances.into_iter().map(|(_, h)| h).collect();
                (name, sorted)
            })
            .collect();
        children.sort_by(|(a, _), (b, _)| a.cmp(b));

        InstanceHierarchy {
            module_name,
            index: current_path.last().map_or(0, |(_, index)| *index),
            indexed,
            signals,
            children,
        }
    }
}
