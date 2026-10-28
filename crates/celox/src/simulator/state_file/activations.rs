//! Named activation storage and graph validation for portable state files.
use super::*;
use celox_runtime::Activation;
use celox_runtime::timing::Value;
use celox_runtime::{ActivationRecord, FrameValueKind, FrameValueRecord};
use std::collections::BTreeSet;

fn kind(object: &NamedObject) -> FrameValueKind {
    if object.strings {
        FrameValueKind::String
    } else if object.objects {
        FrameValueKind::Object
    } else {
        FrameValueKind::Bits
    }
}
fn mismatch(message: impl Into<String>) -> StateError {
    StateError::Mismatch(StateMismatch {
        unknown_names: vec![message.into()],
        ..Default::default()
    })
}
fn frame_value_matches(
    value: &FrameValueRecord,
    object: &NamedObject,
    strings: &[Vec<u8>],
) -> bool {
    let bytes = object.signal.width.div_ceil(8);
    if value.width != object.signal.width as u64
        || value.is_4state != object.signal.is_4state
        || value.kind != kind(object)
        || value.value.len() != bytes
        || value.mask.len() != bytes
    {
        return false;
    }
    if !object.signal.width.is_multiple_of(8) {
        let high = !((1u8 << (object.signal.width % 8)) - 1);
        if value.value.last().is_some_and(|b| b & high != 0)
            || value.mask.last().is_some_and(|b| b & high != 0)
        {
            return false;
        }
    }
    match value.kind {
        FrameValueKind::Bits => object.signal.is_4state || value.mask.iter().all(|&b| b == 0),
        FrameValueKind::Object => value.value.iter().chain(&value.mask).all(|&b| b == 0),
        FrameValueKind::String => {
            value.mask.iter().all(|&b| b == 0)
                && value.value.len().is_multiple_of(celox_heap::HANDLE_BYTES)
                && value
                    .value
                    .as_chunks::<{ celox_heap::HANDLE_BYTES }>()
                    .0
                    .iter()
                    .all(|v| u32::from_le_bytes(*v) as usize <= strings.len())
        }
    }
}
fn decode_value(value: &FrameValueRecord, four_state: bool) -> Value {
    let bits = BigUint::from_bytes_le(&value.value);
    let mask = BigUint::from_bytes_le(&value.mask);
    if four_state && value.is_4state {
        (bits, mask)
    } else {
        let unknown = &bits & &mask;
        (bits - unknown, 0u8.into())
    }
}

impl<B: SimBackend> Simulator<B> {
    fn capture_frame_value(
        &self,
        object: &NamedObject,
        value: &Value,
        strings: &mut Vec<Vec<u8>>,
    ) -> Result<FrameValueRecord, StateError> {
        if object.objects && value.0 != BigUint::default() {
            return Err(StateError::Objects(object.path.clone()));
        }
        let bytes = object.signal.width.div_ceil(8);
        let encode = |value: &BigUint| {
            let mut bytes_value = value.to_bytes_le();
            bytes_value.resize(bytes, 0);
            bytes_value
        };
        let mut bits = encode(&value.0);
        if object.strings {
            for element in bits.as_chunks_mut::<{ celox_heap::HANDLE_BYTES }>().0 {
                let contents = self
                    .backend
                    .heap()
                    .string(celox_heap::Handle::from_le_bytes(*element));
                let id = if contents.is_empty() {
                    0
                } else {
                    let index = strings
                        .iter()
                        .position(|s| s == contents)
                        .unwrap_or_else(|| {
                            strings.push(contents.to_vec());
                            strings.len() - 1
                        });
                    u32::try_from(index + 1).map_err(|_| mismatch("too many strings"))?
                };
                *element = id.to_le_bytes();
            }
        }
        Ok(FrameValueRecord {
            width: object.signal.width as u64,
            is_4state: object.signal.is_4state,
            kind: kind(object),
            value: bits,
            mask: encode(&value.1),
        })
    }

