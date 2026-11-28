//! Independent compile tasks on a few scoped threads, with results that do
//! not depend on how many threads run them.

/// The stack of a task thread: what the main thread of a process has, since
/// lowering and compiling recurse over statements and expressions as they
/// do on it.
const STACK_SIZE: usize = 8 * 1024 * 1024;

/// Run task `index` for each index of `order`, on up to `workers` threads.
/// The tasks are taken in the order of `order`, but the results come back by
/// task index. The first task that fails stops the others from taking more
/// tasks; of the tasks that failed, the error of the one with the lowest
/// index is returned. With one worker or one task, the tasks run in `order`
/// on the calling thread.
pub fn run_ordered<T: Send, E: Send>(
    order: &[usize],
    workers: usize,
    task: impl Fn(usize) -> Result<T, E> + Sync,
) -> std::thread::Result<Result<Vec<T>, E>> {
    run_ordered_with(order, workers, || (), |(), index| task(index))
}

/// [`run_ordered`], with state of each worker: `state` makes it in the
/// worker, once, and the worker passes it to each task it runs.
pub fn run_ordered_with<S, T: Send, E: Send>(
    order: &[usize],
    workers: usize,
    state: impl Fn() -> S + Sync,
    task: impl Fn(&mut S, usize) -> Result<T, E> + Sync,
) -> std::thread::Result<Result<Vec<T>, E>> {
    use std::sync::atomic::Ordering;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    let mut results = Vec::with_capacity(order.len());
    let mut failure = None;
    if workers <= 1 || order.len() <= 1 {
        let mut state = state();
        for &index in order {
            match task(&mut state, index) {
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
            reason = "cross-thread: scoped task threads claim the next task index from this shared counter"
        )]
        let next = AtomicUsize::new(0);
        #[expect(
            clippy::disallowed_types,
            reason = "cross-thread: the first failing scoped task thread sets it and the other threads stop taking tasks"
        )]
        let stopped = AtomicBool::new(false);
        // The tasks log as the calling thread does.
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        std::thread::scope(|scope| {
            let handles = (0..workers.min(order.len()))
                .map(|_| {
                    let next = &next;
                    let stopped = &stopped;
                    let state = &state;
                    let task = &task;
                    let dispatcher = dispatcher.clone();
                    std::thread::Builder::new()
                        .stack_size(STACK_SIZE)
                        .spawn_scoped(scope, move || {
                            let _logging = tracing::dispatcher::set_default(&dispatcher);
                            let mut state = state();
                            let mut results = Vec::new();
                            let mut failure = None;
                            while !stopped.load(Ordering::Relaxed) {
                                let position = next.fetch_add(1, Ordering::Relaxed);
                                let Some(&index) = order.get(position) else {
                                    break;
                                };
                                match task(&mut state, index) {
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
                        .expect("spawn a compile task thread")
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

/// [`run_ordered`] over tasks that each own their input: task `index` takes
/// `inputs[index]`.
pub fn run_owned<I: Send, T: Send, E: Send>(
    inputs: Vec<I>,
    order: &[usize],
    workers: usize,
    task: impl Fn(usize, I) -> Result<T, E> + Sync,
) -> std::thread::Result<Result<Vec<T>, E>> {
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: each scoped task thread takes the input of the task it claimed, once"
    )]
    let inputs = inputs
        .into_iter()
        .map(|input| std::sync::Mutex::new(Some(input)))
        .collect::<Vec<_>>();
    run_ordered(order, workers, |index| {
        let input = inputs[index]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("each task runs once");
        task(index, input)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_come_back_by_index_for_every_worker_count() {
        let order = [3, 0, 2, 1, 4];
        for workers in [1, 2, 8] {
            let results =
                run_ordered::<_, ()>(&order, workers, |index| Ok(index * 10)).expect("no panic");
            assert_eq!(results, Ok(vec![0, 10, 20, 30, 40]), "{workers} workers");
        }
    }

    #[test]
    fn the_failure_with_the_lowest_index_is_returned() {
        // With one worker the tasks run in `order` and stop at the first
        // failure taken, task 3.
        let order = [3, 1, 0, 2];
        let result = run_ordered(
            &order,
            1,
            |index| if index % 2 == 1 { Err(index) } else { Ok(()) },
        )
        .expect("no panic");
        assert_eq!(result, Err(3));
    }

    #[test]
    fn each_task_owns_its_input() {
        let inputs = vec![String::from("a"), String::from("b"), String::from("c")];
        let results = run_owned::<_, _, ()>(inputs, &[2, 1, 0], 2, |index, input| {
            Ok(format!("{index}{input}"))
        })
        .expect("no panic");
        assert_eq!(
            results,
            Ok(vec!["0a".to_string(), "1b".to_string(), "2c".to_string()])
        );
    }
}
