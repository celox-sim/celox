//! Construction of resumable process kernels.
//!
//! A process kernel is one SIR execution unit that runs a process from where
//! it last stopped to its next suspension. Its entry block loads the resume
//! slot and dispatches to the block after that suspension point. A
//! suspension stores the next resume point, a [`ProcessStatus`] and its wait
//! amount into the process's control slots and returns to the runtime.
//!
//! A delay resumes after a time. An event or level wait resumes at the first
//! occurrence of one of the terms of its [`WaitSite`] after the wait began.
//! A level wait, or an event control on an expression, checks its condition
//! at the resume point and either continues or reports
//! [`ProcessStatus::Pending`].
//!
//! The builder is source-independent: a frontend lowers statements and
//! expressions into [`ProcessKernelBuilder::builder`] and calls the
//! suspension methods where the source suspends. Wait and nonblocking
//! sites are numbered per process, in the order the frontend lists them in
//! the module's metadata.
//!
//! [`WaitSite`]: celox_design::WaitSite

use celox_design::{
    HostRequestKind, NbaPosition, NbaSite, PROCESS_DELAY_WIDTH, PROCESS_STATUS_WIDTH, ProcessSlots,
    ProcessStatus, RegionedVarAddrBase, STABLE_REGION,
};
use celox_sir::{
    BlockId, ExecutionUnit, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRSwitchCase,
    SIRTerminator, SIRValue,
};
use thiserror::Error;

use crate::SourceVarId;

type RegionedSourceAddr<A> = RegionedVarAddrBase<A>;

/// Widest selector of a single `Switch`.
const SWITCH_BITS: usize = 8;

/// Width of a process's resume slot. Its high byte is zero until a process
/// has more than 255 suspension points.
pub const PROCESS_RESUME_WIDTH: usize = 2 * SWITCH_BITS;

/// Most suspension points one process kernel can have.
pub const MAX_PROCESS_SUSPENSIONS: usize = (1 << PROCESS_RESUME_WIDTH) - 1;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProcessKernelError {
    #[error("a process has more than {MAX_PROCESS_SUSPENSIONS} suspension points")]
    TooManySuspensions,
}

/// Width of the bit offset and of the bit count staged by a nonblocking
/// assignment whose position is computed at run time ([`NbaPosition`]).
pub const NBA_POSITION_WIDTH: usize = 64;

/// Builder of one process kernel.
pub struct ProcessKernelBuilder<A = SourceVarId> {
    builder: SIRBuilder<RegionedSourceAddr<A>>,
    slots: ProcessSlots<A>,
    /// The block each resume point continues in; point 0 is the start.
    resume_blocks: Vec<BlockId>,
    /// The wait site and predicate entry resume point being lowered.
    wait_site: Option<(u64, usize)>,
    /// The runtime event site the process reports at its flush points
    /// (see [`Self::flush_deferred_at`]).
    flush_site: Option<u32>,
}

impl<A: Copy> ProcessKernelBuilder<A> {
    /// Start a kernel. The builder is positioned at the start of the
    /// process body.
    pub fn new(slots: ProcessSlots<A>) -> Self {
        let builder = SIRBuilder::new();
        let start = builder.current_block();
        Self {
            builder,
            slots: ProcessSlots {
                const_delays: true,
                ..slots
            },
            resume_blocks: vec![start],
            wait_site: None,
            flush_site: None,
        }
    }

    /// Report runtime event site `site` whenever the process resumes from
    /// an event control or a wait: a flush point of its deferred
    /// assertions (IEEE 1800-2023 16.4.2). A resume after a delay is not.
    pub fn flush_deferred_at(&mut self, site: u32) {
        self.flush_site = Some(site);
    }

    /// Number of continuation entries emitted so far.
    pub fn resume_count(&self) -> usize {
        self.resume_blocks.len()
    }

    /// The kernel's control slots, with [`ProcessSlots::const_delays`] set
    /// for the delays lowered so far.
    pub fn slots(&self) -> ProcessSlots<A> {
        self.slots
    }

    /// The SIR builder positioned where the process continues. After
    /// [`Self::finish`] no block is open until the caller switches to one.
    pub fn builder(&mut self) -> &mut SIRBuilder<RegionedSourceAddr<A>> {
        &mut self.builder
    }

    fn store_constant(&mut self, slot: A, width: usize, value: u64) {
        let register = self.builder.alloc_bit(width, false);
        self.builder
            .emit(SIRInstruction::Imm(register, SIRValue::new(value)));
        self.store(slot, width, register);
    }

