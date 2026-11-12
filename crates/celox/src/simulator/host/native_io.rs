//! Host services called through the native scheduler's extern-function ABI.
//! Signal callbacks use a prepared invocation, with no backend method or state
//! slice borrowed across generated execution. They never re-enter simulation
//! scheduling; the generated caller settles their writes and resumes its waves.
pub(crate) use super::native_arena::Header as ArenaHeader;
pub(crate) use super::native_nba::Header as NbaHeader;
use super::*;
use celox_design::{HostRequestKind, HostRequestSite};

pub(crate) const SYMBOL: &str = "__celox_native_services_v11";

pub(crate) const BEFORE_DOMAINS: u64 = u64::MAX - 14;
/// A watched value may have changed (`SlotObserver::after_update`).
pub(crate) const AFTER_UPDATE: u64 = u64::MAX - 15;
/// A unit wrote a watched object (`celox_sir::extern_abi::VALUE_CHANGE_ID`):
/// show the observer the update now. What it does in return takes effect at
/// the next `AFTER_UPDATE`, which follows the unit.
pub(crate) const VALUE_CHANGE: u64 = u64::MAX - 16;
/// The bit of an `AFTER_UPDATE` result that requests a finish.
pub(crate) const FINISHED: u64 = 4;

pub(crate) const CHECKPOINT: u64 = u64::MAX;
pub(crate) const SCHEDULER_RUN: u64 = u64::MAX - 1;
pub(crate) const SCHEDULER_PASS: u64 = u64::MAX - 2;
pub(crate) const NBA_PUBLISH: u64 = u64::MAX - 3;
pub(crate) const PROCESS_PHASE: u64 = u64::MAX - 4;
pub(crate) const NBA_REQUEST: u64 = 1 << 63;
pub(crate) const ARENA_RESERVE_RECORDS: u64 = u64::MAX - 5;
pub(crate) const ARENA_RESERVE_IMAGES: u64 = u64::MAX - 6;
pub(crate) const ARENA_WAVE_LIMIT: u64 = u64::MAX - 7;
pub(crate) const GUARD_FAULT: u64 = u64::MAX - 8;
pub(crate) const COMB_BEFORE: u64 = u64::MAX - 9;
pub(crate) const COMB_AFTER: u64 = u64::MAX - 10;
pub(crate) const ARENA_RESERVE_NBA: u64 = u64::MAX - 11;
pub(crate) const NBA_TIME_OVERFLOW: u64 = u64::MAX - 13;
pub(crate) const ARENA_RESERVE_DELAYED_NBA: u64 = u64::MAX - 12;

pub(crate) fn signature() -> celox_design::ExternSignature {
    let integer = celox_design::ExternType::Integer {
        width: 64,
        signed: false,
    };
    celox_design::ExternSignature {
        pure: false,
        result: Some(integer),
        arguments: vec![integer; 5],
        directions: Vec::new(),
    }
}

pub(super) struct WriteSite {
    pub request: HostRequestSite<SignalRef>,
    pub format: RuntimeEventSite,
}

pub(super) struct Metadata {
    pub layout: MemoryLayout,
    pub context: SignalRef,
    pub events: SignalRef,
    /// Per process, indexed by that process's FileIo request index.
    pub sites: Vec<Vec<Option<WriteSite>>>,
    /// Per process, the FileIo sites of the function units it calls, by
    /// their index among those (see [`celox_design::SiteProcess`]).
    pub function_sites: Vec<Vec<Option<WriteSite>>>,
    pub runtime_sites: Vec<RuntimeEventSite>,
    pub process_names: Vec<String>,
    pub nbas: Vec<Vec<celox_design::NbaSite<SignalRef>>>,
    pub native_nbas: Vec<celox_design::NbaSite<SignalRef>>,
    pub host_inputs: crate::HashMap<SignalRef, Vec<usize>>,
    pub flagged: Vec<(usize, usize)>,
    pub deferred_groups: Vec<(u32, usize)>,
}

pub(crate) fn supported<A>(site: &HostRequestSite<A>) -> bool {
    site.kind == HostRequestKind::FileIo
        && matches!(site.operation, 2 | 3)
        && site.results.is_empty()
        && !site.args.is_empty()
}

