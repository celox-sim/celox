//! Concurrent execution of lane-partitioned kernels.
//!
//! A kernel is a list of tasks. Every task belongs to one lane, and each lane
//! runs its tasks in list order on one thread: lane 0 on the calling thread,
//! the others on persistent worker threads. Before a task starts, its waits
//! require other lanes to have completed a number of their tasks in the
//! current execution. Completion counts are published with release stores and
//! observed with acquire loads, so a consumer sees every state write of the
//! producer tasks it waited for.
//!
//! Idle workers spin briefly for the next kernel and then park; dispatch
//! unparks only workers that announced they were going to sleep.

use std::cell::UnsafeCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// One task of a lane-partitioned kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneTaskSpec {
    /// Lane whose thread executes the task.
    pub lane: u32,
    /// `(lane, completed task count)` pairs that must hold before the task
    /// starts.
    pub waits: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaneScheduleError {
    #[error("task {task} names lane {lane}, but the schedule has {lanes} lanes")]
    Lane { task: usize, lane: u32, lanes: u32 },
    #[error("task {task} has an invalid wait on lane {lane} for {completed} tasks")]
    Wait {
        task: usize,
        lane: u32,
        completed: u32,
    },
}

/// Validated execution order of a lane-partitioned kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneSchedule {
    lanes: u32,
    tasks: Vec<LaneTaskSpec>,
    /// Global task indices of every lane, in execution order.
    lane_tasks: Vec<Vec<usize>>,
}

impl LaneSchedule {
    /// Validate `tasks`, listed in a valid sequential order.
    ///
    /// Every wait must refer to tasks of another lane that are listed before
    /// the waiting task, which makes concurrent execution deadlock free.
    pub fn new(lanes: u32, tasks: Vec<LaneTaskSpec>) -> Result<Self, LaneScheduleError> {
        let lanes = lanes.max(1);
        let mut lane_tasks = vec![Vec::new(); lanes as usize];
        for (task, spec) in tasks.iter().enumerate() {
            if spec.lane >= lanes {
                return Err(LaneScheduleError::Lane {
                    task,
                    lane: spec.lane,
                    lanes,
                });
            }
            for &(lane, completed) in &spec.waits {
                let listed_before = lane_tasks
                    .get(lane as usize)
                    .is_some_and(|listed: &Vec<usize>| completed as usize <= listed.len());
                if lane == spec.lane || completed == 0 || !listed_before {
                    return Err(LaneScheduleError::Wait {
                        task,
                        lane,
                        completed,
                    });
                }
            }
            lane_tasks[spec.lane as usize].push(task);
        }
        Ok(Self {
            lanes,
            tasks,
            lane_tasks,
        })
    }

    pub fn lanes(&self) -> u32 {
        self.lanes
    }

    pub fn tasks(&self) -> &[LaneTaskSpec] {
        &self.tasks
    }

    /// Run every task on the calling thread in list order.
    pub fn run_sequential(&self, runner: &dyn LaneTaskRunner) -> Result<(), LaneTaskFailure> {
        for task in 0..self.tasks.len() {
            let code = runner.run_task(task);
            if code != 0 {
                return Err(LaneTaskFailure { task, code });
            }
        }
        Ok(())
    }
}

/// Executes the tasks of one kernel.
///
/// `run_task` is called concurrently for tasks of different lanes, which the
/// schedule guarantees to have no conflicting state accesses.
pub trait LaneTaskRunner: Sync {
    /// Run one task and return its generated-code status (zero on success).
    fn run_task(&self, task: usize) -> i64;
}

/// The failure with the lowest task index of one kernel execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneTaskFailure {
    pub task: usize,
    pub code: i64,
}

/// Status reported for a task whose runner panicked.
pub const LANE_TASK_PANICKED: i64 = i64::MIN;

#[repr(align(128))]
struct Padded<T>(T);