    fn store(&mut self, slot: A, width: usize, value: RegisterId) {
        self.builder.emit(SIRInstruction::Store(
            stable(slot),
            SIROffset::Static(0),
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }

    /// Return to the runtime with `status`, leaving no block open.
    fn end(&mut self, status: ProcessStatus) {
        self.store_constant(
            self.slots.status,
            PROCESS_STATUS_WIDTH,
            u64::from(status.code()),
        );
        self.builder.seal_block(SIRTerminator::Return);
    }

    /// Store `value` into the delay slot.
    fn store_delay_constant(&mut self, value: u64) {
        self.store_constant(self.slots.delay, PROCESS_DELAY_WIDTH, value);
    }

    /// Store the next resume point, return with `status` and continue in a
    /// new block at that point.
    fn suspend(&mut self, status: ProcessStatus) -> Result<(), ProcessKernelError> {
        let point = self.resume_blocks.len();
        if point > MAX_PROCESS_SUSPENSIONS {
            return Err(ProcessKernelError::TooManySuspensions);
        }
        self.store_constant(self.slots.resume, PROCESS_RESUME_WIDTH, point as u64);
        self.end(status);
        let resume = self.builder.new_block();
        self.resume_blocks.push(resume);
        self.builder.switch_to_block(resume);
        Ok(())
    }

    /// Open a separate dispatch entry for a pure event-expression probe.
    /// The caller must close it with `end_event_probe`, without scheduling
    /// operations or executing the waiting process's continuation.
    pub fn begin_event_probe(&mut self) -> Result<(BlockId, u32), ProcessKernelError> {
        let resume = self.resume_blocks.len();
        if resume > MAX_PROCESS_SUSPENSIONS {
            return Err(ProcessKernelError::TooManySuspensions);
        }
        let previous = self.builder.new_block();
        self.builder
            .seal_block(SIRTerminator::Jump(previous, Vec::new()));
        let probe = self.builder.new_block();
        self.resume_blocks.push(probe);
        self.builder.switch_to_block(probe);
        Ok((previous, resume as u32))
    }

    pub fn end_event_probe(&mut self, previous: BlockId) {
        self.end(ProcessStatus::EventProbe);
        self.builder.switch_to_block(previous);
    }

    /// Suspend for the number of time units in `amount`, an unsigned
    /// two-state register of [`PROCESS_DELAY_WIDTH`] bits, and continue at
    /// a new resume point. A zero delay resumes after the nonblocking
    /// assignments of the time are published. `constant` is the amount's
    /// value when the frontend knows it, which it records in
    /// [`ProcessSlots::const_delays`].
    pub fn delay(
        &mut self,
        amount: RegisterId,
        constant: Option<u64>,
    ) -> Result<(), ProcessKernelError> {
        self.note_delay(constant);
        self.store(self.slots.delay, PROCESS_DELAY_WIDTH, amount);
        self.suspend(ProcessStatus::Delay)
    }

    /// A delay with the scheduling of IEEE 1800-2023: a zero delay resumes
    /// in the inactive region of the same time (4.4.2.3), before
    /// nonblocking assignments are published.
    pub fn inactive_delay(
        &mut self,
        amount: RegisterId,
        constant: Option<u64>,
    ) -> Result<(), ProcessKernelError> {
        self.note_delay(constant);
        self.store(self.slots.delay, PROCESS_DELAY_WIDTH, amount);
        self.suspend(ProcessStatus::InactiveDelay)
    }

    /// A concurrent assertion's Observed predicate / Reactive action boundary.
    pub fn assertion_region(&mut self, reactive: bool) -> Result<(), ProcessKernelError> {
        self.slots.const_delays = false;
        self.suspend(if reactive {
            ProcessStatus::AssertionReactive
        } else {
            ProcessStatus::AssertionObserved
        })
    }

    fn note_delay(&mut self, constant: Option<u64>) {
        self.slots.const_delays &= constant.is_some_and(|amount| amount > 0);
    }

    /// Return to the runtime, which charges its time-slot budget and
    /// resumes the process at once. Frontends place this in loops without
    /// timing controls so that a runaway process is reported, not hung in.
    pub fn yield_now(&mut self) -> Result<(), ProcessKernelError> {
        self.suspend(ProcessStatus::Yield)
    }

    /// Reserve one entry point for each child, then return to the runtime to spawn them.
    /// The caller lowers each child into its entry block and ends it with `complete`.
    pub fn fork(
        &mut self,
        count: usize,
        join: celox_design::ForkJoin,
    ) -> Result<(Vec<BlockId>, BlockId), ProcessKernelError> {
        let first = self.resume_blocks.len();
        if first + count > MAX_PROCESS_SUSPENSIONS {
            return Err(ProcessKernelError::TooManySuspensions);
        }
        let children: Vec<_> = (0..count).map(|_| self.builder.new_block()).collect();
        self.resume_blocks.extend(children.iter().copied());
        let mode = match join {
            celox_design::ForkJoin::All => 0u64,
            celox_design::ForkJoin::Any => 1,
            celox_design::ForkJoin::None => 2,
        };
        self.store_delay_constant(first as u64 | ((count as u64) << 16) | (mode << 32));
        self.slots.const_delays = false;
        self.slots.requires_timed_simulation = true;
        self.suspend(ProcessStatus::Fork)?;
        Ok((children, self.builder.current_block()))
    }

    /// Wait for the calling process's immediate children (IEEE 1800-2023 9.6.1).
    pub fn wait_fork(&mut self) -> Result<(), ProcessKernelError> {
        self.slots.const_delays = false;
        self.slots.requires_timed_simulation = true;
        self.suspend(ProcessStatus::WaitFork)
    }

    /// Cancel descendants without suspending HDL execution (9.6.3).
    pub fn disable_fork(&mut self) -> Result<(), ProcessKernelError> {
        self.slots.const_delays = false;
        self.suspend(ProcessStatus::DisableFork)
    }

    pub fn local(&mut self, slot: usize) -> Result<(), ProcessKernelError> {
        self.store_delay_constant(slot as u64);
        self.suspend(ProcessStatus::Local)
    }

    /// Suspend the simulation (`$stop`), and continue at a new resume point
    /// when it continues.
    pub fn stop(&mut self) -> Result<(), ProcessKernelError> {
        self.suspend(ProcessStatus::Stop)
    }

    /// Perform a synchronous host operation, then continue at a new resume
    /// point. Arguments and results live in the request site's state slots.
    pub fn host_request(
        &mut self,
        kind: HostRequestKind,
        site: u32,
    ) -> Result<(), ProcessKernelError> {
        self.store_delay_constant(kind.encode(site));
        self.suspend(ProcessStatus::HostRequest)
    }

    /// Suspend at wait site `site` of this process. The open block
    /// afterwards is the site's resume point: the caller lowers the wait's
    /// condition there and ends it with [`Self::wake_if`].
    pub fn suspend_wait(&mut self, site: u32) -> Result<(), ProcessKernelError> {
        self.suspend_wait_word(u64::from(site))
    }

    pub fn suspend_level_wait(&mut self, site: u32) -> Result<(), ProcessKernelError> {
        self.suspend_wait_word(u64::from(site) | celox_design::PROCESS_LEVEL_WAIT)
    }

    fn suspend_wait_word(&mut self, site: u64) -> Result<(), ProcessKernelError> {
        self.store_delay_constant(site);
        self.suspend(ProcessStatus::Wait)?;
        self.wait_site = Some((site, self.resume_blocks.len() - 1));
        Ok(())
    }

    /// At the resume point of a [`Self::suspend_wait`]: continue when
    /// `condition`, a one-bit two-state register, holds; otherwise report
    /// that the wait is still pending and return to the same resume point.
    /// The continuation block is open afterwards.
    pub fn wake_if(&mut self, condition: RegisterId) {
        let (site, resume) = self.wait_site.take().expect("wake_if follows suspend_wait");
        let woken = self.builder.new_block();
        let pending = self.builder.new_block();
        self.builder.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (woken, Vec::new()),
            false_block: (pending, Vec::new()),
        });
        self.builder.switch_to_block(pending);
        // Predicate evaluation may itself yield to the runtime (for example,
        // disable fork). A later wakeup must evaluate the whole predicate.
        self.store_constant(self.slots.resume, PROCESS_RESUME_WIDTH, resume as u64);
        self.store_delay_constant(site);
        self.end(ProcessStatus::Pending);
        self.builder.switch_to_block(woken);
        if let Some(site_id) = self.flush_site {
            self.builder.emit(SIRInstruction::RuntimeEvent {
                site_id,
                args: Vec::new(),
            });
        }
    }