impl Metadata {
    pub(super) fn new<B: SimBackend>(backend: &B, program: &RuntimeProgram) -> Option<Arc<Self>> {
        if !program
            .runtime_schema
            .extern_functions
            .iter()
            .any(|function| function.name == SYMBOL)
        {
            return None;
        }
        let mut sites: Vec<Vec<Option<WriteSite>>> = program
            .runtime_schema
            .processes
            .iter()
            .map(|_| Vec::new())
            .collect();
        let mut function_sites: Vec<Vec<Option<WriteSite>>> =
            sites.iter().map(|_| Vec::new()).collect();
        for request in &program.runtime_schema.host_request_sites {
            if request.kind != HostRequestKind::FileIo {
                continue;
            }
            let resolved = request.map(&mut |address| backend.resolve_signal(&address));
            let supported = supported(&resolved)
                && resolved
                    .args
                    .iter()
                    .all(|arg| arg.id.array_layout.is_none());
            let site = if supported {
                let process = &program.runtime_schema.processes[request.process.index()];
                let scope = program
                    .design
                    .instance(process.status.instance_id)
                    .map(|instance| {
                        let top = program
                            .design
                            .root_instance()
                            .map(|root| root.module_name.as_str())
                            .unwrap_or("");
                        std::iter::once(top)
                            .chain(instance.display_path.iter().map(String::as_str))
                            .filter(|part| !part.is_empty())
                            .collect::<Vec<_>>()
                            .join(".")
                    });
                let format = RuntimeEventSite {
                    kind: RuntimeEventKind::Write,
                    template: request.strings.first().cloned(),
                    sizing: DisplaySizing::Ieee,
                    scope,
                    location: None,
                    assertion: None,
                    deferred: None,
                    timestamp_arg: false,
                    // The last value is the format of `%t` when the call
                    // ran.
                    time_format_arg: request
                        .strings
                        .get(1)
                        .is_some_and(|text| text == celox_design::TIME_FORMAT_ARGUMENT),
                    arg_widths: Vec::new(),
                    arg_signed: Vec::new(),
                    arg_is_string: Vec::new(),
                    arg_float_precision: Vec::new(),
                    arg_is_handle: Vec::new(),
                };
                Some(WriteSite {
                    request: resolved,
                    format,
                })
            } else {
                None
            };
            let listed = if request.process.is_function_site() {
                &mut function_sites
            } else {
                &mut sites
            };
            listed[request.process.index()].push(site);
        }
        let scheduler = program.runtime_schema.scheduler.as_ref()?;
        let mut nbas: Vec<Vec<_>> = program
            .runtime_schema
            .processes
            .iter()
            .map(|_| Vec::new())
            .collect();
        for site in &program.runtime_schema.nba_sites {
            if scheduler
                .fallback
                .get(site.process)
                .is_some_and(Option::is_none)
            {
                nbas[site.process].push(site.map(|address| backend.resolve_signal(&address)));
            }
        }
        let flagged = nbas
            .iter()
            .enumerate()
            .flat_map(|(process, sites)| {
                sites
                    .iter()
                    .enumerate()
                    .filter(|(_, site)| site.pending.is_some())
                    .map(move |(site, _)| (process, site))
            })
            .collect();
        let mut deferred_groups = Vec::new();
        for observer in &program.runtime_schema.comb_observers {
            if matches!(
                program.runtime_schema.runtime_event_sites[observer.site_id as usize].deferred,
                Some(celox_design::DeferredSite::Action { .. })
            ) && !deferred_groups
                .iter()
                .any(|&(group, _)| group == observer.activation_group)
            {
                deferred_groups.push((observer.activation_group, observer.site_id as usize));
            }
        }
        let mut host_inputs: crate::HashMap<SignalRef, Vec<usize>> = crate::HashMap::default();
        for (id, signal) in crate::scheduler::host_inputs::signals(&program.runtime_schema)
            .into_iter()
            .enumerate()
        {
            host_inputs
                .entry(backend.resolve_signal(&signal))
                .or_default()
                .push(id);
        }
        Some(Arc::new(Self {
            host_inputs,
            native_nbas: program
                .runtime_schema
                .nba_sites
                .iter()
                .map(|site| site.map(|address| backend.resolve_signal(&address)))
                .collect(),
            deferred_groups,
            nbas,
            flagged,
            layout: backend.layout().clone(),
            context: {
                let mut context = backend.resolve_signal(&scheduler.slots.defer_limit_settle);
                context.offset += celox_design::SCHEDULER_HOST_CONTEXT_BIT / 8;
                context.width = 64;
                context
            },
            events: backend.resolve_signal(&scheduler.slots.events),
            sites,
            function_sites,
            runtime_sites: program.runtime_schema.runtime_event_sites.clone(),
            process_names: program
                .runtime_schema
                .process_origins
                .iter()
                .map(ToString::to_string)
                .collect(),
        }))
    }
}

pub(super) trait ObserverService {
    fn before_domains(
        &mut self,
        time: u64,
        metadata: &Metadata,
        arena: &mut super::native_arena::Arena,
    ) -> u64;

    fn after_update(
        &mut self,
        time: u64,
        metadata: &Metadata,
        arena: &mut super::native_arena::Arena,
    ) -> u64;

    /// [`Self::after_update`] inside a unit: what the observer does takes
    /// effect at the next update point, and the host inputs it writes stay
    /// logged until then.
    fn value_change(&mut self, time: u64);
}

pub(super) struct SlotService<'a, B: SimBackend> {
    pub backend: &'a mut B,
    pub observer: Option<&'a mut (dyn crate::SlotObserver<B> + 'static)>,
    pub watch: &'a mut celox_runtime::HostWatch,
    pub dirty: &'a mut bool,
    /// What the observer did at value change points since the last update
    /// point, encoded as the result of `AFTER_UPDATE`.
    pub changed: &'a mut u64,
}

impl<B: SimBackend> ObserverService for SlotService<'_, B> {
    fn before_domains(
        &mut self,
        time: u64,
        metadata: &Metadata,
        arena: &mut super::native_arena::Arena,
    ) -> u64 {
        self.show(time, Some((metadata, arena)), |observer, io, time| {
            observer.before_domains(io, time)
        })
    }

    fn after_update(
        &mut self,
        time: u64,
        metadata: &Metadata,
        arena: &mut super::native_arena::Arena,
    ) -> u64 {
        let changed = std::mem::take(self.changed);
        // A finish requested at a value change point ends the run before
        // the observer sees another update, as in the reference loop.
        if changed & FINISHED != 0 {
            return changed;
        }
        self.show(time, Some((metadata, arena)), |observer, io, time| {
            observer.after_update(io, time)
        }) | changed
    }

    fn value_change(&mut self, time: u64) {
        if *self.changed & FINISHED != 0 {
            return;
        }
        *self.changed |= self.show(time, None, |observer, io, time| {
            observer.after_update(io, time)
        });
    }
}

