//! File writes and loop checkpoints called by a native scheduler through the ordinary
//! extern-function ABI. The active scope borrows host services separately from
//! the backend; the callback never re-enters a Simulator or its scheduler.
use super::*;
use celox_design::{HostRequestKind, HostRequestSite};

pub(crate) const SYMBOL: &str = "__celox_native_services_v1";

pub(crate) const CHECKPOINT: u64 = u64::MAX;
pub(crate) const SCHEDULER_RUN: u64 = u64::MAX - 1;
pub(crate) const SCHEDULER_PASS: u64 = u64::MAX - 2;
pub(crate) const NBA_PUBLISH: u64 = u64::MAX - 3;
pub(crate) const PROCESS_PHASE: u64 = u64::MAX - 4;
pub(crate) const NBA_REQUEST: u64 = 1 << 63;

pub(crate) fn signature() -> celox_design::ExternSignature {
    let integer = celox_design::ExternType::Integer {
        width: 64,
        signed: false,
    };
    celox_design::ExternSignature {
        pure: false,
        result: Some(integer),
        arguments: vec![integer; 5],
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
    pub runtime_sites: Vec<RuntimeEventSite>,
    pub process_names: Vec<String>,
    pub nbas: Vec<Vec<celox_design::NbaSite<SignalRef>>>,
    pub flagged: Vec<(usize, usize)>,
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
                let process = &program.runtime_schema.processes[request.process];
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
            sites[request.process].push(site);
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
        Some(Arc::new(Self {
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

pub(super) struct Scope<'a> {
    pub metadata: &'a Metadata,
    pub state: *const u8,
    pub state_size: usize,
    pub files: &'a mut celox_runtime::file_io::FileIo,
    pub pending: &'a Arc<std::sync::Mutex<PendingRuntimeEvents>>,
    pub read_seq: &'a AtomicU64,
    pub error: Option<RuntimeErrorCode>,
    pub return_budget: u64,
    pub returns: u64,
    pub round_budget: u64,
    pub scheduler_time: Option<u64>,
    pub passes: u64,
    pub run_returns: u64,
    pub nbas: &'a mut std::collections::VecDeque<celox_runtime::NbaUpdate>,
    pub pending_scan: usize,
}

impl Scope<'_> {
    /// Pass this separately borrowed host context through a private state slot.
    /// Generated code loads it as the first argument of the ordinary C call.
    pub(super) fn run<B: SimBackend>(
        &mut self,
        backend: &mut B,
        index: usize,
    ) -> (Result<(), RuntimeErrorCode>, Option<RuntimeErrorCode>) {
        let pointer = std::ptr::from_mut(self) as u64;
        let slot = self.metadata.context;
        let previous: u64 = backend.get_as(slot);
        backend.set(slot, pointer);
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.run_process(index)));
        backend.set(slot, previous);
        let result = match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        (result, self.error.take())
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

    fn charge_scheduler(&mut self, request: u64, timestamp: u64) -> Result<(), RuntimeErrorCode> {
        if self.scheduler_time != Some(timestamp) {
            self.scheduler_time = Some(timestamp);
            self.passes = 0;
            self.run_returns = 0;
            self.returns = 0;
        }
        if request == SCHEDULER_RUN {
            self.run_returns = self.run_returns.saturating_add(1);
            // Ordinary activations can suspend on an event or delay and do
            // not consume the reference process-return budget. This guard is
            // the separate bound on cycling through the native work list.
            // A ready-list sweep may activate every registered process.
            let work_budget = self
                .round_budget
                .saturating_mul(self.metadata.process_names.len().max(1) as u64);
            if self.run_returns > work_budget {
                return Err(Self::fault(format!(
                    "time slot at {timestamp} does not settle within its work-list budget"
                )));
            }
        } else {
            self.passes = self.passes.saturating_add(1);
            self.run_returns = 0;
            if self.passes > self.round_budget {
                return Err(Self::fault(format!(
                    "time slot at {timestamp} does not settle within its round budget"
                )));
            }
        }
        Ok(())
    }

    fn charge_return(&mut self, process: u64, timestamp: u64) -> Result<(), RuntimeErrorCode> {
        if self.scheduler_time != Some(timestamp) {
            self.scheduler_time = Some(timestamp);
            self.passes = 0;
            self.run_returns = 0;
            self.returns = 0;
        }
        self.returns = self.returns.saturating_add(1);
        if self.returns > self.return_budget {
            let name = self
                .metadata
                .process_names
                .get(process as usize)
                .cloned()
                .unwrap_or_else(|| format!("process {process}"));
            return Err(Self::fault(format!(
                "{name} does not suspend within one time slot"
            )));
        }
        Ok(())
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
        let site = self
            .metadata
            .sites
            .get(usize::try_from(process).unwrap_or(usize::MAX))
            .and_then(|sites| sites.get(index as usize))
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
        let (args, message) =
            super::file_io::formatted_file_write(&site.request, &values, &site.format, timestamp);
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
        // The active native backend installs its heap after code adoption;
        // read the current pointer rather than retaining the earlier tier's.
        let heap = unsafe {
            self.state
                .add(celox_state_layout::STATE_HEADER_HEAP_ADDR_OFFSET)
                .cast::<*const celox_heap::Heap>()
                .read_unaligned()
                .as_ref()
        }
        .ok_or_else(|| Self::fault("native state has no active heap"))?;
        for event in &mut events {
            event.resolve_handles(heap);
        }
        pending.extend(events, layout.runtime_event_capacity);
        pending.resolve_deferred_handles(heap);
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
    _first: u64,
) -> u64 {
    if context == 0 {
        return 1;
    }
    // SAFETY: Scope::run passes its exclusively borrowed scope through the
    // private context slot for this synchronous call and restores the slot
    // before returning or unwinding. No simulation-global state is used.
    let scope = unsafe { &mut *(context as *mut Scope<'_>) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if matches!(encoded, SCHEDULER_RUN | SCHEDULER_PASS) {
            scope.charge_scheduler(encoded, timestamp)?;
        } else if encoded == PROCESS_PHASE {
            // One counter spans every activation/pass of process_phase.
            // A new phase after NBA publication starts a fresh counter.
            scope.returns = 0;
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
            runtime_sites: vec![],
            process_names: vec![],
            flagged: vec![(0, 0), (0, 1)],
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
                    })
                    .collect(),
            ],
        };
        let mut state = [0u8; 16];
        let mut files = celox_runtime::file_io::FileIo::default();
        let pending = Arc::default();
        let read_seq = AtomicU64::new(0);
        let mut queue = std::collections::VecDeque::new();
        let mut scope = Scope {
            metadata: &metadata,
            state: state.as_mut_ptr().cast_const(),
            state_size: state.len(),
            files: &mut files,
            pending: &pending,
            read_seq: &read_seq,
            error: None,
            return_budget: u64::MAX,
            returns: 0,
            round_budget: u64::MAX,
            scheduler_time: None,
            passes: 0,
            run_returns: 0,
            nbas: &mut queue,
            pending_scan: 0,
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
