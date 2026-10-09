//! Construction of resumable process kernels.
//!
//! A process kernel is one SIR execution unit that runs a process from where
//! it last stopped to its next suspension. Its entry block loads the resume
//! slot and dispatches to the block after that suspension point. A
//! suspension stores the next resume point, a [`ProcessStatus`] and its wait
//! amount into the process's control slots and returns to the runtime.
//!
//! A delay resumes after a time, a clock wait after a number of edges of a
//! process clock, and a host request once the host has served it. An event
//! or level wait resumes at a check the kernel runs itself: the runtime
//! resumes the process whenever the state may have changed, and the kernel
//! continues past the wait or reports that it is still pending.
//!
//! The builder is source-independent: a frontend lowers statements and
//! expressions into [`ProcessKernelBuilder::builder`] and calls the
//! suspension methods where the source suspends.

use celox_design::{
    PROCESS_CLOCK_WIDTH, PROCESS_DELAY_WIDTH, PROCESS_RELEASE_WIDTH, PROCESS_STATUS_WIDTH,
    ProcessSlots, ProcessStatus, RegionedVarAddrBase, STABLE_REGION,
};
use celox_sir::{
    BlockId, ExecutionUnit, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRSwitchCase,
    SIRTerminator, SIRValue,
};
use thiserror::Error;

use crate::SourceVarId;

type RegionedSourceAddr = RegionedVarAddrBase<SourceVarId>;

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
    #[error("a process waits on clock {0}, which its slots do not list")]
    UnknownClock(u32),
    #[error("a process ends a wait with release {0}, which its slots do not list")]
    UnknownRelease(u32),
    #[error("a process makes host request {0}, which its slots do not list")]
    UnknownHostRequest(u32),
}

/// The stable-region address of a control slot.
pub fn stable_slot(var_id: SourceVarId) -> RegionedSourceAddr {
    RegionedVarAddrBase {
        region: STABLE_REGION,
        var_id,
    }
}

/// Builder of one process kernel, addressed like its slots: `A` is the
/// stable-region address type of the kernel.
pub struct ProcessKernelBuilder<A = RegionedSourceAddr> {
    builder: SIRBuilder<A>,
    resume: A,
    status: A,
    delay: A,
    clock: A,
    release: A,
    clock_count: usize,
    release_count: usize,
    host_request_count: usize,
    /// The block each resume point continues in; point 0 is the start.
    resume_blocks: Vec<BlockId>,
}

impl ProcessKernelBuilder<RegionedSourceAddr> {
    /// Start a kernel whose slots are module variables. The builder is
    /// positioned at the start of the process body.
    pub fn new(slots: ProcessSlots<SourceVarId>) -> Self {
        Self::with_addresses(slots.map(stable_slot))
    }
}

impl<A: Clone> ProcessKernelBuilder<A> {
    /// Start a kernel whose slots are the given stable addresses. The
    /// builder is positioned at the start of the process body.
    pub fn with_addresses(slots: ProcessSlots<A>) -> Self {
        let builder = SIRBuilder::new();
        let start = builder.current_block();
        Self {
            builder,
            resume: slots.resume,
            status: slots.status,
            delay: slots.delay,
            clock: slots.clock,
            release: slots.release,
            clock_count: slots.clocks.len(),
            release_count: slots.releases.len(),
            host_request_count: slots.host_requests.len(),
            resume_blocks: vec![start],
        }
    }

    /// The SIR builder positioned where the process continues. After
    /// [`Self::build`] no block is open until the caller switches to one.
    pub fn builder(&mut self) -> &mut SIRBuilder<A> {
        &mut self.builder
    }

    /// Register a further clock the process waits on, for a frontend that
    /// finds the clocks while it lowers the body; returns its index. The
    /// slots the frontend emits must list it at that index.
    pub fn add_clock(&mut self) -> u32 {
        self.clock_count += 1;
        self.clock_count as u32 - 1
    }

    /// Register a further release a wait may end with; see
    /// [`Self::add_clock`].
    pub fn add_release(&mut self) -> u32 {
        self.release_count += 1;
        self.release_count as u32 - 1
    }

    /// Register a further host request; see [`Self::add_clock`].
    pub fn add_host_request(&mut self) -> u32 {
        self.host_request_count += 1;
        self.host_request_count as u32 - 1
    }

    fn store_constant(&mut self, slot: A, width: usize, value: u64) {
        let register = self.builder.alloc_bit(width, false);
        self.builder
            .emit(SIRInstruction::Imm(register, SIRValue::new(value)));
        self.store(slot, width, register);
    }