#[derive(Clone, Copy)]
struct Job {
    schedule: *const LaneSchedule,
    runner: *const (dyn LaneTaskRunner + 'static),
}

/// Progress value of a lane that has finished every task of its epoch.
const LANE_DONE: u64 = u32::MAX as u64;

/// Largest epoch before the counters restart.
const MAX_EPOCH: u64 = u32::MAX as u64;

struct Shared {
    lanes: usize,
    job_epoch: Padded<AtomicU64>,
    /// Written by the dispatching thread before `job_epoch` is published and
    /// read by workers only after they acquire that epoch.
    job: UnsafeCell<Option<Job>>,
    /// Per lane: `epoch << 32 | completed tasks`, or `epoch << 32 |
    /// LANE_DONE` once the lane has finished the epoch. Each lane writes only
    /// its own counter, so completion never contends on a shared line.
    progress: Box<[Padded<AtomicU64>]>,
    abort: Padded<AtomicBool>,
    failure: Mutex<Option<LaneTaskFailure>>,
    sleeping: Box<[Padded<AtomicBool>]>,
    shutdown: AtomicBool,
    idle_spin: Duration,
}

// SAFETY: `job` is written only by the dispatching thread while every worker
// is idle (between the previous epoch's completion and the next epoch's
// release store) and read only after acquiring the new epoch. The pointers it
// holds stay valid until the dispatching call observes every worker finished.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

impl Shared {
    fn record_failure(&self, failure: LaneTaskFailure) {
        let mut slot = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none_or(|current| failure.task < current.task) {
            *slot = Some(failure);
        }
        self.abort.0.store(true, Ordering::Release);
    }

    fn execute_lane(&self, lane: usize, epoch: u64, job: Job) {
        // SAFETY: the dispatching thread keeps both referents alive until
        // every lane of this epoch has finished.
        let (schedule, runner) = unsafe { (&*job.schedule, &*job.runner) };
        let base = epoch << 32;
        let Some(tasks) = schedule.lane_tasks.get(lane) else {
            return;
        };
        for (sequence, &task) in tasks.iter().enumerate() {
            let mut aborted = self.abort.0.load(Ordering::Acquire);
            for &(other, completed) in &schedule.tasks[task].waits {
                if aborted {
                    break;
                }
                let target = base | u64::from(completed);
                let progress = &self.progress[other as usize].0;
                let mut spins = 0u32;
                while progress.load(Ordering::Acquire) < target {
                    if self.abort.0.load(Ordering::Acquire) {
                        aborted = true;
                        break;
                    }
                    wait_pause(&mut spins);
                }
            }
            if !aborted {
                let code = catch_unwind(AssertUnwindSafe(|| runner.run_task(task)))
                    .unwrap_or(LANE_TASK_PANICKED);
                if code != 0 {
                    self.record_failure(LaneTaskFailure { task, code });
                }
            }
            self.progress[lane]
                .0
                .store(base | (sequence as u64 + 1), Ordering::Release);
        }
    }
}

/// Back off inside a dependency wait: spin first, then yield so an
/// oversubscribed host still makes progress.
fn wait_pause(spins: &mut u32) {
    *spins = spins.wrapping_add(1);
    if *spins < 2048 {
        std::hint::spin_loop();
    } else {
        std::thread::yield_now();
    }
}

/// Persistent worker threads for one lane-partitioned program.
pub struct LanePool {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    epoch: u64,
}

impl std::fmt::Debug for LanePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanePool")
            .field("lanes", &self.shared.lanes)
            .finish_non_exhaustive()
    }
}

impl LanePool {
    /// Default time an idle worker keeps polling for the next kernel before
    /// it parks.
    pub const DEFAULT_IDLE_SPIN: Duration = Duration::from_micros(200);

    /// Start `lanes - 1` worker threads; lane 0 runs on the dispatching
    /// thread.
    pub fn new(lanes: u32) -> std::io::Result<Self> {
        Self::with_idle_spin(lanes, Self::DEFAULT_IDLE_SPIN)
    }

