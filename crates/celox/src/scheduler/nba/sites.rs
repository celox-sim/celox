//! Generated NBA capture and publication. Storage helpers never choose which
//! process runs or which queued update is published.
use super::*;
use celox_design::NbaSite;

pub(in crate::scheduler) struct Sites {
    queue: Queue,
    sites: Vec<NbaSite<AbsoluteAddr>>,
    global: Vec<usize>,
    site_count: usize,
    import_formats: HashSet<usize>,
    processes: Vec<Vec<usize>>,
    observations: Vec<observations::Observations>,
    objects: Vec<(writes::Object, writes::Object)>,
    edges: Vec<Vec<Edge>>,
}

#[derive(Clone, Copy)]
struct Edge {
    index: usize,
    signal: AbsoluteAddr,
    width: usize,
    rising: bool,
}

impl Sites {
    pub(in crate::scheduler) fn new(
        program: &LaidOutProgram,
        compiled: &HashSet<usize>,
        service: u32,
        events: &[Event],
    ) -> Self {
        let mut sites = Vec::new();
        let mut global = Vec::new();
        let mut processes = vec![Vec::new(); program.runtime.runtime_schema.processes.len()];
        for (id, site) in program.runtime.runtime_schema.nba_sites.iter().enumerate() {
            if compiled.contains(&site.process) {
                processes[site.process].push(sites.len());
            }
            sites.push(site.clone());
            global.push(id);
        }
        // Portable queues select the widest available staging format. Emit
        // its general range path once per target/plane format, rather than
        // duplicating it for every static assignment to the same destination.
        let mut formats: HashMap<(AbsoluteAddr, bool), usize> = HashMap::default();
        for (index, site) in sites.iter().enumerate() {
            let masked = program.layout().four_state
                && program
                    .layout()
                    .is_4states
                    .get(&site.value)
                    .copied()
                    .unwrap_or(true);
            for mask_required in [false, true] {
                if mask_required && !masked {
                    continue;
                }
                let format = formats.entry((site.target, mask_required)).or_insert(index);
                if sites[*format].width <= site.width {
                    *format = index;
                }
            }
        }
        let shared = program.shared_storage();
        let storage = |signal| shared.get(&signal).copied().unwrap_or(signal);
        let mut by_signal: HashMap<_, Vec<Edge>> = HashMap::default();
        for (index, event) in events.iter().enumerate() {
            by_signal
                .entry(storage(event.signal))
                .or_default()
                .push(Edge {
                    index,
                    signal: event.signal,
                    width: event.width.min(8),
                    rising: event.active != 0,
                });
        }
        Self {
            import_formats: formats.into_values().collect(),
            queue: Queue { service },
            edges: sites
                .iter()
                .map(|site| {
                    by_signal
                        .get(&storage(site.target))
                        .cloned()
                        .unwrap_or_default()
                })
                .collect(),
            global,
            site_count: program.runtime.runtime_schema.nba_sites.len(),
            objects: sites
                .iter()
                .map(|site| {
                    let object = |signal, width| writes::Object {
                        signal,
                        width,
                        element: program
                            .layout()
                            .unpacked_arrays
                            .get(&signal)
                            .map(|layout| layout.element_width),
                    };
                    (
                        object(site.value, site.width),
                        object(site.target, program.layout().widths[&site.target]),
                    )
                })
                .collect(),
            observations: sites
                .iter()
                .map(|site| observations::Observations::new(program, site.target))
                .collect(),
            sites,
            processes,
        }
    }

    fn range(g: &mut Generator<'_>, site: &NbaSite<AbsoluteAddr>) -> (RegisterId, RegisterId) {
        let base = g.constant(site.lsb as u64, 64);
        let max = g.constant(site.width as u64, 64);
        match site.position {
            None => (base, max),
            Some(position) => {
                let offset = g.load(
                    position.offset,
                    celox_frontend_core::process::NBA_POSITION_WIDTH,
                );
                let width = g.load(
                    position.width,
                    celox_frontend_core::process::NBA_POSITION_WIDTH,
                );
                let lsb = g.binary(64, base, BinaryOp::Add, offset);
                let smaller = g.binary(1, width, BinaryOp::LtU, max);
                let count = g.b.alloc_bit(64, false);
                g.b.emit(SIRInstruction::Mux(count, smaller, width, max));
                (lsb, count)
            }
        }
    }

