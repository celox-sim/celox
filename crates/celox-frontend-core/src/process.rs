//! Construction of resumable process kernels.
//!
//! A process kernel is one SIR execution unit that runs a process from where
//! it last stopped to its next suspension. Its entry block loads the resume
//! slot and dispatches to the block after that suspension point. A
//! suspension stores the next resume point, a [`ProcessStatus`] and its wait
//! amount into the process's control slots and returns to the runtime.
//!
//! A delay resumes after a time. An event or level wait resumes at a check
//! the kernel runs itself: the runtime resumes the process whenever the
//! state may have changed, and the kernel continues past the wait or
//! reports that it is still pending.
//!
//! The builder is source-independent: a frontend lowers statements and
//! expressions into [`ProcessKernelBuilder::builder`] and calls the
//! suspension methods where the source suspends.

use celox_design::{
    PROCESS_CLOCK_WIDTH, PROCESS_DELAY_WIDTH, PROCESS_STATUS_WIDTH, ProcessSlots, ProcessStatus,
    RegionedVarAddrBase, STABLE_REGION,
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
}

/// Builder of one process kernel.
pub struct ProcessKernelBuilder {
    builder: SIRBuilder<RegionedSourceAddr>,
    slots: ProcessSlots<SourceVarId>,
    /// The block each resume point continues in; point 0 is the start.
    resume_blocks: Vec<BlockId>,
}

impl ProcessKernelBuilder {
    /// Start a kernel. The builder is positioned at the start of the
    /// process body.
    pub fn new(slots: ProcessSlots<SourceVarId>) -> Self {
        let builder = SIRBuilder::new();
        let start = builder.current_block();
        Self {
            builder,
            slots,
            resume_blocks: vec![start],
        }
    }

    /// The SIR builder positioned where the process continues. After
    /// [`Self::finish`] no block is open until the caller switches to one.
    pub fn builder(&mut self) -> &mut SIRBuilder<RegionedSourceAddr> {
        &mut self.builder
    }

    fn store_constant(&mut self, slot: SourceVarId, width: usize, value: u64) {
        let register = self.builder.alloc_bit(width, false);
        self.builder
            .emit(SIRInstruction::Imm(register, SIRValue::new(value)));
        self.store(slot, width, register);
    }

    fn store(&mut self, slot: SourceVarId, width: usize, value: RegisterId) {
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

    /// Suspend for the number of time units in `amount`, an unsigned
    /// two-state register of [`PROCESS_DELAY_WIDTH`] bits, and continue at
    /// a new resume point.
    pub fn delay(&mut self, amount: RegisterId) -> Result<(), ProcessKernelError> {
        let point = self.resume_blocks.len();
        if point > MAX_PROCESS_SUSPENSIONS {
            return Err(ProcessKernelError::TooManySuspensions);
        }
        self.store(self.slots.delay, PROCESS_DELAY_WIDTH, amount);
        self.store_constant(self.slots.resume, PROCESS_RESUME_WIDTH, point as u64);
        self.end(ProcessStatus::Delay);
        let resume = self.builder.new_block();
        self.resume_blocks.push(resume);
        self.builder.switch_to_block(resume);
        Ok(())
    }

    /// Suspend until `count` rising edges of the process clock `clock` (an
    /// index into the slots' clock list) have passed, and continue at a new
    /// resume point one period after the last of them. `count` is an
    /// unsigned two-state register of [`PROCESS_DELAY_WIDTH`] bits; a count
    /// of zero continues at once.
    pub fn wait_clock(&mut self, clock: u32, count: RegisterId) -> Result<(), ProcessKernelError> {
        if clock as usize >= self.slots.clocks.len() {
            return Err(ProcessKernelError::UnknownClock(clock));
        }
        let point = self.resume_blocks.len();
        if point > MAX_PROCESS_SUSPENSIONS {
            return Err(ProcessKernelError::TooManySuspensions);
        }
        self.store(self.slots.delay, PROCESS_DELAY_WIDTH, count);
        self.store_constant(self.slots.clock, PROCESS_CLOCK_WIDTH, u64::from(clock));
        self.store_constant(self.slots.resume, PROCESS_RESUME_WIDTH, point as u64);
        self.end(ProcessStatus::WaitClock);
        let resume = self.builder.new_block();
        self.resume_blocks.push(resume);
        self.builder.switch_to_block(resume);
        Ok(())
    }

    /// Suspend until a condition the kernel evaluates holds, and continue at
    /// a new resume point. The caller lowers the condition at that point
    /// and ends it with [`Self::wake_if`]; the runtime resumes the process
    /// there whenever the state may have changed. Sampling that an edge
    /// condition compares against is done by the caller before this call.
    pub fn begin_wait(&mut self) -> Result<(), ProcessKernelError> {
        let point = self.resume_blocks.len();
        if point > MAX_PROCESS_SUSPENSIONS {
            return Err(ProcessKernelError::TooManySuspensions);
        }
        self.store_constant(self.slots.resume, PROCESS_RESUME_WIDTH, point as u64);
        self.end(ProcessStatus::Wait);
        let resume = self.builder.new_block();
        self.resume_blocks.push(resume);
        self.builder.switch_to_block(resume);
        Ok(())
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
    pub fn build(mut self) -> ExecutionUnit<RegionedSourceAddr> {
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
        let byte = |builder: &mut SIRBuilder<RegionedSourceAddr>, lsb: usize| {
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

fn stable(var_id: SourceVarId) -> RegionedSourceAddr {
    RegionedSourceAddr {
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