    pub fn with_idle_spin(lanes: u32, idle_spin: Duration) -> std::io::Result<Self> {
        let lanes = lanes.max(1) as usize;
        let shared = Arc::new(Shared {
            lanes,
            job_epoch: Padded(AtomicU64::new(0)),
            job: UnsafeCell::new(None),
            progress: (0..lanes).map(|_| Padded(AtomicU64::new(0))).collect(),
            abort: Padded(AtomicBool::new(false)),
            failure: Mutex::new(None),
            sleeping: (0..lanes).map(|_| Padded(AtomicBool::new(false))).collect(),
            shutdown: AtomicBool::new(false),
            idle_spin,
        });
        let mut workers = Vec::with_capacity(lanes - 1);
        for lane in 1..lanes {
            let worker_shared = Arc::clone(&shared);
            let spawned = std::thread::Builder::new()
                .name(format!("celox-lane-{lane}"))
                .spawn(move || worker(&worker_shared, lane));
            match spawned {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    let pool = Self {
                        shared,
                        workers,
                        epoch: 0,
                    };
                    drop(pool);
                    return Err(error);
                }
            }
        }
        Ok(Self {
            shared,
            workers,
            epoch: 0,
        })
    }

    pub fn lanes(&self) -> u32 {
        self.shared.lanes as u32
    }

    /// Execute one kernel and return its lowest-indexed task failure.
    ///
    /// After a failure, tasks that have not started are skipped; every lane
    /// still finishes before this call returns.
    pub fn run(
        &mut self,
        schedule: &LaneSchedule,
        runner: &dyn LaneTaskRunner,
    ) -> Result<(), LaneTaskFailure> {
        if schedule.lanes as usize > self.shared.lanes {
            return schedule.run_sequential(runner);
        }
        let shared = &*self.shared;
        // Progress words hold `epoch << 32 | count`, so the epoch must fit 32
        // bits. Every lane is idle between runs; clearing the words before
        // wrapping keeps no stale value above the new epoch's targets.
        if self.epoch >= MAX_EPOCH {
            for progress in shared.progress.iter() {
                progress.0.store(0, Ordering::Relaxed);
            }
            self.epoch = 0;
        }
        self.epoch += 1;
        let epoch = self.epoch;
        // SAFETY: lifetime erasure only; the referents outlive this call,
        // which waits for every worker below before returning.
        let runner: &'static dyn LaneTaskRunner = unsafe {
            std::mem::transmute::<&dyn LaneTaskRunner, &'static dyn LaneTaskRunner>(runner)
        };
        let job = Job {
            schedule: schedule as *const LaneSchedule,
            runner: runner as *const dyn LaneTaskRunner,
        };
        // SAFETY: every worker finished the previous epoch (see the wait at
        // the end of the previous call) and reads `job` only after acquiring
        // the epoch published below.
        unsafe {
            *shared.job.get() = Some(job);
        }
        shared.abort.0.store(false, Ordering::Relaxed);
        shared.job_epoch.0.store(epoch, Ordering::SeqCst);
        for (index, worker) in self.workers.iter().enumerate() {
            // Reading first keeps the flag's line shared while the worker
            // spins; only a sleeping worker needs the exchange.
            let sleeping = &shared.sleeping[index + 1].0;
            if sleeping.load(Ordering::SeqCst) && sleeping.swap(false, Ordering::SeqCst) {
                worker.thread().unpark();
            }
        }

        shared.execute_lane(0, epoch, job);

        let done = (epoch << 32) | LANE_DONE;
        for lane in 1..shared.lanes {
            let mut spins = 0u32;
            while shared.progress[lane].0.load(Ordering::Acquire) != done {
                wait_pause(&mut spins);
            }
        }
        // SAFETY: every worker is idle again.
        unsafe {
            *shared.job.get() = None;
        }
        let failure = shared
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        match failure {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }
}

fn worker(shared: &Shared, lane: usize) {
    let mut seen = 0u64;
    loop {
        let mut idle_since = None::<Instant>;
        let mut spins = 0u32;
        let epoch = loop {
            let epoch = shared.job_epoch.0.load(Ordering::Acquire);
            if epoch != seen || shared.shutdown.load(Ordering::Acquire) {
                break epoch;
            }
            spins = spins.wrapping_add(1);
            std::hint::spin_loop();
            if !spins.is_multiple_of(256) {
                continue;
            }
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() < shared.idle_spin {
                continue;
            }
            // Announce sleep, then re-check so a concurrent dispatch either
            // sees the flag (and unparks) or is seen here.
            shared.sleeping[lane].0.store(true, Ordering::SeqCst);
            let epoch = shared.job_epoch.0.load(Ordering::SeqCst);
            if epoch != seen || shared.shutdown.load(Ordering::SeqCst) {
                shared.sleeping[lane].0.store(false, Ordering::SeqCst);
                break epoch;
            }
            std::thread::park();
            shared.sleeping[lane].0.store(false, Ordering::SeqCst);
            idle_since = None;
        };
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        seen = epoch;
        // SAFETY: published before the epoch acquired above.
        let job = unsafe { *shared.job.get() };
        if let Some(job) = job {
            shared.execute_lane(lane, epoch, job);
        }
        // The dispatching thread may replace `job` once every lane is done.
        shared.progress[lane]
            .0
            .store((epoch << 32) | LANE_DONE, Ordering::Release);
    }
}

