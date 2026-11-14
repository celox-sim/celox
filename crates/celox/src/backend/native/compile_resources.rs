//! Choose compiler concurrency from currently available host resources.

const MAX_WORKERS: usize = 4;

/// Dispatch in priority order, but return results in stable task-index order.
/// Each worker owns at most one expanded process and its compilation state.
/// The first task that fails stops the others from taking more tasks, and its
/// error is returned.
pub(super) fn compile_tasks<T: Send, E: Send>(
    order: &[usize],
    workers: usize,
    compile: impl Fn(usize) -> Result<T, E> + Sync,
) -> std::thread::Result<Result<Vec<T>, E>> {
    compile_tasks_with(order, workers, || (), |(), index| compile(index))
}

/// [`compile_tasks`], with state of each worker: `state` makes it in the
/// worker, once, and the worker passes it to each task it compiles.
pub(super) fn compile_tasks_with<S, T: Send, E: Send>(
    order: &[usize],
    workers: usize,
    state: impl Fn() -> S + Sync,
    compile: impl Fn(&mut S, usize) -> Result<T, E> + Sync,
) -> std::thread::Result<Result<Vec<T>, E>> {
    use std::sync::atomic::Ordering;
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: scoped compile worker threads share the next-task counter and the stop flag the first failing worker sets"
    )]
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    let mut results = Vec::with_capacity(order.len());
    let mut failure = None;
    if workers <= 1 || order.len() <= 1 {
        let mut state = state();
        for &index in order {
            match compile(&mut state, index) {
                Ok(result) => results.push((index, result)),
                Err(error) => {
                    failure = Some((index, error));
                    break;
                }
            }
        }
    } else {
        #[expect(
            clippy::disallowed_types,
            reason = "cross-thread: scoped compile worker threads claim the next task index from this shared counter"
        )]
        let next = AtomicUsize::new(0);
        #[expect(
            clippy::disallowed_types,
            reason = "cross-thread: the first failing scoped compile worker sets it and the other workers stop taking tasks"
        )]
        let stopped = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let handles = (0..workers.min(order.len()))
                .map(|_| {
                    let next = &next;
                    let stopped = &stopped;
                    let state = &state;
                    let compile = &compile;
                    scope.spawn(move || {
                        let mut state = state();
                        let mut results = Vec::new();
                        let mut failure = None;
                        while !stopped.load(Ordering::Relaxed) {
                            let position = next.fetch_add(1, Ordering::Relaxed);
                            let Some(&index) = order.get(position) else {
                                break;
                            };
                            match compile(&mut state, index) {
                                Ok(result) => results.push((index, result)),
                                Err(error) => {
                                    stopped.store(true, Ordering::Relaxed);
                                    failure = Some((index, error));
                                    break;
                                }
                            }
                        }
                        (results, failure)
                    })
                })
                .collect::<Vec<_>>();
            let mut panic = None;
            for handle in handles {
                match handle.join() {
                    Ok((completed, failed)) => {
                        results.extend(completed);
                        // Of the tasks that failed before the others
                        // stopped, the first by index.
                        if let Some(failed) = failed
                            && failure.as_ref().is_none_or(|(index, _)| failed.0 < *index)
                        {
                            failure = Some(failed);
                        }
                    }
                    Err(error) => {
                        if panic.is_none() {
                            panic = Some(error);
                        }
                    }
                }
            }
            match panic {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })?;
    }
    if let Some((_, error)) = failure {
        return Ok(Err(error));
    }
    results.sort_unstable_by_key(|(index, _)| *index);
    Ok(Ok(results.into_iter().map(|(_, result)| result).collect()))
}

/// What compiling a unit of `blocks` blocks and `instructions` instructions
/// is estimated to cost, in bytes of compiler state: the order in which
/// tasks are dispatched, the largest first, and the memory each worker
/// needs, at least [`MIN_TASK_MEMORY`], both follow it.
pub(super) fn compile_cost(blocks: usize, instructions: usize) -> u64 {
    (blocks as u64)
        .saturating_mul(24 * 1024)
        .max((instructions as u64).saturating_mul(4096))
}

const MIN_TASK_MEMORY: u64 = 128 * 1024 * 1024;

/// The budget assumed when the host reports no available memory and each
/// worker needs state of its own: a copy that does not fit it twice leaves
/// one worker.
const ASSUMED_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

pub(super) fn workers(blocks: usize, instructions: usize, tasks: usize) -> usize {
    workers_with_state(blocks, instructions, tasks, 0)
}