    fn store(&mut self, slot: A, width: usize, value: RegisterId) {
        self.builder.emit(SIRInstruction::Store(
            slot,
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
            self.status.clone(),
            PROCESS_STATUS_WIDTH,
            u64::from(status.code()),
        );
        self.builder.seal_block(SIRTerminator::Return);
    }

    /// Store the next resume point and open its block after `end` runs.
    fn suspend(&mut self, status: ProcessStatus) -> Result<(), ProcessKernelError> {
        let point = self.resume_blocks.len();
        if point > MAX_PROCESS_SUSPENSIONS {
            return Err(ProcessKernelError::TooManySuspensions);
        }
        self.store_constant(self.resume.clone(), PROCESS_RESUME_WIDTH, point as u64);
        self.end(status);
        let resume = self.builder.new_block();
        self.resume_blocks.push(resume);
        self.builder.switch_to_block(resume);
        Ok(())
    }

    /// Suspend for the number of time units in `amount`, an unsigned
    /// two-state register of [`PROCESS_DELAY_WIDTH`] bits, and continue at
    /// a new resume point.
    pub fn delay(&mut self, amount: RegisterId) -> Result<(), ProcessKernelError> {
        self.store(self.delay.clone(), PROCESS_DELAY_WIDTH, amount);
        self.suspend(ProcessStatus::Delay)
    }

    /// Suspend until `count` rising edges of the process clock `clock` (an
    /// index into the slots' clock list) have passed, and continue at a new
    /// resume point one period after the last of them. `count` is an
    /// unsigned two-state register of [`PROCESS_DELAY_WIDTH`] bits; a count
    /// of zero continues at once. With `release`, the runtime makes that
    /// write of the slots' release list when the wait is over.
    pub fn wait_clock(
        &mut self,
        clock: u32,
        count: RegisterId,
        release: Option<u32>,
    ) -> Result<(), ProcessKernelError> {
        if clock as usize >= self.clock_count {
            return Err(ProcessKernelError::UnknownClock(clock));
        }
        let release = match release {
            Some(release) if release as usize >= self.release_count => {
                return Err(ProcessKernelError::UnknownRelease(release));
            }
            Some(release) => u64::from(release) + 1,
            None => 0,
        };
        self.store(self.delay.clone(), PROCESS_DELAY_WIDTH, count);
        self.store_constant(self.clock.clone(), PROCESS_CLOCK_WIDTH, u64::from(clock));
        self.store_constant(self.release.clone(), PROCESS_RELEASE_WIDTH, release);
        self.suspend(ProcessStatus::WaitClock)
    }

    /// Ask the host to serve `request` (an index into the slots' request
    /// list) and continue at a new resume point once it has.
    pub fn host(&mut self, request: u32) -> Result<(), ProcessKernelError> {
        if request as usize >= self.host_request_count {
            return Err(ProcessKernelError::UnknownHostRequest(request));
        }
        self.store_constant(self.delay.clone(), PROCESS_DELAY_WIDTH, u64::from(request));
        self.suspend(ProcessStatus::Host)
    }

    /// Have the runtime settle the combinational logic, then continue at
    /// once. A process stores to design state directly, so a read of a
    /// combinational result after such a store needs this first.
    pub fn settle(&mut self) -> Result<(), ProcessKernelError> {
        self.suspend(ProcessStatus::Settle)
    }

    /// Suspend until a condition the kernel evaluates holds, and continue at
    /// a new resume point. The caller lowers the condition at that point
    /// and ends it with [`Self::wake_if`]; the runtime resumes the process
    /// there whenever the state may have changed. Sampling that an edge
    /// condition compares against is done by the caller before this call.
    pub fn begin_wait(&mut self) -> Result<(), ProcessKernelError> {
        self.suspend(ProcessStatus::Wait)
    }

    /// At the resume point of a [`Self::begin_wait`]: continue when
    /// `condition`, a one-bit two-state register, holds, otherwise report
    /// that the wait is still pending and return. The resume point is left
    /// unchanged, so the runtime resumes the process at the same check. The
    /// continuation block is open afterwards.
    pub fn wake_if(&mut self, condition: RegisterId) {
        let woken = self.builder.new_block();
        let pending = self.builder.new_block();
        self.builder.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (woken, Vec::new()),
            false_block: (pending, Vec::new()),
        });
        self.builder.switch_to_block(pending);
        self.end(ProcessStatus::Pending);
        self.builder.switch_to_block(woken);
    }

    /// End the simulation. No block is open afterwards.
    pub fn finish(&mut self) {
        self.end(ProcessStatus::Finish);
    }

    /// Complete the kernel. If a block is still open, the process ends there.
    pub fn build(mut self) -> ExecutionUnit<A> {
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
            self.resume.clone(),
            SIROffset::Static(0),
            PROCESS_RESUME_WIDTH,
        ));
        let groups = self
            .resume_blocks
            .chunks(1 << SWITCH_BITS)
            .map(<[BlockId]>::to_vec)
            .collect::<Vec<_>>();
        let byte = |builder: &mut SIRBuilder<A>, lsb: usize| {
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
