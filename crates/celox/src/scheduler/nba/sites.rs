//! Generated NBA capture and publication. Storage helpers never choose which
//! process runs or which queued update is published.
use super::*;
use celox_design::NbaSite;

pub(in crate::scheduler) struct Sites {
    queue: Queue,
    sites: Vec<NbaSite<AbsoluteAddr>>,
    global: Vec<usize>,
    site_count: usize,
    processes: Vec<Vec<usize>>,
    observations: Vec<observations::Observations>,
}

impl Sites {
    pub(in crate::scheduler) fn new(
        program: &LaidOutProgram,
        compiled: &HashSet<usize>,
        service: u32,
    ) -> Self {
        let mut sites = Vec::new();
        let mut global = Vec::new();
        let mut processes = vec![Vec::new(); program.runtime.runtime_schema.processes.len()];
        for (id, site) in program.runtime.runtime_schema.nba_sites.iter().enumerate() {
            if compiled.contains(&site.process) {
                processes[site.process].push(sites.len());
                sites.push(site.clone());
                global.push(id);
            }
        }
        Self {
            queue: Queue { service },
            global,
            site_count: program.runtime.runtime_schema.nba_sites.len(),
            observations: sites
                .iter()
                .map(|site| observations::Observations::new(program, site.target))
                .collect(),
            sites,
            processes,
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
            let site = &self.sites[site_index];
            if let Some(pending) = site.pending {
                let zero = g.constant(0, 1);
                g.store(pending, 1, zero);
            }
            if site.width != 0 {
                let class = Self::class(site, g);
                let record = self.queue.allocate(g, class);
                let id = g.constant(self.global[site_index] as u64, 64);
                self.queue.write(g, record, SITE, id);
                let lsb = g.constant(site.lsb as u64, 64);
                self.queue.write(g, record, LSB, lsb);
                let width = g.constant(site.width as u64, 64);
                self.queue.write(g, record, WIDTH, width);
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
                self.queue.append(g, record);
            }
            g.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        }
        g.b.switch_to_block(done);
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

    fn update(
        &self,
        g: &mut Generator<'_>,
        site: &NbaSite<AbsoluteAddr>,
        observation: &observations::Observations,
        record: Option<RegisterId>,
        checks: &[WaitCheck<'_>],
    ) {
        let snapshot = observation.before(g);
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
        observation.after(g, snapshot);
        g.if_bit(changed, 0, |g| {
            g.mark_dirty();
            g.check_waits_at(checks, false);
        });
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
        let blocks: Vec<_> = self.sites.iter().map(|_| g.b.new_block()).collect();
        let mut targets = vec![bad; self.site_count];
        for (&id, &block) in self.global.iter().zip(&blocks) {
            targets[id] = block;
        }
        g.dispatch_sites(site, &targets, bad);
        for ((site, observation), block) in self.sites.iter().zip(&self.observations).zip(blocks) {
            g.b.switch_to_block(block);
            self.update(g, site, observation, Some(record), checks);
            let class = Self::class(site, g);
            self.queue.release(g, record, class);
            g.b.seal_block(SIRTerminator::Jump(again, Vec::new()));
        }
        g.b.switch_to_block(flagged);
        for (site, observation) in self.sites.iter().zip(&self.observations) {
            if let Some(pending) = site.pending {
                let flag = g.load(pending, 1);
                g.if_bit(flag, 0, |g| {
                    let zero = g.constant(0, 1);
                    g.store(pending, 1, zero);
                    self.update(g, site, observation, None, checks);
                });
            }
        }
    }
}