    /// `wait (condition)` (IEEE 1800-2023 9.4.3) at wait site `site`:
    /// `predicate` lowers the condition to a one-bit two-state register at
    /// the open block. It is evaluated once before suspending, so a
    /// condition that already holds does not suspend, and again whenever
    /// the process is resumed.
    pub fn wait_level(
        &mut self,
        site: u32,
        mut predicate: impl FnMut(&mut SIRBuilder<RegionedSourceAddr<A>>) -> RegisterId,
    ) -> Result<(), ProcessKernelError> {
        let holds = predicate(&mut self.builder);
        let join = self.builder.new_block();
        let wait = self.builder.new_block();
        self.builder.seal_block(SIRTerminator::Branch {
            cond: holds,
            true_block: (join, Vec::new()),
            false_block: (wait, Vec::new()),
        });
        self.builder.switch_to_block(wait);
        self.suspend_wait(site)?;
        let holds = predicate(&mut self.builder);
        self.wake_if(holds);
        self.builder
            .seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.builder.switch_to_block(join);
        Ok(())
    }

    /// The nonblocking assignment of nonblocking site `index` of this
    /// process, described by `site`: schedule a value for the NBA region of
    /// the current time (IEEE 1800-2023 10.4.2). `lower` lowers the
    /// right-hand side at the open block and returns the value, a register
    /// of `site.width` bits, and for a site with a run-time position the bit
    /// offset and the number of bits written, unsigned two-state registers
    /// of [`NBA_POSITION_WIDTH`] bits.
    ///
    /// A site with a `pending` flag stages the value without returning. If
    /// the flag is still set from an earlier execution in the same time
    /// slot, the kernel first returns [`ProcessStatus::Nba`] so the runtime
    /// queues the earlier value, and evaluates the right-hand side only
    /// after that.
    pub fn nonblocking<E: From<ProcessKernelError>>(
        &mut self,
        index: u32,
        site: &NbaSite<A>,
        lower: impl FnOnce(
            &mut SIRBuilder<RegionedSourceAddr<A>>,
        ) -> Result<(RegisterId, Option<NbaPosition<RegisterId>>), E>,
    ) -> Result<(), E> {
        if let Some(pending) = site.pending {
            let flag = self.builder.alloc_bit(1, false);
            self.builder.emit(SIRInstruction::Load(
                flag,
                stable(pending),
                SIROffset::Static(0),
                1,
            ));
            let flush = self.builder.new_block();
            let stage = self.builder.new_block();
            self.builder.seal_block(SIRTerminator::Branch {
                cond: flag,
                true_block: (flush, Vec::new()),
                false_block: (stage, Vec::new()),
            });
            self.builder.switch_to_block(flush);
            self.store_delay_constant(u64::from(index));
            self.suspend(ProcessStatus::Nba)?;
            self.builder
                .seal_block(SIRTerminator::Jump(stage, Vec::new()));
            self.builder.switch_to_block(stage);
        }
        let (value, position) = lower(&mut self.builder)?;
        self.store(site.value, site.width, value);
        if let (Some(slots), Some(position)) = (site.position, position) {
            self.store(slots.offset, NBA_POSITION_WIDTH, position.offset);
            self.store(slots.width, NBA_POSITION_WIDTH, position.width);
        }
        if let Some(pending) = site.pending {
            self.store_constant(pending, 1, 1);
            return Ok(());
        }
        self.store_delay_constant(u64::from(index));
        self.suspend(ProcessStatus::Nba)?;
        Ok(())
    }