/// [`workers`] for tasks that each worker compiles with state of its own,
/// such as a copy of the layout, of `state` bytes.
pub(super) fn workers_with_state(
    blocks: usize,
    instructions: usize,
    tasks: usize,
    state: u64,
) -> usize {
    choose_workers_with_state(
        blocks,
        instructions,
        tasks,
        std::thread::available_parallelism().map_or(1, usize::from),
        available_memory(),
        state,
    )
}

/// About the heap a copy of `table` takes. A hash table holds a control byte
/// besides each entry.
fn table_bytes<K, V>(table: &fxhash::FxHashMap<K, V>) -> u64 {
    (table.capacity() as u64).saturating_mul((size_of::<K>() + size_of::<V>() + 1) as u64)
}

/// About the heap a copy of `layout` takes: its tables of objects.
pub(super) fn layout_bytes<A>(layout: &celox_state_layout::MemoryLayout<A>) -> u64 {
    [
        table_bytes(&layout.offsets),
        table_bytes(&layout.widths),
        table_bytes(&layout.is_4states),
        table_bytes(&layout.unpacked_arrays),
        table_bytes(&layout.working_offsets),
        table_bytes(&layout.sparse_offsets),
        table_bytes(&layout.sparse_layouts),
        (layout.runtime_event_site_layouts.capacity() as u64)
            .saturating_mul(size_of::<celox_state_layout::RuntimeEventSiteLayout>() as u64),
    ]
    .into_iter()
    .fold(0, u64::saturating_add)
}

/// One primary circuit and a queue of smaller independent event functions.
/// Reserve the primary once, rather than charging its size to every worker.
pub(super) fn workers_with_primary(
    primary: (usize, usize),
    secondary: (usize, usize),
    tasks: usize,
) -> usize {
    choose_workers_with_primary(
        primary,
        secondary,
        tasks,
        std::thread::available_parallelism().map_or(1, usize::from),
        available_memory(),
    )
}

fn choose_workers_with_primary(
    primary: (usize, usize),
    secondary: (usize, usize),
    tasks: usize,
    cpus: usize,
    available: Option<u64>,
) -> usize {
    let maximum = MAX_WORKERS.min(cpus.max(1)).min(tasks.saturating_add(1));
    let Some(available) = available else {
        return choose_workers(
            primary.0.max(secondary.0),
            primary.1.max(secondary.1),
            tasks.saturating_add(1),
            cpus,
            None,
        );
    };
    let budget = available - available / 5;
    let remaining = budget.saturating_sub(estimated_memory(primary.0, primary.1));
    1 + (remaining / estimated_memory(secondary.0, secondary.1))
        .min(maximum.saturating_sub(1) as u64) as usize
}

fn estimated_memory(blocks: usize, instructions: usize) -> u64 {
    compile_cost(blocks, instructions).max(MIN_TASK_MEMORY)
}

fn choose_workers(
    blocks: usize,
    instructions: usize,
    tasks: usize,
    cpus: usize,
    available: Option<u64>,
) -> usize {
    choose_workers_with_state(blocks, instructions, tasks, cpus, available, 0)
}

fn choose_workers_with_state(
    blocks: usize,
    instructions: usize,
    tasks: usize,
    cpus: usize,
    available: Option<u64>,
    state: u64,
) -> usize {
    let maximum = MAX_WORKERS.min(cpus.max(1)).min(tasks.max(1));
    // Budget the additional SIR/MIR copies and allocator analyses: the shared
    // input is already deducted from available memory. Three overlapping
    // eight-hart functions peaked at 6.54 GiB for the entire process tree.
    // Account for straight-line designs too, not only CFG size.
    // These are conservative estimates, not reservations or memory limits.
    let per_function = estimated_memory(blocks, instructions).saturating_add(state);
    let budget = match available {
        // Leave room for the running simulator and unrelated host activity.
        Some(available) => available - available / 5,
        // Retain the conservative large-design fallback when the OS cannot
        // report a usable budget; small designs keep their usual parallelism
        // unless each worker needs state of its own, which is charged
        // against an assumed budget.
        None if blocks >= 65_536 => return 1,
        None if state == 0 => return maximum,
        None => ASSUMED_BUDGET,
    };
    (budget / per_function).min(maximum as u64).max(1) as usize
}

#[cfg(not(target_os = "linux"))]
fn available_memory() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn available_memory() -> Option<u64> {
    host_available_memory()
        .into_iter()
        .chain(address_space_available())
        .min()
}