impl<B: SimBackend> SlotService<'_, B> {
    /// Show the observer the state at `time`; the result encodes whether it
    /// wrote (bit 1) and requested a finish (bit 2). The host inputs it
    /// writes are packed into `inputs`, or stay logged without one.
    fn show(
        &mut self,
        time: u64,
        inputs: Option<(&Metadata, &mut super::native_arena::Arena)>,
        show: impl FnOnce(&mut dyn crate::SlotObserver<B>, &mut crate::SlotIo<'_, B>, u64),
    ) -> u64 {
        let mut io = crate::SlotIo::new(crate::IOContext {
            backend: self.backend,
            watch: self.watch,
        });
        if let Some(observer) = self.observer.as_deref_mut() {
            show(observer, &mut io, time);
        }
        let wrote = io.wrote();
        let finish = io.finish_requested();
        *self.dirty |= wrote;
        if !finish && let Some((metadata, arena)) = inputs {
            pack_host_inputs(self.backend, metadata, arena, self.watch.take_log());
        }
        // A finish precedes host-write observation in the reference round.
        // Keep its pending log in the saved state rather than a transient arena.
        (u64::from(wrote) * 2) | (u64::from(finish) * FINISHED)
    }
}

pub(super) fn pack_host_inputs<B: SimBackend>(
    backend: &B,
    metadata: &Metadata,
    arena: &mut super::native_arena::Arena,
    writes: Vec<(SignalRef, (BigUint, BigUint))>,
) {
    arena.host_values.clear();
    for (signal, value) in writes {
        let Some(ids) = metadata.host_inputs.get(&signal) else {
            continue;
        };
        let final_value = backend.get_four_state(signal);
        let words = signal.width.div_ceil(64);
        let masks = backend.layout().four_state && signal.is_4state;
        let planes = if masks { 2 } else { 1 };
        for &id in ids {
            for (header_id, (value, mask)) in [(id as u64, &value), (0, &final_value)] {
                let start = arena.host_values.len();
                arena.host_values.resize(start + 8 + words * planes, 0);
                arena.host_values[start] = header_id;
                for (word, value) in value.iter_u64_digits().take(words).enumerate() {
                    arena.host_values[start + 8 + word] = value;
                }
                if masks {
                    for (word, value) in mask.iter_u64_digits().take(words).enumerate() {
                        arena.host_values[start + 8 + words + word] = value;
                    }
                }
            }
        }
    }
    arena.header.host_values = arena.host_values.as_mut_ptr();
    arena.header.host_words = arena.host_values.len() as u64;
}

#[repr(C)]
pub(super) struct Scope<'a> {
    // Generated code reads this first pointer; Arena's first member is Header.
    pub arena: &'a mut super::native_arena::Arena,
    pub metadata: &'a Metadata,
    pub state: *const u8,
    pub state_size: usize,
    pub files: &'a mut celox_runtime::file_io::FileIo,
    pub pending: &'a Arc<std::sync::Mutex<PendingRuntimeEvents>>,
    pub read_seq: &'a AtomicU64,
    pub error: Option<RuntimeErrorCode>,
    pub calls: u64,
    pub return_budget: u64,
    pub round_budget: u64,
    pub nbas: &'a mut std::collections::VecDeque<celox_runtime::NbaUpdate>,
    pub delayed: &'a mut std::collections::VecDeque<(u64, celox_runtime::NbaUpdate)>,
    pub pending_scan: usize,
    pub comb_before: Vec<bool>,
    pub observer: Option<&'a mut dyn ObserverService>,
    /// The observer watches every update (`AFTER_UPDATE`).
    pub updates: bool,
}