    fn class(site: &NbaSite<AbsoluteAddr>, g: &Generator<'_>) -> usize {
        let planes = if g.keeps_unknowns(site.value) { 2 } else { 1 };
        (VALUE + site.width.div_ceil(64) * planes)
            .next_power_of_two()
            .trailing_zeros() as usize
    }

    /// The local site index is a full-width selector, including for processes
    /// with more than 256 assignments.
    pub(in crate::scheduler) fn capture(
        &self,
        g: &mut Generator<'_>,
        process: usize,
        selector: RegisterId,
    ) {
        let done = g.b.new_block();
        let bad = g.exit_block(SchedulerExit::Internal);
        let blocks: Vec<_> = self.processes[process]
            .iter()
            .map(|_| g.b.new_block())
            .collect();
        g.dispatch_sites(selector, &blocks, bad);
        for (&site_index, block) in self.processes[process].iter().zip(blocks) {
            g.b.switch_to_block(block);
            self.stage(g, site_index);
            g.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        }
        g.b.switch_to_block(done);
    }

    fn stage(&self, g: &mut Generator<'_>, index: usize) {
        let site = &self.sites[index];
        if let Some(pending) = site.pending {
            let zero = g.constant(0, 1);
            g.store(pending, 1, zero);
        }
        if site.width == 0 {
            return;
        }
        let (lsb, width) = Self::range(g, site);
        let empty = g.equals(width, 0);
        let present = g.equals(empty, 0);
        g.if_bit(present, 0, |g| {
            let due = if site.delay != 0 {
                let now = g.load(g.slots.now, 64);
                let delay = g.constant(site.delay, 64);
                let due = g.binary(64, now, BinaryOp::Add, delay);
                let overflow = g.binary(1, due, BinaryOp::LtU, now);
                g.if_bit(overflow, 0, |g| {
                    g.arena_service(self.queue.service, native_io::NBA_TIME_OVERFLOW, now);
                });
                Some(due)
            } else {
                None
            };
            let record = self.queue.allocate(g, Self::class(site, g));
            let id = g.constant(self.global[index] as u64, 64);
            self.queue.write(g, record, SITE, id);
            self.queue.write(g, record, LSB, lsb);
            self.queue.write(g, record, WIDTH, width);
            let zero = g.constant(0, 64);
            self.queue.write(g, record, IMPORTED, zero);
            for offset in (0..site.width).step_by(64) {
                let width = (site.width - offset).min(64);
                let value = g.load_state(site.value, offset, width);
                let unknowns = g.keeps_unknowns(site.value);
                let (value, mask) = self.planes(g, value, width, unknowns);
                self.queue.write(g, record, VALUE + offset / 64, value);
                if let Some(mask) = mask {
                    self.queue.write(
                        g,
                        record,
                        VALUE + site.width.div_ceil(64) + offset / 64,
                        mask,
                    );
                }
            }
            if let Some(due) = due {
                self.queue.write(g, record, DUE, due);
                let sequence = self.queue.header_load(g, offset_of!(NbaHeader, sequence));
                self.queue.write(g, record, SEQUENCE, sequence);
                let one = g.constant(1, 64);
                let next = g.binary(64, sequence, BinaryOp::Add, one);
                self.queue
                    .header_store(g, offset_of!(NbaHeader, sequence), next);
                self.queue.delay(g, record);
            } else {
                self.queue.append(g, record);
            }
        });
    }

