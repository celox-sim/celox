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

/// Process-local identity for diagnostic objects. Never derived from addresses
/// or topology, and never persisted in native images.
pub fn new_diagnostic_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("diagnostic identity exhausted");
    id.to_string()
}

// Use one metadata identity for the execution tracing gate.
static TRACE_GATE: tracing::callsite::DefaultCallsite =
    tracing::callsite::DefaultCallsite::new(&TRACE_METADATA);
static TRACE_METADATA: tracing::Metadata<'static> = tracing::Metadata::new(
    "parallel execution tracing",
    "celox::parallel",
    tracing::Level::TRACE,
    Some(file!()),
    Some(line!()),
    Some(module_path!()),
    tracing::field::FieldSet::new(&[], tracing::callsite::Identifier(&TRACE_GATE)),
    tracing::metadata::Kind::HINT,
);

fn execution_tracing_enabled() -> bool {
    if tracing::Level::TRACE > tracing::level_filters::STATIC_MAX_LEVEL {
        return false;
    }
    TRACE_GATE.register();
    // A callsite first registered on an untraced thread may cache "never".
    // Profiling belongs to the caller's dispatcher, not that cached default.
    tracing::dispatcher::get_default(|dispatch| dispatch.enabled(&TRACE_METADATA))
}

#[derive(Default)]
struct TaskTiming {
    waits: Vec<(u32, u32, u64, u64)>,
    start: u64,
    end: u64,
    code: Option<i64>,
}

struct ExecutionTrace {
    start: Instant,
    // Each cell has one writer. Read only after all workers have finished.
    tasks: Vec<Mutex<TaskTiming>>,
}

impl ExecutionTrace {
    fn nanos(&self) -> u64 {
        self.start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
    }
}

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
#[derive(Debug, Clone)]
pub struct LaneSchedule {
    lanes: u32,
    tasks: Vec<LaneTaskSpec>,
    /// Global task indices of every lane, in execution order.
    lane_tasks: Vec<Vec<usize>>,
    diagnostic_id: String,
}

impl PartialEq for LaneSchedule {
    fn eq(&self, other: &Self) -> bool {
        self.lanes == other.lanes
            && self.tasks == other.tasks
            && self.lane_tasks == other.lane_tasks
    }
}
impl Eq for LaneSchedule {}

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
            diagnostic_id: new_diagnostic_id(),
        })
    }

    /// Bind this runtime schedule to its compiler diagnostic identity.
    pub fn with_diagnostic_id(mut self, name: String) -> Self {
        self.diagnostic_id = name;
        self
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

/// A kernel failure. Non-fatal failures take precedence over combinational
/// fatal captures; within either class the lowest task index wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneTaskFailure {
    pub task: usize,
    pub code: i64,
}

/// Status reported for a task whose runner panicked.
pub const LANE_TASK_PANICKED: i64 = i64::MIN;

// repr requires a literal; keep the common layout boundary checked below.
#[repr(align(256))]
struct Padded<T>(T);
const _: () = assert!(std::mem::align_of::<Padded<u8>>() == celox_state_layout::LANE_SEGMENT_ALIGN);

#[derive(Clone, Copy)]
struct Job {
    schedule: *const LaneSchedule,
    runner: *const (dyn LaneTaskRunner + 'static),
    trace: *const ExecutionTrace,
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
        if slot.is_none_or(|current| {
            let priority = |failure: LaneTaskFailure| {
                (crate::comb_fatal_site(failure.code).is_some(), failure.task)
            };
            priority(failure) < priority(current)
        }) {
            *slot = Some(failure);
        }
        self.abort.0.store(true, Ordering::Release);
    }