impl Scope<'_> {
    /// Pass this separately borrowed host context through a private state slot.
    /// Generated code loads it as the first argument of the ordinary C call.
    pub(super) fn run<B: SimBackend>(
        &mut self,
        backend: &mut B,
        index: usize,
    ) -> (Result<(), RuntimeErrorCode>, Option<RuntimeErrorCode>) {
        self.run_call(|| (backend.run_process(index), None)).0
    }

    pub(super) fn run_prepared(
        &mut self,
        call: celox_runtime::backend::PreparedProcess,
    ) -> (
        (Result<(), RuntimeErrorCode>, Option<RuntimeErrorCode>),
        Option<std::time::Duration>,
    ) {
        self.run_call(|| {
            // SAFETY: the owner retains the backend exclusively. Only SlotIo
            // may borrow it synchronously; it cannot execute or promote code.
            unsafe { call.invoke() }
        })
    }

    fn run_call(
        &mut self,
        run: impl FnOnce() -> (Result<(), RuntimeErrorCode>, Option<std::time::Duration>),
    ) -> (
        (Result<(), RuntimeErrorCode>, Option<RuntimeErrorCode>),
        Option<std::time::Duration>,
    ) {
        self.arena.header.wave_limit = self.round_budget;
        self.arena.header.guard_valid = 0;
        self.arena.header.work_limit = self
            .round_budget
            .saturating_mul(self.metadata.process_names.len().max(1) as u64);
        self.arena.header.return_limit = self.return_budget;
        if let Err(error) = self.import_nbas() {
            let _ = self.export_nbas();
            return ((Err(error), self.error.take()), None);
        }
        self.arena.header.slot_observer = u64::from(self.observer.is_some());
        self.arena.header.observes_updates = u64::from(self.observer.is_some() && self.updates);
        let pointer = std::ptr::from_mut(self) as u64;
        let offset = self.metadata.context.offset;
        if offset
            .checked_add(8)
            .is_none_or(|end| end > self.state_size)
        {
            return (
                (
                    Err(Self::fault("native service context exceeds state image")),
                    None,
                ),
                None,
            );
        }
        // SAFETY: the image is interior mutable and exclusively executed. The
        // slot is validated above and no Rust reference into it is retained.
        let slot = unsafe { self.state.add(offset).cast_mut().cast::<u64>() };
        let previous = unsafe { slot.read_unaligned() };
        unsafe { slot.write_unaligned(pointer) };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
        unsafe { slot.write_unaligned(previous) };
        self.arena.header.slot_observer = 0;
        self.arena.header.observes_updates = 0;
        let (result, elapsed) = match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        let exported = self.export_nbas();
        ((result.and(exported), self.error.take()), elapsed)
    }

    fn store_value(
        &mut self,
        signal: SignalRef,
        value: &(BigUint, BigUint),
    ) -> Result<(), RuntimeErrorCode> {
        // The same bounds used for reads cover the planes written below.
        let before = self.value(signal)?;
        let plane = celox_state_layout::get_byte_size(signal.width);
        let count = if self.metadata.layout.four_state && signal.is_4state {
            2
        } else {
            1
        };
        for (index, value) in [&value.0, &value.1].into_iter().take(count).enumerate() {
            let bytes = value.to_bytes_le();
            if bytes.len() > plane {
                return Err(Self::fault("native NBA value exceeds its state plane"));
            }
            // SAFETY: native state uses interior-mutable storage, remains live
            // for the synchronous call, and the validated range covers this
            // plane. Generated code is paused and holds no Rust reference to it.
            unsafe {
                let target = self.state.add(signal.offset + index * plane).cast_mut();
                std::ptr::write_bytes(target, 0, plane);
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            }
        }
        if let Some(settle) = &self.metadata.layout.settle
            && self.value(signal)? != before
        {
            // SAFETY: this scope owns synchronous access to the state image
            // described by the same layout. These writes bypass backend.set.
            unsafe { settle.mark_range(self.state.cast_mut(), signal.offset, plane * count) };
        }
        Ok(())
    }

    fn import_record(
        &mut self,
        update: &celox_runtime::NbaUpdate,
        due: Option<u64>,
    ) -> Result<(), RuntimeErrorCode> {
        // Snapshots identify logical targets, not generated site numbers.
        // All static NBA destinations have a publication descriptor, including
        // values staged by a cold initializer before the first native run.
        let (id, site) = self
            .metadata
            .native_nbas
            .iter()
            .enumerate()
            .filter(|(_, site)| {
                site.target == update.target
                    && site.width >= update.width
                    && (update.value.1 == BigUint::default()
                        || (self.metadata.layout.four_state && site.value.is_4state))
            })
            .max_by_key(|(_, site)| site.width)
            .ok_or_else(|| Self::fault("queued NBA target has no native publication descriptor"))?;
        let masks = self.metadata.layout.four_state && site.value.is_4state;
        let words = site.width.div_ceil(64);
        if update.value.0.bits() > (words * 64) as u64
            || update.value.1.bits() > (words * 64) as u64
        {
            return Err(Self::fault("queued NBA exceeds its native staging format"));
        }
        let mut record = vec![0u64; 8 + words * if masks { 2 } else { 1 }];
        record[1] = due.unwrap_or_default();
        record[2] = id as u64;
        record[3] = update.lsb as u64;
        record[4] = update.width as u64;
        record[6] = self.arena.header.nba.sequence;
        record[7] = 1;
        for (index, value) in update.value.0.iter_u64_digits().enumerate() {
            record[8 + index] = value;
        }
        if masks {
            for (index, mask) in update.value.1.iter_u64_digits().enumerate() {
                record[8 + words + index] = mask;
            }
        }
        if due.is_some() {
            let required = self.arena.header.nba.delayed_len + 1;
            self.arena
                .nba
                .reserve_delayed(&mut self.arena.header.nba, required)?;
        }
        let pointer = self.arena.nba.import(&mut self.arena.header.nba, &record)?;
        if due.is_some() {
            // Input is ordered by (time, issue order). A sorted array is also
            // a min-heap; generated code owns all subsequent heap operations.
            self.arena
                .nba
                .append_delayed(&mut self.arena.header.nba, pointer)?;
            self.arena.header.nba.sequence += 1;
        } else {
            self.arena
                .nba
                .append_imported(&mut self.arena.header.nba, pointer)?;
        }
        Ok(())
    }

    fn import_nbas(&mut self) -> Result<(), RuntimeErrorCode> {
        while let Some(update) = self.nbas.front().cloned() {
            self.import_record(&update, None)?;
            self.nbas.pop_front();
        }
        while let Some((time, update)) = self.delayed.front().cloned() {
            self.import_record(&update, Some(time))?;
            self.delayed.pop_front();
        }
        Ok(())
    }

    fn decode_nba(&self, pointer: u64) -> Result<celox_runtime::NbaUpdate, RuntimeErrorCode> {
        let header = self
            .arena
            .nba
            .record(pointer, 8)
            .ok_or_else(|| Self::fault("invalid native NBA record"))?;
        let site = usize::try_from(header[2])
            .ok()
            .and_then(|site| self.metadata.native_nbas.get(site))
            .ok_or_else(|| Self::fault("invalid native NBA site"))?;
        let lsb =
            usize::try_from(header[3]).map_err(|_| Self::fault("invalid native NBA range"))?;
        let width =
            usize::try_from(header[4]).map_err(|_| Self::fault("invalid native NBA range"))?;
        let words = site.width.div_ceil(64);
        let masks = self.metadata.layout.four_state && site.value.is_4state;
        let record = self
            .arena
            .nba
            .record(pointer, 8 + words * if masks { 2 } else { 1 })
            .ok_or_else(|| Self::fault("invalid native NBA value"))?;
        let plane = |slice: &[u64]| {
            BigUint::new(
                slice
                    .iter()
                    .flat_map(|&word| [word as u32, (word >> 32) as u32])
                    .collect(),
            )
        };
        let value = plane(&record[8..8 + words]);
        let mask = if masks {
            plane(&record[8 + words..8 + words * 2])
        } else {
            BigUint::default()
        };
        Ok(celox_runtime::NbaUpdate {
            target: site.target,
            lsb,
            width,
            value: (value, mask),
        })
    }

    /// Portable serialization at invocation boundaries; the generated scheduler
    /// owns both queues throughout the run.
    fn export_nbas(&mut self) -> Result<(), RuntimeErrorCode> {
        while self.arena.header.nba.head != 0 {
            let pointer = self.arena.header.nba.head;
            let header = self
                .arena
                .nba
                .record(pointer, 8)
                .ok_or_else(|| Self::fault("invalid native NBA record"))?;
            let (next, class) = (header[0], header[5]);
            let update = self.decode_nba(pointer)?;
            self.nbas.push_back(update);
            self.arena
                .nba
                .release(&mut self.arena.header.nba, pointer, class)?;
            self.arena.header.nba.head = next;
        }
        self.arena.header.nba.tail = 0;
        let mut pending = Vec::new();
        for &pointer in self.arena.nba.delayed_records(&self.arena.header.nba)? {
            let record = self
                .arena
                .nba
                .record(pointer, 8)
                .ok_or_else(|| Self::fault("invalid native delayed NBA record"))?;
            pending.push((record[1], record[6], pointer, record[5]));
        }
        pending.sort_unstable_by_key(|&(time, sequence, _, _)| (time, sequence));
        // Decode before releasing any record, so malformed state cannot cause
        // an exported queue to retain freed native pointers.
        let updates: Result<Vec<_>, _> = pending
            .iter()
            .map(|&(time, _, pointer, _)| self.decode_nba(pointer).map(|update| (time, update)))
            .collect();
        let updates = updates?;
        for (_, _, pointer, class) in pending {
            self.arena
                .nba
                .release(&mut self.arena.header.nba, pointer, class)?;
        }
        self.delayed.extend(updates);
        self.arena.header.nba.delayed_len = 0;
        self.arena.header.nba.sequence = 0;
        Ok(())
    }

    fn queue_nba(&mut self, process: u64, index: u64) -> Result<(), RuntimeErrorCode> {
        let site = self
            .metadata
            .nbas
            .get(process as usize)
            .and_then(|sites| sites.get(index as usize))
            .cloned()
            .ok_or_else(|| Self::fault("invalid native NBA site"))?;
        if let Some(pending) = site.pending {
            self.store_value(pending, &(BigUint::default(), BigUint::default()))?;
        }
        if site.width == 0 || site.lsb >= site.target.width {
            return Ok(());
        }
        self.nbas.push_back(celox_runtime::NbaUpdate {
            target: site.target,
            lsb: site.lsb,
            width: site.width,
            value: self.value(site.value)?,
        });
        Ok(())
    }

    fn publish_nba(&mut self) -> Result<u64, RuntimeErrorCode> {
        loop {
            let update = if let Some(update) = self.nbas.pop_front() {
                Some(update)
            } else {
                let mut update = None;
                while let Some(&(process, index)) = self.metadata.flagged.get(self.pending_scan) {
                    self.pending_scan += 1;
                    let site = self.metadata.nbas[process][index].clone();
                    let pending = site.pending.expect("flagged NBA site");
                    if self.value(pending)?.0.bit(0) {
                        self.store_value(pending, &(BigUint::default(), BigUint::default()))?;
                        if site.width == 0 || site.lsb >= site.target.width {
                            continue;
                        }
                        update = Some(celox_runtime::NbaUpdate {
                            target: site.target,
                            lsb: site.lsb,
                            width: site.width,
                            value: self.value(site.value)?,
                        });
                        break;
                    }
                }
                update
            };
            let Some(update) = update else {
                self.pending_scan = 0;
                return Ok(0);
            };
            let before = self.value(update.target)?;
            let value = update.merge_into(&before);
            self.store_value(update.target, &value)?;
            // The compiled caller marks comb dirty and checks its waiters for a
            // changed publication. Identical writes cannot wake value-change or
            // edge waiters; consume them here without requesting another settle.
            // Compare the stored representation, including partial writes
            // and targets which do not keep an unknown-mask plane.
            if self.value(update.target)? != before {
                return Ok(2);
            }
        }
    }

    fn charge_scheduler(
        &mut self,
        process: u64,
        request: u64,
        timestamp: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let header = &mut self.arena.header;
        header.guard_timestamp(timestamp);
        if request == SCHEDULER_RUN {
            header.recent[(header.recent_next % header.recent.len() as u64) as usize] = process;
            header.recent_next = header.recent_next.saturating_add(1);
            header.work = header.work.saturating_add(1);
            // Ordinary suspensions do not consume the process-return budget.
            // This separate bound covers cycling through the native work list.
            if header.work > header.work_limit {
                return Err(self.scheduler_fault(timestamp, "work-list"));
            }
        } else {
            header.passes = header.passes.saturating_add(1);
            header.work = 0;
            if header.passes > header.wave_limit {
                return Err(self.scheduler_fault(timestamp, "round"));
            }
        }
        Ok(())
    }

    fn scheduler_fault(&self, timestamp: u64, budget: &str) -> RuntimeErrorCode {
        let mut processes = self.arena.header.recent.to_vec();
        processes.retain(|&process| process != u64::MAX);
        processes.sort_unstable();
        processes.dedup();
        let names: Vec<_> = processes
            .into_iter()
            .map(|process| {
                self.metadata
                    .process_names
                    .get(process as usize)
                    .cloned()
                    .unwrap_or_else(|| format!("process {process}"))
            })
            .collect();
        let origins = if names.is_empty() {
            String::new()
        } else {
            format!(": processes keep resuming: {}", names.join(", "))
        };
        Self::fault(format!(
            "time slot at {timestamp} does not settle within its {budget} budget{origins}"
        ))
    }

    fn charge_return(&mut self, process: u64, timestamp: u64) -> Result<(), RuntimeErrorCode> {
        let header = &mut self.arena.header;
        header.guard_timestamp(timestamp);
        header.returns = header.returns.saturating_add(1);
        if header.returns > header.return_limit {
            return Err(self.return_fault(process));
        }
        Ok(())
    }

    fn return_fault(&self, process: u64) -> RuntimeErrorCode {
        let name = self
            .metadata
            .process_names
            .get(process as usize)
            .cloned()
            .unwrap_or_else(|| format!("process {process}"));
        Self::fault(format!("{name} does not suspend within one time slot"))
    }

    fn fault(message: impl Into<String>) -> RuntimeErrorCode {
        RuntimeErrorCode::Runtime {
            message: message.into(),
            signals: Vec::new(),
        }
    }

    fn value(&self, signal: SignalRef) -> Result<(BigUint, BigUint), RuntimeErrorCode> {
        if signal.array_layout.is_some() {
            return Err(Self::fault(
                "native file-write argument is an unpacked array",
            ));
        }
        let plane = celox_state_layout::get_byte_size(signal.width);
        let unknowns = self.metadata.layout.four_state && signal.is_4state;
        let size = plane
            .checked_mul(if unknowns { 2 } else { 1 })
            .and_then(|size| signal.offset.checked_add(size))
            .filter(|&end| end <= self.state_size)
            .ok_or_else(|| Self::fault("native file-write argument is outside simulation state"))?;
        // SAFETY: the caller keeps native state live throughout this synchronous
        // scope, and the bounds above cover both planes. Native execution is
        // paused at this synchronous call while the callback reads its state.
        let bytes = unsafe {
            std::slice::from_raw_parts(self.state.add(signal.offset), size - signal.offset)
        };
        let bits = (BigUint::from(1u8) << signal.width) - 1u8;
        let value = BigUint::from_bytes_le(&bytes[..plane]) & &bits;
        let mask = if unknowns {
            BigUint::from_bytes_le(&bytes[plane..]) & bits
        } else {
            BigUint::default()
        };
        Ok((value, mask))
    }

    fn drain_events(&self, pending: &mut PendingRuntimeEvents) -> Result<(), RuntimeErrorCode> {
        let offset = celox_state_layout::STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET;
        if offset + 8 > self.state_size {
            return Err(Self::fault("native state has no runtime-event header"));
        }
        // SAFETY: the native ABI installs this live event-buffer pointer before
        // entering compiled code; this callback executes synchronously after
        // earlier records have been committed.
        let pointer = unsafe { self.state.add(offset).cast::<*const u8>().read_unaligned() };
        let layout = &self.metadata.layout;
        let mut read_seq = self.read_seq.load(Ordering::Acquire);
        let mut events = if pointer.is_null() {
            Vec::new()
        } else {
            let size = celox_state_layout::RUNTIME_EVENT_HEADER_SIZE
                + layout.runtime_event_capacity * layout.runtime_event_slot_size;
            collect_runtime_events(
                layout,
                &self.metadata.runtime_sites,
                &mut read_seq,
                size,
                |offset| unsafe { pointer.add(offset).cast::<u64>().read_volatile() },
                |offset| unsafe {
                    (&*pointer.add(offset).cast::<AtomicU64>()).load(Ordering::Acquire)
                },
            )
        };
        self.read_seq.store(read_seq, Ordering::Release);
        let heap = self.heap()?;
        for event in &mut events {
            event.resolve_handles(heap);
        }
        pending.extend(events, layout.runtime_event_capacity);
        pending.resolve_deferred_handles(heap);
        Ok(())
    }

    /// The heap of the active native backend. It installs its heap after
    /// code adoption, so the current pointer is read rather than retaining
    /// the earlier tier's.
    fn heap(&self) -> Result<&celox_heap::Heap, RuntimeErrorCode> {
        // SAFETY: the backend keeps the heap it installs in this state
        // image alive while the image runs.
        unsafe {
            self.state
                .add(celox_state_layout::STATE_HEADER_HEAP_ADDR_OFFSET)
                .cast::<*const celox_heap::Heap>()
                .read_unaligned()
                .as_ref()
        }
        .ok_or_else(|| Self::fault("native state has no active heap"))
    }

    fn capture_enabled(&self, site: usize) -> bool {
        // SAFETY: the backend installs a live flags array for every event site;
        // this scope and its metadata belong to that same state image.
        unsafe {
            let pointer = self
                .state
                .add(celox_state_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET)
                .cast::<*const u8>()
                .read_unaligned();
            *pointer.add(site) != 0
        }
    }

    fn before_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        self.comb_before = self
            .metadata
            .deferred_groups
            .iter()
            .map(|&(_, site)| self.capture_enabled(site))
            .collect();
        let mut pending = self.pending.lock().unwrap();
        self.drain_events(&mut pending)?;
        for (&(group, _), &active) in self.metadata.deferred_groups.iter().zip(&self.comb_before) {
            if active {
                pending.flush(group);
            }
        }
        Ok(())
    }

    fn after_comb(&self) {
        let mut pending = self.pending.lock().unwrap();
        // Do not collect the new records before cancelling the prior activation.
        for (&(group, site), &active) in self.metadata.deferred_groups.iter().zip(&self.comb_before)
        {
            if !active && self.capture_enabled(site) {
                pending.flush(group);
            }
        }
    }

    fn write(
        &mut self,
        process: u64,
        encoded: u64,
        timestamp: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let (HostRequestKind::FileIo, index) = HostRequestKind::decode(encoded)
            .ok_or_else(|| Self::fault("invalid native file-write request"))?
        else {
            return Err(Self::fault(
                "native file-write request has another service kind",
            ));
        };
        let listed = if index & celox_design::FUNCTION_SITE_NUMBER != 0 {
            &self.metadata.function_sites
        } else {
            &self.metadata.sites
        };
        let site = listed
            .get(usize::try_from(process).unwrap_or(usize::MAX))
            .and_then(|sites| sites.get((index & !celox_design::FUNCTION_SITE_NUMBER) as usize))
            .and_then(Option::as_ref)
            .ok_or_else(|| Self::fault("native file-write request is not registered"))?;
        let values = site
            .request
            .args
            .iter()
            .map(|arg| self.value(arg.id))
            .collect::<Result<Vec<_>, _>>()?;
        let Some((descriptor, mask)) = values.first() else {
            return Err(Self::fault("file output has no descriptor"));
        };
        if *mask != BigUint::default() {
            return Err(Self::fault("file I/O descriptor/address contains X or Z"));
        }
        let descriptor = descriptor.to_u32_digits().first().copied().unwrap_or(0);
        let (args, message) = super::file_io::formatted_file_write(
            &site.request,
            &values,
            &site.format,
            timestamp,
            self.heap()?,
        );
        let (stdout, result) = self.files.write_with_stdout(descriptor, &message);
        if stdout {
            let mut format = site.format.clone();
            format.kind = if site.request.operation == 3 {
                RuntimeEventKind::Display
            } else {
                RuntimeEventKind::Write
            };
            self.queue_output(format, args)?;
        }
        result.map_err(|error| Self::fault(format!("file output: {error}")))
    }

    fn queue_output(
        &mut self,
        site: RuntimeEventSite,
        args: Vec<RuntimeEventArgValue>,
    ) -> Result<(), RuntimeErrorCode> {
        let mut pending = self.pending.lock().unwrap();
        self.drain_events(&mut pending)?;
        let layout = &self.metadata.layout;
        pending.push(
            RawRuntimeEvent::Host {
                site: Box::new(site),
                args,
            },
            layout.runtime_event_capacity,
        );
        drop(pending);
        // Host stdout records share the ring's bounded pending queue. Count
        // them in the same batch as RuntimeEvent instructions so the host can
        // honor a paced drain before the next run overflows that queue.
        let events = self.value(self.metadata.events)?.0 + 1u8;
        self.store_value(self.metadata.events, &(events, BigUint::default()))?;
        Ok(())
    }
}