impl Drop for LanePool {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.job_epoch.0.fetch_add(1, Ordering::SeqCst);
        for worker in &self.workers {
            worker.thread().unpark();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI64;

    /// Each task appends its index to a log after checking that every
    /// task it waits for has already logged.
    struct Recorder {
        log: Mutex<Vec<usize>>,
        done: Vec<AtomicBool>,
        predecessors: Vec<Vec<usize>>,
        fail: Option<usize>,
        calls: AtomicI64,
    }

    impl LaneTaskRunner for Recorder {
        fn run_task(&self, task: usize) -> i64 {
            self.calls.fetch_add(1, Ordering::Relaxed);
            for &predecessor in &self.predecessors[task] {
                assert!(
                    self.done[predecessor].load(Ordering::Acquire),
                    "task {task} ran before {predecessor}"
                );
            }
            self.log.lock().unwrap().push(task);
            self.done[task].store(true, Ordering::Release);
            if self.fail == Some(task) { 7 } else { 0 }
        }
    }

    fn chain_schedule() -> (LaneSchedule, Vec<Vec<usize>>) {
        // lane 0: t0, t2; lane 1: t1 (waits for t0); lane 2: t3 (waits for t1, t2)
        let tasks = vec![
            LaneTaskSpec {
                lane: 0,
                waits: vec![],
            },
            LaneTaskSpec {
                lane: 1,
                waits: vec![(0, 1)],
            },
            LaneTaskSpec {
                lane: 0,
                waits: vec![],
            },
            LaneTaskSpec {
                lane: 2,
                waits: vec![(1, 1), (0, 2)],
            },
        ];
        let predecessors = vec![vec![], vec![0], vec![0], vec![1, 2]];
        (LaneSchedule::new(3, tasks).unwrap(), predecessors)
    }

    fn recorder(predecessors: Vec<Vec<usize>>, fail: Option<usize>) -> Recorder {
        Recorder {
            log: Mutex::new(Vec::new()),
            done: (0..predecessors.len())
                .map(|_| AtomicBool::new(false))
                .collect(),
            predecessors,
            fail,
            calls: AtomicI64::new(0),
        }
    }

    #[test]
    fn schedules_reject_forward_or_self_waits() {
        let forward = vec![LaneTaskSpec {
            lane: 0,
            waits: vec![(1, 1)],
        }];
        assert!(LaneSchedule::new(2, forward).is_err());
        let self_wait = vec![
            LaneTaskSpec {
                lane: 0,
                waits: vec![],
            },
            LaneTaskSpec {
                lane: 0,
                waits: vec![(0, 1)],
            },
        ];
        assert!(LaneSchedule::new(1, self_wait).is_err());
    }

    #[test]
    fn repeated_executions_honour_waits() {
        let (schedule, predecessors) = chain_schedule();
        let mut pool = LanePool::with_idle_spin(3, Duration::from_micros(0)).unwrap();
        for _ in 0..2000 {
            let runner = recorder(predecessors.clone(), None);
            pool.run(&schedule, &runner).unwrap();
            assert_eq!(runner.log.lock().unwrap().len(), 4);
        }
    }

    #[test]
    fn failures_skip_pending_work_and_report_the_first_task() {
        let (schedule, predecessors) = chain_schedule();
        let mut pool = LanePool::new(3).unwrap();
        let runner = recorder(predecessors.clone(), Some(0));
        assert_eq!(
            pool.run(&schedule, &runner),
            Err(LaneTaskFailure { task: 0, code: 7 })
        );
        // Dependents of the failed task never ran.
        assert!(!runner.done[1].load(Ordering::Acquire));
        assert!(!runner.done[3].load(Ordering::Acquire));
        // The pool remains usable after a failure.
        let runner = recorder(predecessors, None);
        pool.run(&schedule, &runner).unwrap();
        assert_eq!(runner.calls.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn epochs_restart_without_releasing_waits_early() {
        let (schedule, predecessors) = chain_schedule();
        let mut pool = LanePool::new(3).unwrap();
        pool.epoch = MAX_EPOCH - 2;
        for _ in 0..4 {
            let runner = recorder(predecessors.clone(), None);
            pool.run(&schedule, &runner).unwrap();
            assert_eq!(runner.calls.load(Ordering::Relaxed), 4);
        }
        assert!(pool.epoch < 4);
    }

    #[test]
    fn sequential_fallback_preserves_list_order() {
        let (schedule, predecessors) = chain_schedule();
        let runner = recorder(predecessors, None);
        schedule.run_sequential(&runner).unwrap();
        assert_eq!(*runner.log.lock().unwrap(), vec![0, 1, 2, 3]);
    }
}

/// Which implementation of a kernel to run next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelChoice {
    Parallel,
    Sequential,
}

/// How a runtime picks between a kernel's partitioned and sequential code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KernelSelection {
    /// Measure both implementations on the running simulation and keep the
    /// faster one.
    #[default]
    Measured,
    /// Always run the partitioned kernel.
    Parallel,
    /// Always run the sequential kernel.
    Sequential,
}

/// Picks a kernel implementation from measured execution times.
///
/// Calibration runs blocks of consecutive calls of one implementation so that
/// worker wake-up and cache warm-up are charged the way steady execution
/// would see them. The first calls of every block, which pay for moving the
/// state between cores, are discarded. After the configured number of
/// rounds, the implementation with the lower median call time is kept; the
/// partitioned kernel must win by a margin, since the sequential kernel needs
/// no worker threads.
///
/// A measured decision is checked again after a number of calls that doubles
/// with every check, so a choice made while the host was busy, or before the
/// simulation reached its steady state, does not stay wrong for the whole
/// run. Checks become rare quickly and cost a few dozen calls each.
#[derive(Debug, Clone)]
pub struct KernelSelector {
    state: SelectorState,
    /// Calls between the next decision and its check; zero for a fixed
    /// selection.
    recheck_interval: u64,
}

#[derive(Debug, Clone)]
enum SelectorState {
    Calibrating {
        round: u32,
        position: u32,
        samples: [Vec<u64>; 2],
    },
    Decided {
        choice: KernelChoice,
        /// Calls left until the decision is measured again.
        remaining: u64,
    },
}

impl KernelSelector {
    /// Calls per calibration block, including the discarded calls.
    const BLOCK: u32 = 16;
    /// Calls discarded at the start of every block.
    const DISCARDED: u32 = 4;
    /// Blocks per implementation.
    const ROUNDS: u32 = 3;
    /// The partitioned kernel's median must be below this fraction (in
    /// percent) of the sequential median.
    const PARALLEL_MARGIN_PERCENT: u64 = 95;
    /// Calls before the first check of a measured decision.
    const FIRST_RECHECK: u64 = 1 << 16;
    /// Longest interval between two checks.
    const MAX_RECHECK: u64 = 1 << 26;