    /// A nonblocking update of `site` with two paths that a load of the
    /// one-bit variable `switch` selects. With the switch set, the value
    /// `lower` computes is staged as [`Self::nonblocking`] does for a site
    /// with a `pending` flag. Otherwise the kernel returns
    /// [`ProcessStatus::Nba`] at once, for a site whose value object already
    /// holds the value. A frontend emits both before the whole design shows
    /// which one may be used; assembly keeps one
    /// (`crate::symbolic::artifact::NbaSwitch`).
    pub fn nonblocking_switched<E: From<ProcessKernelError>>(
        &mut self,
        index: u32,
        site: &NbaSite<A>,
        switch: A,
        lower: impl FnOnce(
            &mut SIRBuilder<RegionedSourceAddr<A>>,
        ) -> Result<(RegisterId, Option<NbaPosition<RegisterId>>), E>,
    ) -> Result<(), E> {
        assert!(
            site.pending.is_some(),
            "a switched nonblocking site has a pending flag"
        );
        let on = self.builder.alloc_bit(1, false);
        self.builder.emit(SIRInstruction::Load(
            on,
            stable(switch),
            SIROffset::Static(0),
            1,
        ));
        let staged = self.builder.new_block();
        let queued = self.builder.new_block();
        let join = self.builder.new_block();
        self.builder.seal_block(SIRTerminator::Branch {
            cond: on,
            true_block: (staged, Vec::new()),
            false_block: (queued, Vec::new()),
        });
        self.builder.switch_to_block(staged);
        self.nonblocking(index, site, lower)?;
        self.builder
            .seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.builder.switch_to_block(queued);
        self.store_delay_constant(u64::from(index));
        self.suspend(ProcessStatus::Nba)?;
        self.builder
            .seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.builder.switch_to_block(join);
        Ok(())
    }