    /// Queue payloads still use the legacy raw-bit codec. Reject heap-bearing
    /// payloads for activation files until that codec has portable value kinds.
    pub(crate) fn validate_activation_queue_values(
        &self,
        record: &ProcessRecord,
    ) -> Result<(), StateError> {
        if record.activations.is_empty()
            && record.reactive.is_empty()
            && record.observed.is_empty()
            && record.re_inactive.is_empty()
            && record.re_nba_queue.is_empty()
        {
            return Ok(());
        }
        let schema = self.state_schema();
        let heap_path = |name: &str| {
            schema
                .objects
                .iter()
                .any(|o| o.path == name && (o.strings || o.objects))
        };
        let nonzero = |value: &[u8], mask: &[u8]| value.iter().chain(mask).any(|b| *b != 0);
        if record
            .nba_queue
            .iter()
            .chain(&record.re_nba_queue)
            .any(|n| {
                heap_path(&n.target)
                    && (nonzero(&n.value, &n.mask)
                        || !n.lsb.is_multiple_of(celox_design::HANDLE_WIDTH as u64)
                        || !n.width.is_multiple_of(celox_design::HANDLE_WIDTH as u64))
            })
            || record
                .host_writes
                .iter()
                .any(|(name, value, mask)| heap_path(name) && nonzero(value, mask))
        {
            return Err(mismatch(
                "activation state has unsupported heap-bearing queued writes",
            ));
        }
        for waiter in &record.waiters {
            let kernel_name = match &waiter.process {
                ProcessRef::Kernel(name) => Some(name.as_str()),
                ProcessRef::Activation(id) => record
                    .activations
                    .iter()
                    .find(|a| a.id == *id)
                    .map(|a| a.kernel.as_str()),
            };
            let kernel = self.process_statuses().iter().position(|status| {
                schema
                    .objects
                    .iter()
                    .any(|o| o.signal == *status && Some(o.path.as_str()) == kernel_name)
            });
            if let Some(site) = kernel.and_then(|kernel| {
                self.program
                    .runtime_schema
                    .wait_sites
                    .iter()
                    .filter(|s| s.process == kernel)
                    .nth(waiter.site as usize)
            }) {
                if site
                    .terms
                    .iter()
                    .zip(&waiter.baselines)
                    .any(|((slot, _), baseline)| {
                        let (BaselineRecord::Value { value, mask }
                        | BaselineRecord::Counter { value, mask, .. }
                        | BaselineRecord::Gated { value, mask, .. }) = baseline;
                        nonzero(value, mask)
                            && self
                                .program
                                .design
                                .state_objects
                                .get(slot)
                                .is_some_and(|m| m.value_kind != celox_design::ValueKind::Bits)
                    })
                {
                    return Err(mismatch(
                        "activation state has unsupported heap-bearing wait baselines",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn capture_activation_values(
        &self,
        state: &ProcessState,
        record: &mut ProcessRecord,
        strings: &mut Vec<Vec<u8>>,
    ) -> Result<(), StateError> {
        if state.activations.is_empty() {
            return Ok(());
        }
        let schema = self.state_schema();
        let named = |slot: &crate::AbsoluteAddr| {
            let signal = self.backend.resolve_signal(slot);
            schema
                .objects
                .iter()
                .find(|o| o.signal == signal)
                .ok_or_else(|| mismatch("unnamed activation slot"))
        };
        let statuses = self.process_statuses();
        let mut cells: HashMap<usize, u64> = HashMap::default();
        for (index, activation) in state.activations.iter().enumerate() {
            let status = statuses
                .get(activation.kernel)
                .ok_or_else(|| mismatch("unknown activation kernel"))?;
            let kernel = schema
                .objects
                .iter()
                .find(|o| o.signal == *status)
                .ok_or_else(|| mismatch("unnamed activation kernel"))?
                .path
                .clone();
            let mut private = Vec::new();
            let mut locals = Vec::new();
            // Program exit may leave storage on dead activations. Preserve
            // their graph identities, but only live nodes own execution state.
            if activation.live
                && let Some(frame) = self
                    .program
                    .runtime_schema
                    .process_frames
                    .get(activation.kernel)
                    .and_then(Option::as_ref)
            {
                // A kernel not yet dispatched still keeps its initial frame in
                // backend memory. Materialize it so v3 validates every live
                // continuation, including roots not started at capture time.
                let initial;
                let (values, bindings) = if activation.private.is_empty() {
                    initial = frame
                        .private
                        .iter()
                        .map(|slot| {
                            let signal = self.backend.resolve_signal(slot);
                            if signal.is_4state {
                                self.backend.get_four_state(signal)
                            } else {
                                (self.backend.get(signal), BigUint::default())
                            }
                        })
                        .collect::<Vec<_>>();
                    (initial.as_slice(), vec![None; frame.locals.len()])
                } else {
                    (activation.private.as_slice(), activation.locals.clone())
                };
                for (slot, value) in frame.private.iter().zip(values) {
                    let object = named(slot)?;
                    private.push((
                        object.path.clone(),
                        self.capture_frame_value(object, value, strings)?,
                    ));
                }
                for (slot, cell) in frame.locals.iter().zip(&bindings) {
                    let object = named(slot)?;
                    let cell = match cell {
                        None => None,
                        Some(cell) => Some(match cells.get(cell) {
                            Some(&id) => id,
                            None => {
                                let id = record.cells.len() as u64;
                                let value = state
                                    .cells
                                    .get(*cell)
                                    .ok_or_else(|| mismatch("unknown activation cell"))?;
                                record
                                    .cells
                                    .push(self.capture_frame_value(object, value, strings)?);
                                cells.insert(*cell, id);
                                id
                            }
                        }),
                    };
                    locals.push((object.path.clone(), cell));
                }
            }
            record.activations.push(ActivationRecord {
                id: index as u64,
                root: index < statuses.len(),
                kernel,
                parent: activation.parent.map(|p| p as u64),
                live: activation.live,
                private,
                locals,
                pending: activation.pending.iter().map(|&id| id as u64).collect(),
                join: activation
                    .join
                    .as_ref()
                    .map(|(kind, ids)| (*kind, ids.iter().map(|&id| id as u64).collect())),
            });
        }
        Ok(())
    }

    pub(super) fn resolve_activation_values(
        &self,
        record: &ProcessRecord,
        strings: &[Vec<u8>],
        processes: &mut HashMap<ProcessRef, usize>,
    ) -> Result<(Vec<Activation>, Vec<Value>), Vec<String>> {
        let bad = |message: &str| vec![format!("activation state: {message}")];
        if record.activations.is_empty() {
            return if record.cells.is_empty() {
                Ok((Vec::new(), Vec::new()))
            } else {
                Err(bad("cells without activations"))
            };
        }
        let kernel_count = processes.len();
        let by_id: HashMap<_, _> = record.activations.iter().map(|a| (a.id, a)).collect();
        if by_id.len() != record.activations.len() {
            return Err(bad("duplicate activation ID"));
        }
        let mut roots = BTreeSet::new();
        let mut next = kernel_count;
        let mut mapping = HashMap::default();
        for a in &record.activations {
            let Some(&kernel) = processes.get(&ProcessRef::Kernel(a.kernel.clone())) else {
                return Err(bad("unknown kernel"));
            };
            if a.root && a.parent.is_some() {
                return Err(bad("root has a parent"));
            }
            let index = if a.parent.is_none() {
                // Retired child slots may be reusable tombstones. The explicit
                // root marker distinguishes these from named static kernels.
                if a.root {
                    if !roots.insert(kernel) {
                        return Err(bad("duplicate root kernel"));
                    }
                    kernel
                } else {
                    if a.live
                        || !a.private.is_empty()
                        || !a.locals.is_empty()
                        || !a.pending.is_empty()
                        || a.join.is_some()
                    {
                        return Err(bad("detached live activation without parent"));
                    }
                    let id = next;
                    next += 1;
                    id
                }
            } else {
                let id = next;
                next += 1;
                id
            };
            mapping.insert(a.id, (index, kernel));
        }
        if roots.len() != kernel_count {
            return Err(bad("missing root kernel"));
        }
        let schema = self.state_schema();
        let named = |slot: &crate::AbsoluteAddr| {
            let signal = self.backend.resolve_signal(slot);
            schema.objects.iter().find(|o| o.signal == signal)
        };
        let mut decoded: Vec<Option<Activation>> = vec![None; next];
        let mut used_cells = BTreeSet::new();
        let mut cell_slots = BTreeMap::new();
        for a in &record.activations {
            let (index, kernel) = mapping[&a.id];
            let mut ancestry = BTreeSet::new();
            ancestry.insert(a.id);
            let mut parent = a.parent;
            while let Some(id) = parent {
                if !ancestry.insert(id) {
                    return Err(bad("parent cycle"));
                }
                let ancestor = by_id.get(&id).ok_or_else(|| bad("unknown parent"))?;
                if ancestor.kernel != a.kernel {
                    return Err(bad("parent kernel differs"));
                }
                if ancestor.parent.is_none() && !ancestor.root {
                    return Err(bad("child ancestry does not reach a kernel root"));
                }
                parent = ancestor.parent;
            }
            let child_ids = |ids: &[u64]| -> Result<Vec<usize>, Vec<String>> {
                let mut unique = BTreeSet::new();
                ids.iter()
                    .map(|id| {
                        let child = by_id.get(id).ok_or_else(|| bad("unknown child"))?;
                        if !unique.insert(*id) || child.parent != Some(a.id) || !child.live {
                            return Err(bad("invalid child membership"));
                        }
                        Ok(mapping[id].0)
                    })
                    .collect()
            };
            if !a.live
                && (!a.private.is_empty()
                    || !a.locals.is_empty()
                    || !a.pending.is_empty()
                    || a.join.is_some())
            {
                return Err(bad("retired activation owns execution state"));
            }
            let frame = self
                .program
                .runtime_schema
                .process_frames
                .get(kernel)
                .and_then(Option::as_ref);
            let mut private = Vec::new();
            let mut locals = Vec::new();
            if !a.private.is_empty() || !a.locals.is_empty() {
                let frame = frame.ok_or_else(|| bad("kernel has no frame"))?;
                if a.private.len() != frame.private.len() || a.locals.len() != frame.locals.len() {
                    return Err(bad("frame shape differs"));
                }
                let values: HashMap<_, _> = a
                    .private
                    .iter()
                    .map(|(name, value)| (name.as_str(), value))
                    .collect();
                let bindings: HashMap<_, _> = a
                    .locals
                    .iter()
                    .map(|(name, value)| (name.as_str(), value))
                    .collect();
                if values.len() != a.private.len() || bindings.len() != a.locals.len() {
                    return Err(bad("duplicate frame slot"));
                }
                for slot in &frame.private {
                    let object = named(slot).ok_or_else(|| bad("unnamed frame slot"))?;
                    let value = values
                        .get(object.path.as_str())
                        .ok_or_else(|| bad("missing private slot"))?;
                    if !frame_value_matches(value, object, strings) {
                        return Err(bad("private value shape or handle differs"));
                    }
                    private.push(decode_value(value, self.backend.layout().four_state));
                }
                // Only continuation entries emitted for this kernel are valid.
                // Legacy v2 loads retain their existing guarantees; this checks
                // the new portable activation graph before it can be imported.
                let slots = &self.program.runtime_schema.processes[kernel];
                let control = |slot: &crate::AbsoluteAddr| -> Result<u64, Vec<String>> {
                    let object = named(slot).ok_or_else(|| bad("missing control slot"))?;
                    let value = values
                        .get(object.path.as_str())
                        .ok_or_else(|| bad("missing private control slot"))?;
                    let bits = BigUint::from_bytes_le(&value.value);
                    if value.mask.iter().any(|&byte| byte != 0) || bits.bits() > 64 {
                        return Err(bad("invalid control value"));
                    }
                    Ok(bits.to_u64_digits().first().copied().unwrap_or(0))
                };
                let resume = control(&slots.resume)?;
                let status = control(&slots.status)?;
                let delay = control(&slots.delay)?;
                let is_probe = |entry: u64| {
                    frame
                        .event_probes
                        .iter()
                        .any(|probe| u64::from(probe.resume) == entry)
                };
                if resume >= frame.resume_count as u64
                    || is_probe(resume)
                    || status > u8::MAX as u64
                {
                    return Err(bad("invalid continuation or status"));
                }
                use celox_design::ProcessStatus;
                let status = ProcessStatus::from_code(status as u8);
                let valid = match status {
                    None => control(&slots.status)? == 0,
                    Some(ProcessStatus::EventProbe) => false,
                    Some(ProcessStatus::Local) => delay < frame.locals.len() as u64,
                    Some(ProcessStatus::Fork) => {
                        let first = delay & 0xffff;
                        let count = (delay >> 16) & 0xffff;
                        delay >> 32 <= 2
                            && first > 0
                            && count > 0
                            && first + count <= frame.resume_count as u64
                            && !(first..first + count).any(is_probe)
                    }
                    Some(ProcessStatus::Wait | ProcessStatus::Pending) => {
                        delay
                            < self
                                .program
                                .runtime_schema
                                .wait_sites
                                .iter()
                                .filter(|s| s.process == kernel)
                                .count() as u64
                    }
                    Some(ProcessStatus::HostRequest) => {
                        celox_design::HostRequestKind::decode(delay).is_some_and(|(kind, index)| {
                            self.program
                                .runtime_schema
                                .host_request_sites
                                .iter()
                                .filter(|site| site.process == kernel && site.kind == kind)
                                .nth(index as usize)
                                .is_some()
                        })
                    }
                    Some(ProcessStatus::Nba) => {
                        delay
                            < self
                                .program
                                .runtime_schema
                                .nba_sites
                                .iter()
                                .filter(|s| s.process == kernel)
                                .count() as u64
                    }
                    _ => true,
                };
                if !valid {
                    return Err(bad("invalid control payload"));
                }
                for slot in &frame.locals {
                    let object = named(slot).ok_or_else(|| bad("unnamed local slot"))?;
                    let cell = bindings
                        .get(object.path.as_str())
                        .ok_or_else(|| bad("missing local slot"))?;
                    let cell = cell
                        .map(|id| {
                            let id = usize::try_from(id).map_err(|_| bad("cell ID overflow"))?;
                            let value = record
                                .cells
                                .get(id)
                                .ok_or_else(|| bad("unknown shared cell"))?;
                            if !frame_value_matches(value, object, strings) {
                                return Err(bad("shared cell shape or handle differs"));
                            }
                            if cell_slots
                                .insert(id, object.path.as_str())
                                .is_some_and(|previous| previous != object.path)
                            {
                                return Err(bad("shared cell aliases different lexical slots"));
                            }
                            used_cells.insert(id);
                            Ok(id)
                        })
                        .transpose()?;
                    locals.push(cell);
                }
            } else if a.live && (a.parent.is_some() || frame.is_some()) {
                return Err(bad("live activation has no private frame"));
            }
            let pending = child_ids(&a.pending)?;
            let join = a
                .join
                .as_ref()
                .map(|(kind, ids)| {
                    if *kind == celox_design::ForkJoin::None || ids.is_empty() || !a.live {
                        return Err(bad("invalid pending join"));
                    }
                    Ok((*kind, child_ids(ids)?))
                })
                .transpose()?;
            decoded[index] = Some(Activation {
                kernel,
                parent: a.parent.map(|id| mapping[&id].0),
                private,
                locals,
                live: a.live,
                pending,
                join,
            });
            processes.insert(ProcessRef::Activation(a.id), index);
        }
        if used_cells.len() != record.cells.len() {
            return Err(bad("unreferenced shared cell"));
        }
        let activations = decoded
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| bad("incomplete activation map"))?;
        let cells = record
            .cells
            .iter()
            .map(|v| decode_value(v, self.backend.layout().four_state))
            .collect();
        Ok((activations, cells))
    }

    pub(super) fn validate_activation_schedule(
        &self,
        record: &ProcessRecord,
        processes: &HashMap<ProcessRef, usize>,
        activations: &[Activation],
        errors: &mut Vec<String>,
    ) {
        if activations.is_empty() {
            return;
        }
        let mut scheduled = BTreeSet::new();
        for id in record
            .wakeups
            .iter()
            .map(|(id, _)| id)
            .chain(record.waiters.iter().map(|w| &w.process))
            .chain(&record.ready)
            .chain(&record.inactive)
            .chain(&record.reactive)
            .chain(&record.observed)
            .chain(&record.re_inactive)
        {
            if let Some(&index) = processes.get(id) {
                if !scheduled.insert(index)
                    || !activations[index].live
                    || activations[index].join.is_some()
                {
                    errors.push(format!("invalid scheduled {id}"));
                }
            }
        }
        for (ids, reactive) in [
            (&record.ready, false),
            (&record.inactive, false),
            (&record.re_inactive, true),
        ] {
            for id in ids {
                if let Some(&index) = processes.get(id) {
                    let kernel = activations[index].kernel;
                    if self.program.runtime_schema.processes[kernel]
                        .program
                        .is_some()
                        != reactive
                    {
                        errors.push(format!("wrong region for {id}"));
                    }
                }
            }
        }
        let control = |index: usize| -> Option<(u64, u64)> {
            let a = &activations[index];
            let frame = self.program.runtime_schema.process_frames[a.kernel].as_ref()?;
            let slots = &self.program.runtime_schema.processes[a.kernel];
            let value = |slot| {
                let index = frame.private.iter().position(|s| s == slot)?;
                Some(
                    a.private
                        .get(index)?
                        .0
                        .to_u64_digits()
                        .first()
                        .copied()
                        .unwrap_or(0),
                )
            };
            Some((value(&slots.status)?, value(&slots.delay)?))
        };
        for (id, _) in &record.wakeups {
            if let Some(&index) = processes.get(id) {
                if control(index).is_some_and(|(status, _)| {
                    !matches!(
                        celox_design::ProcessStatus::from_code(status as u8),
                        Some(
                            celox_design::ProcessStatus::Delay
                                | celox_design::ProcessStatus::InactiveDelay
                        )
                    )
                }) {
                    errors.push(format!("wakeup control differs for {id}"));
                }
            }
        }
        for waiter in &record.waiters {
            if let Some(&index) = processes.get(&waiter.process) {
                if control(index).is_some_and(|(status, site)| {
                    site != waiter.site
                        || !matches!(
                            celox_design::ProcessStatus::from_code(status as u8),
                            Some(
                                celox_design::ProcessStatus::Wait
                                    | celox_design::ProcessStatus::Pending
                            )
                        )
                }) {
                    errors.push(format!("waiter control differs for {}", waiter.process));
                }
            }
        }
        for id in record.inactive.iter().chain(&record.re_inactive) {
            if let Some(&index) = processes.get(id) {
                if control(index).is_some_and(|(status, delay)| {
                    delay != 0
                        || celox_design::ProcessStatus::from_code(status as u8)
                            != Some(celox_design::ProcessStatus::InactiveDelay)
                }) {
                    errors.push(format!("inactive control differs for {id}"));
                }
            }
        }
        for a in activations {
            for child in &a.pending {
                if scheduled.contains(child) {
                    errors.push("pending child already scheduled".into());
                }
            }
        }
    }

    /// Resolve validated portable string IDs only after the ordinary state load
    /// succeeds. No fallible operation follows before importing the new schedule.
    pub(crate) fn bind_activation_strings(
        &mut self,
        state: &mut ProcessState,
        record: &ProcessRecord,
        strings: &[Vec<u8>],
    ) {
        let mut bind = |value: &mut Value| {
            let mut bytes = value.0.to_bytes_le();
            bytes.resize(
                bytes.len().div_ceil(celox_heap::HANDLE_BYTES) * celox_heap::HANDLE_BYTES,
                0,
            );
            for element in bytes.as_chunks_mut::<{ celox_heap::HANDLE_BYTES }>().0 {
                let id = u32::from_le_bytes(*element) as usize;
                let content = id
                    .checked_sub(1)
                    .and_then(|i| strings.get(i))
                    .map_or(&[][..], Vec::as_slice);
                *element = self.backend.heap_mut().intern(content).to_le_bytes();
            }
            value.0 = BigUint::from_bytes_le(&bytes);
        };
        for activation in &mut state.activations {
            if let Some(frame) = self
                .program
                .runtime_schema
                .process_frames
                .get(activation.kernel)
                .and_then(Option::as_ref)
            {
                for (slot, value) in frame.private.iter().zip(&mut activation.private) {
                    if self
                        .program
                        .design
                        .state_objects
                        .get(slot)
                        .is_some_and(|m| m.value_kind == celox_design::ValueKind::StrRef)
                    {
                        bind(value);
                    }
                }
            }
        }
        for (value, saved) in state.cells.iter_mut().zip(&record.cells) {
            if saved.kind == FrameValueKind::String {
                bind(value);
            }
        }
    }
}