    pub fn new(selection: KernelSelection) -> Self {
        let decided = |choice| SelectorState::Decided {
            choice,
            remaining: u64::MAX,
        };
        match selection {
            KernelSelection::Measured => Self {
                state: Self::calibrating(),
                recheck_interval: Self::FIRST_RECHECK,
            },
            KernelSelection::Parallel => Self {
                state: decided(KernelChoice::Parallel),
                recheck_interval: 0,
            },
            KernelSelection::Sequential => Self {
                state: decided(KernelChoice::Sequential),
                recheck_interval: 0,
            },
        }
    }

    fn calibrating() -> SelectorState {
        SelectorState::Calibrating {
            round: 0,
            position: 0,
            samples: [Vec::new(), Vec::new()],
        }
    }

    /// The implementation to run next, and whether its duration should be
    /// reported with [`Self::record`].
    pub fn next(&self) -> (KernelChoice, bool) {
        match &self.state {
            SelectorState::Decided { choice, .. } => (*choice, false),
            SelectorState::Calibrating { round, .. } => {
                // Alternate whole blocks: parallel, sequential, parallel, ...
                let choice = if round % 2 == 0 {
                    KernelChoice::Parallel
                } else {
                    KernelChoice::Sequential
                };
                (choice, true)
            }
        }
    }

    /// The current decision, or `None` while measuring.
    pub fn decided(&self) -> Option<KernelChoice> {
        match self.state {
            SelectorState::Decided { choice, .. } => Some(choice),
            SelectorState::Calibrating { .. } => None,
        }
    }