    /// End only this activation, notifying a waiting fork parent.
    pub fn complete(&mut self) {
        self.end(ProcessStatus::Done);
    }

    /// End the simulation. No block is open afterwards.
    pub fn finish(&mut self) {
        self.wait_site = None;
        self.end(ProcessStatus::Finish);
    }

    /// End the kernel and the other kernels of its program instance
    /// (`$exit`, IEEE 1800-2023 24.7). No block is open afterwards.
    pub fn exit(&mut self) {
        self.wait_site = None;
        self.end(ProcessStatus::Exit);
    }

    /// Complete the kernel. If a block is still open, the process ends there.
    pub fn build(mut self) -> ExecutionUnit<RegionedSourceAddr<A>> {
        if self.builder.has_open_block() {
            self.end(ProcessStatus::Done);
        }
        // A resume value the kernel never stored cannot occur; ending the
        // process keeps such state inert.
        let invalid = self.builder.new_block();
        self.builder.switch_to_block(invalid);
        self.end(ProcessStatus::Done);

        let entry = self.builder.new_block();
        self.builder.switch_to_block(entry);
        let resume = self.builder.alloc_bit(PROCESS_RESUME_WIDTH, false);
        self.builder.emit(SIRInstruction::Load(
            resume,
            stable(self.slots.resume),
            SIROffset::Static(0),
            PROCESS_RESUME_WIDTH,
        ));
        let groups = self
            .resume_blocks
            .chunks(1 << SWITCH_BITS)
            .map(<[BlockId]>::to_vec)
            .collect::<Vec<_>>();
        let byte = |builder: &mut SIRBuilder<RegionedSourceAddr<A>>, lsb: usize| {
            let byte = builder.alloc_bit(SWITCH_BITS, false);
            builder.emit(SIRInstruction::Slice(byte, resume, lsb, SWITCH_BITS));
            byte
        };
        if let [targets] = groups.as_slice() {
            let low = byte(&mut self.builder, 0);
            let high = byte(&mut self.builder, SWITCH_BITS);
            // Every stored resume point fits the low byte.
            let in_range = self.builder.new_block();
            self.builder.seal_block(SIRTerminator::Switch {
                selector: high,
                cases: switch_cases(&[in_range]),
                default: invalid,
            });
            self.builder.switch_to_block(in_range);
            self.builder.seal_block(SIRTerminator::Switch {
                selector: low,
                cases: switch_cases(targets),
                default: invalid,
            });
        } else {
            let high = byte(&mut self.builder, SWITCH_BITS);
            let group_blocks = groups
                .iter()
                .map(|_| self.builder.new_block())
                .collect::<Vec<_>>();
            self.builder.seal_block(SIRTerminator::Switch {
                selector: high,
                cases: switch_cases(&group_blocks),
                default: invalid,
            });
            for (block, targets) in group_blocks.into_iter().zip(&groups) {
                self.builder.switch_to_block(block);
                let low = byte(&mut self.builder, 0);
                self.builder.seal_block(SIRTerminator::Switch {
                    selector: low,
                    cases: switch_cases(targets),
                    default: invalid,
                });
            }
        }

        let (blocks, register_map, _) = self.builder.drain();
        ExecutionUnit {
            entry_block_id: entry,
            blocks,
            register_map,
        }
    }
}

fn stable<A>(var_id: A) -> RegionedSourceAddr<A> {
    RegionedVarAddrBase {
        region: STABLE_REGION,
        var_id,
    }
}

fn switch_cases(targets: &[BlockId]) -> Vec<SIRSwitchCase> {
    targets
        .iter()
        .enumerate()
        .map(|(value, &target)| SIRSwitchCase {
            value: (value as u64).into(),
            target,
        })
        .collect()
}