#[cfg(target_os = "linux")]
fn address_space_available() -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // RLIMIT_AS covers allocator arenas, mapped code and thread stacks as well
    // as resident pages. Host/cgroup headroom can be much larger than it.
    if unsafe { libc::getrlimit(libc::RLIMIT_AS, &mut limit) } != 0
        || limit.rlim_cur == libc::RLIM_INFINITY
    {
        return None;
    }
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let virtual_bytes = status.lines().find_map(|line| {
        line.strip_prefix("VmSize:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    })?;
    // rlim_t has different widths across Linux ABIs.
    #[allow(clippy::useless_conversion)]
    let limit = u64::try_from(limit.rlim_cur).ok()?;
    Some(limit.saturating_sub(virtual_bytes))
}

#[cfg(target_os = "linux")]
fn host_available_memory() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let host = meminfo.lines().find_map(|line| {
        let value = line.strip_prefix("MemAvailable:")?;
        value
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    })?;
    let membership = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let (root, mount) = mounts.lines().find_map(|line| {
        let (fields, kind) = line.split_once(" - ")?;
        if kind.split_whitespace().next()? != "cgroup2" {
            return None;
        }
        let mut fields = fields.split_whitespace();
        Some((
            unescape_mount(fields.nth(3)?),
            unescape_mount(fields.next()?),
        ))
    })?;
    let relative = std::path::Path::new(path).strip_prefix(&root).ok()?;
    let mount = std::path::PathBuf::from(mount);
    let leaf = mount.join(relative);
    // A cgroup namespace may hide ancestors, but its mounted root still
    // contributes a limit. Walk every visible ancestor, not just the leaf.
    cgroup_available(host, &mount, &leaf)
}

#[cfg(target_os = "linux")]
fn unescape_mount(field: &str) -> String {
    field
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(target_os = "linux")]
fn cgroup_available(host: u64, mount: &std::path::Path, leaf: &std::path::Path) -> Option<u64> {
    if !leaf.is_dir() || !leaf.starts_with(mount) {
        return None;
    }
    let mut available = host;
    for directory in leaf.ancestors().take_while(|path| path.starts_with(mount)) {
        match std::fs::read_to_string(directory.join("memory.max")) {
            Ok(limit) if limit.trim() == "max" => {}
            Ok(limit) => {
                let limit = limit.trim().parse::<u64>().ok()?;
                let current = std::fs::read_to_string(directory.join("memory.current")).ok()?;
                let current = current.trim().parse::<u64>().ok()?;
                available = available.min(limit.saturating_sub(current));
            }
            // The root of the host cgroup hierarchy has no memory.max.
            Err(error) if directory == mount && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(available)
}

#[cfg(test)]
#[allow(
    clippy::disallowed_types,
    reason = "test-only: counters the compile worker threads update while the tests check them"
)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn per_worker_state_counts_against_the_memory_budget() {
        let small = (100, 1_000);
        // Small units alone take every CPU the tasks can use.
        assert_eq!(
            choose_workers_with_state(small.0, small.1, 100, 16, Some(8 * GIB), 0),
            4
        );
        // A large copy for each worker leaves room for two workers only.
        assert_eq!(
            choose_workers_with_state(small.0, small.1, 100, 16, Some(8 * GIB), 3 * GIB),
            2
        );
        // A copy that does not fit twice leaves one worker, which reuses one.
        assert_eq!(
            choose_workers_with_state(small.0, small.1, 100, 16, Some(8 * GIB), 6 * GIB),
            1
        );
        assert_eq!(
            choose_workers_with_state(small.0, small.1, 100, 16, Some(8 * GIB), 64 * GIB),
            1
        );
    }

    #[test]
    fn a_table_copy_is_estimated_from_its_capacity() {
        let mut table = fxhash::FxHashMap::<u64, usize>::default();
        assert_eq!(table_bytes(&table), 0);
        table.extend((0..100_000).map(|object| (object, 0)));
        assert!(table_bytes(&table) >= 100_000 * 17);
    }

    #[test]
    fn worker_state_is_made_once_per_worker_and_results_keep_task_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let order = [3, 0, 2, 1, 4];
        for workers in [1, 3] {
            let made = AtomicUsize::new(0);
            let results = compile_tasks_with(
                &order,
                workers,
                || {
                    made.fetch_add(1, Ordering::Relaxed);
                    Vec::new()
                },
                |seen: &mut Vec<usize>, index| {
                    seen.push(index);
                    Ok::<_, ()>((index, seen.len()))
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                results.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
                [0, 1, 2, 3, 4]
            );
            let made = made.load(Ordering::Relaxed);
            assert!((1..=workers).contains(&made));
            if workers == 1 {
                // One state takes the tasks in priority order.
                assert_eq!(
                    results.iter().map(|(_, seen)| *seen).collect::<Vec<_>>(),
                    [2, 4, 3, 1, 5]
                );
            }
        }
    }

    #[test]
    fn primary_worker_budget_keeps_capacity_for_smaller_functions() {
        let primary = (65_536, 1_000_000);
        let small = (100, 1_000);
        assert_eq!(
            choose_workers_with_primary(primary, small, 10, 16, Some(8 * GIB)),
            4
        );
        assert_eq!(
            choose_workers_with_primary(primary, primary, 10, 16, Some(8 * GIB)),
            1
        );
        assert_eq!(
            choose_workers_with_primary(primary, small, 10, 16, Some(4 * GIB)),
            1
        );
        assert_eq!(
            choose_workers_with_primary(primary, small, 0, 16, Some(8 * GIB)),
            1
        );
        assert_eq!(
            choose_workers_with_primary(primary, small, 10, 2, Some(8 * GIB)),
            2
        );
        assert_eq!(
            choose_workers_with_primary((120_000, 1), small, 10, 16, None),
            1
        );
        for available in [0, GIB, 4 * GIB, 5 * GIB, 8 * GIB, u64::MAX] {
            for secondary in [small, primary, (usize::MAX, usize::MAX)] {
                let workers =
                    choose_workers_with_primary(primary, secondary, 10, 16, Some(available));
                assert!((1..=MAX_WORKERS).contains(&workers));
                if workers > 1 {
                    let reserved = u128::from(estimated_memory(primary.0, primary.1))
                        + (workers - 1) as u128
                            * u128::from(estimated_memory(secondary.0, secondary.1));
                    assert!(reserved <= u128::from(available - available / 5));
                }
            }
        }
    }

    #[test]
    fn indexed_tasks_keep_results_ordered_and_bound_active_work() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let order = (0..16).rev().collect::<Vec<_>>();
        for workers in [1, 2, 4] {
            let active = AtomicUsize::new(0);
            let peak = AtomicUsize::new(0);
            let seen = AtomicUsize::new(0);
            let results = compile_tasks(&order, workers, |index| {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                seen.fetch_or(1 << index, Ordering::SeqCst);
                std::thread::yield_now();
                active.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, ()>(index * 3)
            })
            .unwrap()
            .unwrap();
            assert_eq!(results, (0..16).map(|index| index * 3).collect::<Vec<_>>());
            assert_eq!(seen.load(Ordering::SeqCst), (1 << 16) - 1);
            assert_eq!(active.load(Ordering::SeqCst), 0);
            assert!(peak.load(Ordering::SeqCst) <= workers);
        }
    }

    #[test]
    fn indexed_tasks_preserve_errors_and_return_worker_panics() {
        // One worker stops at the first task, in priority order, that fails.
        let failing = |index| {
            if index == 1 || index == 3 {
                Err(index)
            } else {
                Ok(index)
            }
        };
        assert_eq!(compile_tasks(&[3, 2, 1, 0], 1, failing).unwrap(), Err(3));
        // Several workers stop taking tasks once one fails, and return the
        // error of a task that failed.
        let error = compile_tasks(&[3, 2, 1, 0], 2, failing)
            .unwrap()
            .unwrap_err();
        assert!(error == 1 || error == 3, "{error}");
        assert!(
            compile_tasks(&[0, 1, 2], 2, |index| {
                assert_ne!(index, 1, "worker panic probe");
                Ok::<_, ()>(index)
            })
            .is_err()
        );
    }

    #[test]
    fn a_failed_task_stops_the_workers_from_taking_more() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for workers in [1, 2, 4] {
            let taken = AtomicUsize::new(0);
            let order = (0..1000).collect::<Vec<_>>();
            let result = compile_tasks(&order, workers, |index| {
                taken.fetch_add(1, Ordering::SeqCst);
                if index == 0 { Err("first") } else { Ok(index) }
            })
            .unwrap();
            assert_eq!(result, Err("first"));
            // One worker takes no task after the one that failed; several
            // stop well before they have taken every task.
            let taken = taken.load(Ordering::SeqCst);
            if workers == 1 {
                assert_eq!(taken, 1);
            }
            assert!(taken < 1000, "{workers} workers took every task");
        }
    }

    #[test]
    fn per_worker_state_is_charged_without_memory_information() {
        let small = (100, 1_000);
        // Without state, small units keep every CPU the tasks can use.
        assert_eq!(
            choose_workers_with_state(small.0, small.1, 100, 16, None, 0),
            4
        );
        // A small copy fits the assumed budget several times.
        assert_eq!(
            choose_workers_with_state(small.0, small.1, 100, 16, None, 64 * 1024 * 1024),
            4
        );
        // A copy that alone is large leaves one worker.
        assert_eq!(
            choose_workers_with_state(small.0, small.1, 100, 16, None, GIB),
            1
        );
    }

    #[test]
    fn dispatch_cost_counts_blocks_as_well_as_instructions() {
        // A unit of many small blocks costs more than one of more
        // instructions in few blocks.
        assert!(compile_cost(10_000, 20_000) > compile_cost(10, 50_000));
        assert!(compile_cost(10, 50_000) > compile_cost(10, 40_000));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finite_process_limit_caps_the_worker_memory_budget() {
        const CHILD: &str = "CELOX_TEST_ADDRESS_SPACE_LIMIT";
        // Test-process recursion marker, not a compiler diagnostic option.
        #[allow(clippy::disallowed_methods)]
        let is_child = std::env::var_os(CHILD).is_some();
        if is_child {
            let remaining = address_space_available().expect("finite process limit");
            assert!(remaining < GIB);
            let available = available_memory().expect("process budget is usable without cgroups");
            assert!(available <= GIB);
            assert_eq!(choose_workers(1, 200_000, 4, 16, Some(available)), 1);
            return;
        }
        use std::os::unix::process::CommandExt;
        let current = std::thread::current();
        let name = current.name().expect("named test thread");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", name, "--test-threads=1", "--nocapture"])
            .env(CHILD, "1");
        unsafe {
            child.pre_exec(|| {
                let cap: libc::rlim_t = 1024 * 1024 * 1024;
                let limit = libc::rlimit {
                    rlim_cur: cap,
                    rlim_max: cap,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let result = child.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
    }

    #[test]
    fn large_design_uses_host_memory_instead_of_always_serializing() {
        assert_eq!(choose_workers(120_000, 1_000_000, 5, 16, Some(38 * GIB)), 4);
        assert_eq!(choose_workers(120_000, 1_000_000, 5, 16, Some(5 * GIB)), 1);
        assert_eq!(choose_workers(120_000, 1_000_000, 5, 16, Some(10 * GIB)), 2);
        assert_eq!(choose_workers(120_000, 1_000_000, 5, 2, Some(11 * GIB)), 2);
    }

    #[test]
    fn respects_cpu_task_and_straight_line_memory_limits() {
        assert_eq!(choose_workers(1, 1, 5, 1, Some(38 * GIB)), 1);
        assert_eq!(choose_workers(1, 1, 2, 16, Some(38 * GIB)), 2);
        assert_eq!(choose_workers(1, 2_000_000, 5, 16, Some(5 * GIB)), 1);
        assert_eq!(choose_workers(1, 1, 5, 16, Some(0)), 1);
        assert_eq!(
            choose_workers(usize::MAX, usize::MAX, 5, 16, Some(u64::MAX)),
            1
        );
    }

    #[test]
    fn missing_memory_information_preserves_conservative_fallback() {
        assert_eq!(choose_workers(120_000, 1, 5, 16, None), 1);
        assert_eq!(choose_workers(100, 1000, 5, 16, None), 4);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn accounts_for_parent_usage_below_an_unlimited_child() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(parent.join("memory.max"), (8 * GIB).to_string()).unwrap();
        std::fs::write(parent.join("memory.current"), (3 * GIB).to_string()).unwrap();
        std::fs::write(child.join("memory.max"), "max\n").unwrap();
        assert_eq!(
            cgroup_available(38 * GIB, temp.path(), &child),
            Some(5 * GIB)
        );
        assert_eq!(cgroup_available(GIB, temp.path(), &child), Some(GIB));
        std::fs::write(parent.join("memory.current"), (9 * GIB).to_string()).unwrap();
        assert_eq!(cgroup_available(38 * GIB, temp.path(), &child), Some(0));
        std::fs::remove_file(parent.join("memory.current")).unwrap();
        assert_eq!(cgroup_available(38 * GIB, temp.path(), &child), None);
        assert_eq!(
            cgroup_available(GIB, temp.path(), &child.join("missing")),
            None
        );
    }
}