    /// Account for `calls` executions of the decided implementation; a
    /// measured decision is checked again when its interval has elapsed.
    pub fn advance(&mut self, calls: u64) {
        if self.recheck_interval == 0 {
            return;
        }
        if let SelectorState::Decided { remaining, .. } = &mut self.state {
            *remaining = remaining.saturating_sub(calls);
            if *remaining == 0 {
                self.state = Self::calibrating();
            }
        }
    }

    /// Report the duration of a call started after [`Self::next`] asked for
    /// timing.
    pub fn record(&mut self, choice: KernelChoice, nanos: u64) {
        let SelectorState::Calibrating {
            round,
            position,
            samples,
        } = &mut self.state
        else {
            return;
        };
        if *position >= Self::DISCARDED {
            samples[usize::from(choice == KernelChoice::Sequential)].push(nanos);
        }
        *position += 1;
        if *position < Self::BLOCK {
            return;
        }
        *position = 0;
        *round += 1;
        if *round < 2 * Self::ROUNDS {
            return;
        }
        let median = |values: &mut Vec<u64>| {
            values.sort_unstable();
            values.get(values.len() / 2).copied().unwrap_or(u64::MAX)
        };
        let [parallel, sequential] = samples;
        let parallel = median(parallel);
        let sequential = median(sequential);
        let choice = if parallel.saturating_mul(100)
            < sequential.saturating_mul(Self::PARALLEL_MARGIN_PERCENT)
        {
            KernelChoice::Parallel
        } else {
            KernelChoice::Sequential
        };
        tracing::debug!(
            "[parallel] kernel selection: parallel median {parallel} ns, sequential median {sequential} ns -> {choice:?}"
        );
        self.state = SelectorState::Decided {
            choice,
            remaining: self.recheck_interval,
        };
        self.recheck_interval = self
            .recheck_interval
            .saturating_mul(2)
            .min(Self::MAX_RECHECK);
    }
}

#[cfg(test)]
mod selector_tests {
    use super::*;

    fn calibrate(parallel: u64, sequential: u64) -> KernelChoice {
        let mut selector = KernelSelector::new(KernelSelection::Measured);
        while selector.decided().is_none() {
            let (choice, timed) = selector.next();
            assert!(timed);
            selector.record(
                choice,
                match choice {
                    KernelChoice::Parallel => parallel,
                    KernelChoice::Sequential => sequential,
                },
            );
        }
        selector.decided().unwrap()
    }

    #[test]
    fn measured_selection_keeps_the_faster_kernel() {
        assert_eq!(calibrate(500, 1000), KernelChoice::Parallel);
        assert_eq!(calibrate(1000, 500), KernelChoice::Sequential);
        // Parallel execution must win by a margin.
        assert_eq!(calibrate(980, 1000), KernelChoice::Sequential);
    }

    #[test]
    fn measured_decisions_are_checked_again_with_growing_intervals() {
        let mut selector = KernelSelector::new(KernelSelection::Measured);
        let mut calibrations = 0;
        let mut checks = Vec::new();
        let mut calls = 0u64;
        while checks.len() < 3 {
            let (choice, timed) = selector.next();
            if timed {
                let was_measuring = selector.decided().is_none();
                selector.record(
                    choice,
                    if choice == KernelChoice::Parallel {
                        1
                    } else {
                        2
                    },
                );
                if was_measuring && selector.decided().is_some() {
                    calibrations += 1;
                    checks.push(calls);
                }
            } else {
                selector.advance(1 << 12);
            }
            calls += 1;
        }
        assert_eq!(calibrations, 3);
        // The second interval is twice the first.
        let first = checks[1] - checks[0];
        let second = checks[2] - checks[1];
        assert!(second > first);
        assert_eq!(selector.decided(), Some(KernelChoice::Parallel));
    }

    #[test]
    fn fixed_selections_do_not_measure() {
        for (selection, expected) in [
            (KernelSelection::Parallel, KernelChoice::Parallel),
            (KernelSelection::Sequential, KernelChoice::Sequential),
        ] {
            let mut selector = KernelSelector::new(selection);
            assert_eq!(selector.next(), (expected, false));
            selector.advance(u64::MAX);
            assert_eq!(selector.next(), (expected, false));
        }
    }
}