    fn planes(
        &self,
        g: &mut Generator<'_>,
        value: RegisterId,
        width: usize,
        unknowns: bool,
    ) -> (RegisterId, Option<RegisterId>) {
        let value = if width < 64 {
            let pad = if unknowns {
                g.b.alloc_logic(64 - width)
            } else {
                g.b.alloc_bit(64 - width, false)
            };
            g.b.emit(SIRInstruction::Imm(pad, SIRValue::new(0u64)));
            let full = if unknowns {
                g.b.alloc_logic(64)
            } else {
                g.b.alloc_bit(64, false)
            };
            g.b.emit(SIRInstruction::Concat(full, vec![pad, value]));
            full
        } else {
            value
        };
        if unknowns {
            let packed = g.b.alloc_bit(128, false);
            g.b.emit(SIRInstruction::Unary(
                packed,
                celox_design::UnaryOp::ToPlanes,
                value,
            ));
            let value = g.b.alloc_bit(64, false);
            let mask = g.b.alloc_bit(64, false);
            g.b.emit(SIRInstruction::Slice(value, packed, 0, 64));
            g.b.emit(SIRInstruction::Slice(mask, packed, 64, 64));
            (value, Some(mask))
        } else {
            (value, None)
        }
    }

    fn value(
        &self,
        g: &mut Generator<'_>,
        record: RegisterId,
        site: &NbaSite<AbsoluteAddr>,
        offset: usize,
        width: usize,
    ) -> RegisterId {
        let bits = self.queue.read(g, record, VALUE + offset / 64);
        let value = if g.keeps_unknowns(site.value) {
            let mask = self
                .queue
                .read(g, record, VALUE + site.width.div_ceil(64) + offset / 64);
            let packed = g.b.alloc_bit(128, false);
            g.b.emit(SIRInstruction::Concat(packed, vec![mask, bits]));
            let value = g.b.alloc_logic(64);
            g.b.emit(SIRInstruction::Unary(
                value,
                celox_design::UnaryOp::FromPlanes,
                packed,
            ));
            value
        } else {
            bits
        };
        if width == 64 {
            value
        } else {
            let narrow = if g.keeps_unknowns(site.value) {
                g.b.alloc_logic(width)
            } else {
                g.b.alloc_bit(width, false)
            };
            g.b.emit(SIRInstruction::Slice(narrow, value, 0, width));
            narrow
        }
    }

    fn static_write(
        &self,
        g: &mut Generator<'_>,
        site: &NbaSite<AbsoluteAddr>,
        record: Option<RegisterId>,
    ) -> RegisterId {
        let mut changed = g.constant(0, 1);
        for offset in (0..site.width).step_by(64) {
            let width = (site.width - offset).min(64);
            let before = g.load_state(site.target, site.lsb + offset, width);
            let value = match record {
                Some(record) => self.value(g, record, site, offset, width),
                None => g.load_state(site.value, offset, width),
            };
            g.store_state(site.target, site.lsb + offset, width, value);
            let after = g.load_state(site.target, site.lsb + offset, width);
            let differs = g.binary(1, before, BinaryOp::NeCase, after);
            changed = g.binary(1, changed, BinaryOp::Or, differs);
        }
        changed
    }