    fn execute_lane(&self, lane: usize, epoch: u64, job: Job) {
        // SAFETY: the dispatching thread keeps both referents alive until
        // every lane of this epoch has finished.
        let (schedule, runner) = unsafe { (&*job.schedule, &*job.runner) };
        // SAFETY: the optional trace is owned by run until every lane finishes.
        let trace = unsafe { job.trace.as_ref() };
        let base = epoch << 32;
        let Some(tasks) = schedule.lane_tasks.get(lane) else {
            return;
        };
        for (sequence, &task) in tasks.iter().enumerate() {
            let mut timing = trace.map(|_| TaskTiming::default());
            let mut aborted = self.abort.0.load(Ordering::Acquire);
            for &(other, completed) in &schedule.tasks[task].waits {
                if aborted {
                    break;
                }
                let target = base | u64::from(completed);
                let progress = &self.progress[other as usize].0;
                let mut spins = 0u32;
                let wait_start = trace.map(ExecutionTrace::nanos);
                while progress.load(Ordering::Acquire) < target {
                    if self.abort.0.load(Ordering::Acquire) {
                        aborted = true;
                        break;
                    }
                    wait_pause(&mut spins);
                }
                if let (Some(trace), Some(timing), Some(start)) = (trace, &mut timing, wait_start) {
                    timing.waits.push((other, completed, start, trace.nanos()));
                }
            }
            // Failed lanes publish progress too, so a waiter can leave its
            // loop through the progress load without observing abort there.
            // Acquire abort again after the dependency's release publication.
            aborted |= self.abort.0.load(Ordering::Acquire);
            if !aborted {
                if let (Some(trace), Some(timing)) = (trace, &mut timing) {
                    timing.start = trace.nanos();
                }
                let code = catch_unwind(AssertUnwindSafe(|| runner.run_task(task)))
                    .unwrap_or(LANE_TASK_PANICKED);
                if let (Some(trace), Some(timing)) = (trace, &mut timing) {
                    timing.end = trace.nanos();
                    timing.code = Some(code);
                }
                if code != 0 {
                    self.record_failure(LaneTaskFailure { task, code });
                }
            }
            if let (Some(trace), Some(timing)) = (trace, timing) {
                *trace.tasks[task].lock().unwrap_or_else(|e| e.into_inner()) = timing;
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
    diagnostic_id: String,
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
                        diagnostic_id: new_diagnostic_id(),
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
            diagnostic_id: new_diagnostic_id(),
        })
    }

    pub fn lanes(&self) -> u32 {
        self.shared.lanes as u32
    }