pub(crate) extern "C" fn call(
    context: u64,
    process: u64,
    encoded: u64,
    timestamp: u64,
    detail: u64,
) -> u64 {
    if context == 0 {
        // A kernel run outside a scope has no observer to show its value
        // changes to.
        return u64::from(encoded != VALUE_CHANGE);
    }
    // SAFETY: Scope::run passes its exclusively borrowed scope through the
    // private context slot for this synchronous call and restores the slot
    // before returning or unwinding. No simulation-global state is used.
    let scope = unsafe { &mut *(context as *mut Scope<'_>) };
    scope.calls = scope.calls.saturating_add(1);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if encoded == BEFORE_DOMAINS {
            return match scope.observer.as_deref_mut() {
                Some(observer) => {
                    Ok(observer.before_domains(timestamp, scope.metadata, scope.arena))
                }
                None => Ok(0),
            };
        } else if encoded == AFTER_UPDATE {
            return match scope.observer.as_deref_mut() {
                Some(observer) => Ok(observer.after_update(timestamp, scope.metadata, scope.arena)),
                None => Ok(0),
            };
        } else if encoded == VALUE_CHANGE {
            if scope.updates
                && let Some(observer) = scope.observer.as_deref_mut()
            {
                observer.value_change(timestamp);
            }
            return Ok(0);
        } else if matches!(encoded, ARENA_RESERVE_RECORDS | ARENA_RESERVE_IMAGES) {
            scope
                .arena
                .reserve(encoded == ARENA_RESERVE_IMAGES, process)?;
        } else if encoded == ARENA_RESERVE_NBA {
            scope
                .arena
                .nba
                .reserve_class(&mut scope.arena.header.nba, process)?;
        } else if encoded == ARENA_RESERVE_DELAYED_NBA {
            scope
                .arena
                .nba
                .reserve_delayed(&mut scope.arena.header.nba, process)?;
        } else if encoded == NBA_TIME_OVERFLOW {
            return Err(Scope::fault(
                "nonblocking assignment delay overflows simulation time",
            ));
        } else if encoded == ARENA_WAVE_LIMIT {
            return Err(Scope::fault(
                "native ordered domain waves exceeded the round budget",
            ));
        } else if encoded == GUARD_FAULT {
            return Err(match detail {
                SCHEDULER_RUN => scope.scheduler_fault(timestamp, "work-list"),
                SCHEDULER_PASS => scope.scheduler_fault(timestamp, "round"),
                CHECKPOINT => scope.return_fault(process),
                _ => Scope::fault("invalid native budget guard"),
            });
        } else if matches!(encoded, SCHEDULER_RUN | SCHEDULER_PASS) {
            scope.charge_scheduler(process, encoded, timestamp)?;
        } else if encoded == PROCESS_PHASE {
            // One counter spans every activation/pass of process_phase.
            // A new phase after NBA publication starts a fresh counter.
            scope.arena.header.returns = 0;
        } else if encoded == COMB_BEFORE {
            scope.before_comb()?;
        } else if encoded == COMB_AFTER {
            scope.after_comb();
        } else if encoded == NBA_PUBLISH {
            return scope.publish_nba();
        } else {
            if encoded != CHECKPOINT {
                if encoded & NBA_REQUEST != 0 {
                    scope.queue_nba(process, encoded & !NBA_REQUEST)?;
                } else {
                    scope.write(process, encoded, timestamp)?;
                }
            }
            scope.charge_return(process, timestamp)?;
        }
        Ok(0)
    }))
    .unwrap_or_else(|_| Err(Scope::fault("native host callback panicked")));
    match result {
        Ok(result) => result,
        Err(error) => {
            scope.error = Some(error);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_nba_publication_skips_identical_queue_and_flagged_updates() {
        let simulation = crate::Simulator::builder("module Top (a: input logic<8>) {}", "Top")
            .four_state(true)
            .build_interpreter()
            .unwrap();
        let signal = |offset, width, is_4state| SignalRef {
            offset,
            width,
            is_4state,
            array_layout: None,
        };
        let target = signal(0, 8, true);
        let staged = [signal(4, 8, true), signal(6, 8, true)];
        let flags = [signal(8, 1, false), signal(9, 1, false)];
        let mut layout = simulation.layout().clone();
        layout.settle = Some(celox_state_layout::SettleLayout::new(
            12,
            1,
            vec![celox_state_layout::SettleObject {
                start: 0,
                end: 2,
                partitions: vec![0],
            }],
        ));
        let dirty = signal(12, 8, false);
        let metadata = Metadata {
            layout,
            context: target,
            events: signal(0, 64, false),
            sites: vec![],
            function_sites: vec![],
            runtime_sites: vec![],
            process_names: vec![],
            flagged: vec![(0, 0), (0, 1)],
            deferred_groups: Vec::new(),
            native_nbas: Vec::new(),
            host_inputs: crate::HashMap::default(),
            nbas: vec![
                staged
                    .iter()
                    .zip(flags)
                    .map(|(&value, pending)| celox_design::NbaSite {
                        delay: 0,
                        process: 0,
                        target,
                        lsb: 0,
                        width: 8,
                        value,
                        position: None,
                        pending: Some(pending),
                        unit: false,
                    })
                    .collect(),
            ],
        };
        let mut state = [0u8; 16];
        let mut files = celox_runtime::file_io::FileIo::default();
        let pending = Arc::default();
        let read_seq = AtomicU64::new(0);
        let mut queue = std::collections::VecDeque::new();
        let mut arena = super::super::native_arena::Arena::default();
        let mut scope = Scope {
            arena: &mut arena,
            metadata: &metadata,
            state: state.as_mut_ptr().cast_const(),
            state_size: state.len(),
            files: &mut files,
            pending: &pending,
            read_seq: &read_seq,
            error: None,
            calls: 0,
            return_budget: u64::MAX,
            round_budget: u64::MAX,
            nbas: &mut queue,
            delayed: &mut std::collections::VecDeque::new(),
            pending_scan: 0,
            comb_before: Vec::new(),
            observer: None,
            updates: false,
        };
        let value = |bits: u8, mask: u8| (BigUint::from(bits), BigUint::from(mask));
        let update = |target, lsb, width, bits, mask| celox_runtime::NbaUpdate {
            target,
            lsb,
            width,
            value: value(bits, mask),
        };
        scope.store_value(target, &value(0x55, 0)).unwrap();
        scope.store_value(staged[0], &value(0x55, 0)).unwrap();
        scope.store_value(staged[1], &value(0xa5, 0)).unwrap();
        for flag in flags {
            scope.store_value(flag, &value(1, 0)).unwrap();
        }
        scope.store_value(dirty, &value(0, 0)).unwrap();
        scope.nbas.push_back(update(target, 0, 8, 0x55, 0));
        assert_eq!(scope.publish_nba().unwrap(), 2);
        assert_eq!(scope.value(target).unwrap(), value(0xa5, 0));
        assert_eq!(scope.value(dirty).unwrap(), value(1, 0));
        for flag in flags {
            assert_eq!(scope.value(flag).unwrap(), value(0, 0));
        }
        assert_eq!(scope.publish_nba().unwrap(), 0);
        assert!(scope.nbas.is_empty());

        scope.store_value(target, &value(0x55, 0)).unwrap();
        scope.store_value(dirty, &value(0, 0)).unwrap();
        scope.nbas.extend([
            update(target, 0, 4, 5, 0),
            update(target, 4, 4, 15, 15),
            update(target, 4, 4, 0, 15),
            update(target, 4, 4, 0, 15),
        ]);
        assert_eq!(scope.publish_nba().unwrap(), 2);
        assert_eq!(scope.value(target).unwrap(), value(0xf5, 0xf0));
        assert_eq!(scope.value(dirty).unwrap(), value(1, 0));
        scope.store_value(dirty, &value(0, 0)).unwrap();
        assert_eq!(scope.publish_nba().unwrap(), 2);
        assert_eq!(scope.value(target).unwrap(), value(5, 0xf0));
        assert_eq!(scope.value(dirty).unwrap(), value(1, 0));
        scope.store_value(dirty, &value(0, 0)).unwrap();
        assert_eq!(scope.publish_nba().unwrap(), 0);
        assert!(scope.nbas.is_empty());

        assert_eq!(scope.value(dirty).unwrap(), value(0, 0));

        // A target without a mask plane compares its stored representation.
        let bit_target = signal(10, 8, false);
        scope.nbas.push_back(update(bit_target, 0, 8, 0, 255));
        assert_eq!(scope.publish_nba().unwrap(), 0);
    }
}