    fn update(
        &self,
        g: &mut Generator<'_>,
        index: usize,
        record: Option<RegisterId>,
        checks: &[WaitCheck<'_>],
    ) {
        let changed = self.apply(g, index, record);
        g.if_bit(changed, 0, |g| {
            g.mark_dirty();
            g.check_waits_at(checks, false);
        });
    }

    /// Write one site's update and pulse its scheduler edges, returning
    /// whether the target changed. The caller checks the waits.
    fn apply(&self, g: &mut Generator<'_>, index: usize, record: Option<RegisterId>) -> RegisterId {
        let site = &self.sites[index];
        let observation = &self.observations[index];
        let objects = self.objects[index];
        let edges = &self.edges[index];
        let before: Vec<_> = edges
            .iter()
            .map(|edge| g.level_at(edge.signal, edge.width))
            .collect();
        let snapshot = observation.before(g);
        let (source, target) = objects;
        let changed = if site.position.is_some()
            || source.element.is_some()
            || target.element.is_some()
            || site
                .lsb
                .checked_add(site.width)
                .is_none_or(|end| end > target.width)
        {
            let (lsb, width) = match record {
                Some(record) => (
                    self.queue.read(g, record, LSB),
                    self.queue.read(g, record, WIDTH),
                ),
                None => Self::range(g, site),
            };
            let width = writes::clip(g, lsb, width, target.width);
            writes::write(g, source, target, record, lsb, width)
        } else if let Some(record) = record.filter(|_| self.import_formats.contains(&index)) {
            let imported = self.queue.read(g, record, IMPORTED);
            let imported = g.equals(imported, 1);
            let general = g.b.new_block();
            let fixed = g.b.new_block();
            let changed = g.b.alloc_bit(1, false);
            let join = g.b.new_block_with(vec![changed]);
            g.b.seal_block(SIRTerminator::Branch {
                cond: imported,
                true_block: (general, Vec::new()),
                false_block: (fixed, Vec::new()),
            });
            g.b.switch_to_block(general);
            let lsb = self.queue.read(g, record, LSB);
            let width = self.queue.read(g, record, WIDTH);
            let width = writes::clip(g, lsb, width, target.width);
            let differs = writes::write(g, source, target, Some(record), lsb, width);
            g.b.seal_block(SIRTerminator::Jump(join, vec![differs]));
            g.b.switch_to_block(fixed);
            let differs = self.static_write(g, site, Some(record));
            g.b.seal_block(SIRTerminator::Jump(join, vec![differs]));
            g.b.switch_to_block(join);
            changed
        } else {
            self.static_write(g, site, record)
        };
        observation.after(g, snapshot);
        if !edges.is_empty() {
            let mut pulses = g.load_at(g.slots.exit_arg, g.event_bits, g.event_bits);
            for (edge, was) in edges.iter().zip(before) {
                let is = g.level_at(edge.signal, edge.width);
                let active = g.level_edge(edge.rising, was, is);
                let bit = g.event_bit(active, edge.index);
                pulses = g.binary(g.event_bits, pulses, BinaryOp::Or, bit);
            }
            g.store_at(g.slots.exit_arg, g.event_bits, g.event_bits, pulses);
        }
        changed
    }

    /// Include the heap root when selecting the next generated time slot.
    pub(in crate::scheduler) fn next_time(
        &self,
        g: &mut Generator<'_>,
        time: RegisterId,
        any: RegisterId,
    ) -> (RegisterId, RegisterId) {
        let head = self.queue.header_load(g, offset_of!(NbaHeader, head));
        let empty = g.equals(head, 0);
        let pending = g.equals(empty, 0);
        let now = g.load(g.slots.now, 64);
        let current = g.b.alloc_bit(64, false);
        g.b.emit(SIRInstruction::Mux(current, pending, now, time));
        let time = current;
        let any = g.binary(1, any, BinaryOp::Or, pending);
        let len = self
            .queue
            .header_load(g, offset_of!(NbaHeader, delayed_len));
        let empty = g.equals(len, 0);
        let read = g.b.new_block();
        let next = g.b.alloc_bit(64, false);
        let present = g.b.alloc_bit(1, false);
        let join = g.b.new_block_with(vec![next, present]);
        g.b.seal_block(SIRTerminator::Branch {
            cond: empty,
            true_block: (join, vec![time, any]),
            false_block: (read, Vec::new()),
        });
        g.b.switch_to_block(read);
        let heap = self.queue.header_load(g, offset_of!(NbaHeader, delayed));
        let zero = g.constant(0, 64);
        let first = g.arena_load(heap, zero);
        let due = self.queue.read(g, first, DUE);
        let earlier = g.binary(1, due, BinaryOp::LtU, time);
        let minimum = g.b.alloc_bit(64, false);
        g.b.emit(SIRInstruction::Mux(minimum, earlier, due, time));
        let one = g.constant(1, 1);
        g.b.seal_block(SIRTerminator::Jump(join, vec![minimum, one]));
        g.b.switch_to_block(join);
        (next, present)
    }

    /// Promote due records before Active execution, preserving issue order.
    pub(in crate::scheduler) fn promote(&self, g: &mut Generator<'_>, now: RegisterId) {
        let again = g.b.new_block();
        let inspect = g.b.new_block();
        let pop = g.b.new_block();
        let done = g.b.new_block();
        g.b.seal_block(SIRTerminator::Jump(again, Vec::new()));
        g.b.switch_to_block(again);
        let len = self
            .queue
            .header_load(g, offset_of!(NbaHeader, delayed_len));
        let empty = g.equals(len, 0);
        g.b.seal_block(SIRTerminator::Branch {
            cond: empty,
            true_block: (done, Vec::new()),
            false_block: (inspect, Vec::new()),
        });
        g.b.switch_to_block(inspect);
        let heap = self.queue.header_load(g, offset_of!(NbaHeader, delayed));
        let zero = g.constant(0, 64);
        let first = g.arena_load(heap, zero);
        let due = self.queue.read(g, first, DUE);
        let ready = g.binary(1, due, BinaryOp::Eq, now);
        g.b.seal_block(SIRTerminator::Branch {
            cond: ready,
            true_block: (pop, Vec::new()),
            false_block: (done, Vec::new()),
        });
        g.b.switch_to_block(pop);
        let record = self.queue.pop_delayed(g);
        self.queue.append(g, record);
        g.b.seal_block(SIRTerminator::Jump(again, Vec::new()));
        g.b.switch_to_block(done);
    }

    pub(in crate::scheduler) fn publish(&self, g: &mut Generator<'_>, checks: &[WaitCheck<'_>]) {
        let again = g.b.new_block();
        let queued = g.b.new_block();
        let flagged = g.b.new_block();
        let bad = g.exit_block(SchedulerExit::Internal);
        g.b.seal_block(SIRTerminator::Jump(again, Vec::new()));
        g.b.switch_to_block(again);
        let head = self.queue.header_load(g, offset_of!(NbaHeader, head));
        let empty = g.equals(head, 0);
        g.b.seal_block(SIRTerminator::Branch {
            cond: empty,
            true_block: (flagged, Vec::new()),
            false_block: (queued, Vec::new()),
        });
        g.b.switch_to_block(queued);
        let record = self.queue.pop(g);
        let site = self.queue.read(g, record, SITE);
        // Every site's update joins one wait check: the queue can hold a
        // record of any site, and checking the waits in each site's block
        // would copy them once per site of the design.
        let changed = g.b.alloc_bit(1, false);
        let applied = g.b.new_block_with(vec![changed]);
        let blocks: Vec<_> = self.sites.iter().map(|_| g.b.new_block()).collect();
        let mut targets = vec![bad; self.site_count];
        for (&id, &block) in self.global.iter().zip(&blocks) {
            targets[id] = block;
        }
        g.dispatch_sites(site, &targets, bad);
        for (index, block) in blocks.into_iter().enumerate() {
            let site = &self.sites[index];
            g.b.switch_to_block(block);
            let differs = self.apply(g, index, Some(record));
            let class = Self::class(site, g);
            self.queue.release(g, record, class);
            g.b.seal_block(SIRTerminator::Jump(applied, vec![differs]));
        }
        g.b.switch_to_block(applied);
        g.if_bit(changed, 0, |g| {
            g.mark_dirty();
            g.check_waits_at(checks, false);
        });
        g.b.seal_block(SIRTerminator::Jump(again, Vec::new()));
        g.b.switch_to_block(flagged);
        for (index, site) in self.sites.iter().enumerate() {
            if self.processes[site.process].is_empty() {
                continue;
            }
            if let Some(pending) = site.pending {
                let flag = g.load(pending, 1);
                g.if_bit(flag, 0, |g| {
                    let zero = g.constant(0, 1);
                    g.store(pending, 1, zero);
                    if site.delay == 0 {
                        self.update(g, index, None, checks);
                    } else {
                        self.stage(g, index);
                    }
                });
            }
        }
    }
}