    /// Execute one kernel and return its failure, preferring non-fatal errors
    /// to captured assertions before comparing task indices.
    ///
    /// After a failure, tasks that have not started are skipped; every lane
    /// still finishes before this call returns.
    pub fn run(
        &mut self,
        schedule: &LaneSchedule,
        runner: &dyn LaneTaskRunner,
    ) -> Result<(), LaneTaskFailure> {
        if schedule.lanes as usize > self.shared.lanes {
            tracing::trace!(target: "celox::parallel", event = "pool_fallback", reason = "insufficient_workers", requested = schedule.lanes, available = self.shared.lanes);
            return schedule.run_sequential(runner);
        }
        let trace = execution_tracing_enabled().then(|| ExecutionTrace {
            start: Instant::now(),
            tasks: (0..schedule.tasks.len())
                .map(|_| Mutex::new(TaskTiming::default()))
                .collect(),
        });
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
            trace: trace
                .as_ref()
                .map_or(std::ptr::null(), |trace| trace as *const ExecutionTrace),
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
        if let Some(trace) = &trace {
            let elapsed_ns = trace.nanos();
            let schedule_id = &schedule.diagnostic_id;
            let pool = &self.diagnostic_id;
            tracing::trace!(target: "celox::parallel", event = "dispatch", pool, epoch,
                schedule = schedule_id, lanes = schedule.lanes, elapsed_ns);
            // Emit after joining: no subscriber locks or I/O on worker threads.
            for (task, timing) in trace.tasks.iter().enumerate() {
                let timing = timing.lock().unwrap_or_else(|e| e.into_inner());
                tracing::trace!(target: "celox::parallel", event = "task_run", pool, epoch,
                    schedule = schedule_id, task, lane = schedule.tasks[task].lane,
                    start_ns = timing.start, end_ns = timing.end, skipped = timing.code.is_none(), code = timing.code.unwrap_or(0));
                for &(other, completed, start_ns, end_ns) in &timing.waits {
                    tracing::trace!(target: "celox::parallel", event = "task_wait", pool, epoch,
                        task, lane = schedule.tasks[task].lane, other, completed, start_ns, end_ns);
                }
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
        // Exercise both wait-loop aborts and dependency progress arriving
        // before the waiter checks abort. Recovery also resets every epoch.
        for _ in 0..2000 {
            let runner = recorder(predecessors.clone(), Some(0));
            assert_eq!(
                pool.run(&schedule, &runner),
                Err(LaneTaskFailure { task: 0, code: 7 })
            );
            // Dependents of the failed task never ran.
            assert!(!runner.done[1].load(Ordering::Acquire));
            assert!(!runner.done[3].load(Ordering::Acquire));
        }
        // The pool remains usable after a failure.
        let runner = recorder(predecessors, None);
        pool.run(&schedule, &runner).unwrap();
        assert_eq!(runner.calls.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn concurrent_loop_and_internal_failures_take_precedence_over_fatal_captures() {
        struct Runner {
            started: std::sync::Barrier,
            codes: [i64; 2],
        }
        impl LaneTaskRunner for Runner {
            fn run_task(&self, task: usize) -> i64 {
                self.started.wait();
                self.codes[task]
            }
        }
        let schedule = LaneSchedule::new(
            2,
            vec![
                LaneTaskSpec {
                    lane: 0,
                    waits: vec![],
                },
                LaneTaskSpec {
                    lane: 1,
                    waits: vec![],
                },
            ],
        )
        .unwrap();
        let mut pool = LanePool::new(2).unwrap();
        for code in [1, 2000, LANE_TASK_PANICKED] {
            for task in [0, 1] {
                let mut codes = [crate::comb_fatal_code(1); 2];
                codes[task] = code;
                let runner = Runner {
                    started: std::sync::Barrier::new(2),
                    codes,
                };
                assert_eq!(
                    pool.run(&schedule, &runner),
                    Err(LaneTaskFailure { task, code })
                );
            }
        }
        let runner = Runner {
            started: std::sync::Barrier::new(2),
            codes: [crate::comb_fatal_code(2), crate::comb_fatal_code(1)],
        };
        assert_eq!(
            pool.run(&schedule, &runner),
            Err(LaneTaskFailure {
                task: 0,
                code: crate::comb_fatal_code(2),
            })
        );
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

    #[test]
    fn diagnostic_ids_distinguish_equal_topologies_and_recreated_pools() {
        let (first, _) = chain_schedule();
        let (second, _) = chain_schedule();
        assert_ne!(first.diagnostic_id, second.diagnostic_id);
        assert_eq!(first, second);
        assert_eq!(first, second.clone().with_diagnostic_id("different".into()));
        let different = LaneSchedule::new(
            1,
            vec![LaneTaskSpec {
                lane: 0,
                waits: vec![],
            }],
        )
        .unwrap();
        assert_ne!(first, different);
        assert_eq!(first.clone().diagnostic_id, first.diagnostic_id);
        let first_pool = LanePool::new(1).unwrap();
        let id = first_pool.diagnostic_id.clone();
        drop(first_pool);
        let second_pool = LanePool::new(1).unwrap();
        assert_ne!(id, second_pool.diagnostic_id);
    }

    #[test]
    fn traced_failure_and_recovery_record_skips_and_real_waits() {
        use std::collections::BTreeMap;
        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<BTreeMap<String, String>>>>);
        struct Fields(BTreeMap<String, String>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.insert(field.name().into(), format!("{value:?}"));
            }
        }
        impl tracing::Subscriber for Capture {
            fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
                Some(tracing::metadata::LevelFilter::TRACE)
            }
            fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
                metadata.target() == "celox::parallel"
            }
            fn register_callsite(
                &self,
                _: &'static tracing::Metadata<'static>,
            ) -> tracing::subscriber::Interest {
                tracing::subscriber::Interest::sometimes()
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                let mut fields = Fields(BTreeMap::new());
                event.record(&mut fields);
                self.0.lock().unwrap().push(fields.0);
            }
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::with_default(Capture(events.clone()), || {
            use tracing::callsite::Callsite;
            // Reproduce a callsite cached by a thread without this subscriber.
            TRACE_GATE.register();
            TRACE_GATE.set_interest(tracing::subscriber::Interest::never());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let concurrent = std::thread::spawn({
                let barrier = barrier.clone();
                move || {
                    let (schedule, predecessors) = chain_schedule();
                    let mut pool = LanePool::new(3).unwrap();
                    barrier.wait();
                    for _ in 0..32 {
                        pool.run(&schedule, &recorder(predecessors.clone(), None))
                            .unwrap();
                    }
                }
            });
            barrier.wait();
            let (schedule, predecessors) = chain_schedule();
            let mut pool = LanePool::new(3).unwrap();
            assert!(
                pool.run(&schedule, &recorder(predecessors.clone(), Some(0)))
                    .is_err()
            );
            pool.run(&schedule, &recorder(predecessors, None)).unwrap();
            concurrent.join().unwrap();
        });
        let events = events.lock().unwrap();
        let rows = |name: &str| {
            let name = format!("{name:?}");
            events.iter().filter(move |e| e["event"] == name)
        };
        assert_eq!(rows("dispatch").count(), 2, "{events:?}");
        let runs = rows("task_run").collect::<Vec<_>>();
        assert_eq!(runs.len(), 8);
        assert!(runs.iter().any(|e| e["skipped"] == "true"));
        for event in runs.iter().filter(|e| e["epoch"] == "2") {
            assert_eq!(event["skipped"], "false");
            assert_eq!(event["code"], "0");
            assert!(
                event["end_ns"].parse::<u64>().unwrap()
                    >= event["start_ns"].parse::<u64>().unwrap()
            );
        }
        assert_eq!(rows("task_wait").filter(|e| e["epoch"] == "2").count(), 3);
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
#[derive(Debug)]
pub struct KernelSelector {
    diagnostic_id: String,
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

impl Clone for KernelSelector {
    fn clone(&self) -> Self {
        Self {
            diagnostic_id: new_diagnostic_id(),
            state: self.state.clone(),
            recheck_interval: self.recheck_interval,
        }
    }
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
                diagnostic_id: new_diagnostic_id(),
                state: Self::calibrating(),
                recheck_interval: Self::FIRST_RECHECK,
            },
            KernelSelection::Parallel => Self {
                diagnostic_id: new_diagnostic_id(),
                state: decided(KernelChoice::Parallel),
                recheck_interval: 0,
            },
            KernelSelection::Sequential => Self {
                diagnostic_id: new_diagnostic_id(),
                state: decided(KernelChoice::Sequential),
                recheck_interval: 0,
            },
        }
    }

    /// Process-local identity preserved across moves, but not across clones.
    pub fn diagnostic_id(&self) -> &str {
        &self.diagnostic_id
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
        tracing::debug!(target: "celox::parallel::selection", event = "selector_decision",
            selector = self.diagnostic_id.as_str(), parallel_ns = parallel, sequential_ns = sequential, choice = ?choice);
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
    fn selector_identity_survives_moves_without_reuse() {
        let selector = KernelSelector::new(KernelSelection::Measured);
        let id = selector.diagnostic_id().to_owned();
        let mut moved = Box::new(selector);
        assert_eq!(moved.diagnostic_id(), id);
        let cloned = moved.clone();
        assert_ne!(cloned.diagnostic_id(), id);
        assert_eq!(cloned.next(), moved.next());
        while moved.decided().is_none() {
            let (choice, _) = moved.next();
            moved.record(choice, 100);
        }
        assert_eq!(moved.diagnostic_id(), id);
        let decided_clone = moved.clone();
        assert_ne!(decided_clone.diagnostic_id(), id);
        assert_eq!(decided_clone.next(), moved.next());
        drop(moved);
        let replacement = KernelSelector::new(KernelSelection::Measured);
        assert_ne!(replacement.diagnostic_id(), id);
        assert_ne!(replacement.diagnostic_id(), cloned.diagnostic_id());
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
