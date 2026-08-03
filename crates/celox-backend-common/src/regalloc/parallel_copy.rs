//! Target-independent scheduling of edge-local parallel copies.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

/// Writable physical location in a parallel assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CopyDestination<R> {
    Register(R),
    Stack(i32),
}

/// Readable physical location or constant in a parallel assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CopySource<R> {
    Register(R),
    Stack(i32),
    Immediate(u64),
}

/// One simultaneous assignment row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParallelCopy<R> {
    pub destination: CopyDestination<R>,
    pub source: CopySource<R>,
}

impl<R: PartialEq> ParallelCopy<R> {
    pub fn is_identity(&self) -> bool {
        matches!(
            (&self.destination, &self.source),
            (
                CopyDestination::Register(destination),
                CopySource::Register(source)
            ) if destination == source
        ) || matches!(
            (&self.destination, &self.source),
            (CopyDestination::Stack(destination), CopySource::Stack(source))
                if destination == source
        )
    }
}

/// One sequential operation implementing a parallel assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOperation<R> {
    Move {
        destination: CopyDestination<R>,
        source: CopySource<R>,
    },
    SwapRegisters {
        left: R,
        right: R,
    },
    SaveTemporary(CopyDestination<R>),
    RestoreTemporary(CopyDestination<R>),
}

/// Work counters retained for allocator diagnostics and regression tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyResolutionWork {
    pub effective_copies: usize,
    pub direct_moves: usize,
    pub register_swaps: usize,
    pub cycle_breaks: usize,
    pub temporary_cycle_breaks: usize,
    pub ready_queue_pops: usize,
    pub dependency_releases: usize,
}

/// Dependency-ordered realization of one parallel assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyResolution<R> {
    pub operations: Vec<CopyOperation<R>>,
    pub work: CopyResolutionWork,
}

/// Malformed copy rows or an inconsistent resolver state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyResolutionError<R> {
    pub rule: &'static str,
    pub destination: Option<CopyDestination<R>>,
    pub message: String,
}

impl<R> CopyResolutionError<R> {
    fn new(rule: &'static str, message: impl Into<String>) -> Self {
        Self {
            rule,
            destination: None,
            message: message.into(),
        }
    }

    fn at(mut self, destination: CopyDestination<R>) -> Self {
        self.destination = Some(destination);
        self
    }
}

impl<R: fmt::Debug> fmt::Display for CopyResolutionError<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "parallel copy [{}]", self.rule)?;
        if let Some(destination) = &self.destination {
            write!(formatter, " at {destination:?}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl<R: fmt::Debug> std::error::Error for CopyResolutionError<R> {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingSource<R> {
    Value(CopySource<R>),
    Temporary,
}

#[derive(Debug, Clone, Copy)]
struct PendingCopy<R> {
    destination: CopyDestination<R>,
    source: PendingSource<R>,
    pending: bool,
}

fn source_as_destination<R: Copy>(source: CopySource<R>) -> Option<CopyDestination<R>> {
    match source {
        CopySource::Register(register) => Some(CopyDestination::Register(register)),
        CopySource::Stack(slot) => Some(CopyDestination::Stack(slot)),
        CopySource::Immediate(_) => None,
    }
}

fn register_cycle<R>(
    copies: &[PendingCopy<R>],
    destination_index: &BTreeMap<CopyDestination<R>, usize>,
    start: usize,
) -> Option<(Vec<usize>, Vec<(R, R)>)>
where
    R: Copy + Ord,
{
    let CopyDestination::Register(start_register) = copies.get(start)?.destination else {
        return None;
    };
    let mut current = start_register;
    let mut members = Vec::new();
    let mut swaps = Vec::new();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return None;
        }
        let destination = CopyDestination::Register(current);
        let &index = destination_index.get(&destination)?;
        let copy = copies.get(index)?;
        if !copy.pending {
            return None;
        }
        let PendingSource::Value(CopySource::Register(source)) = copy.source else {
            return None;
        };
        members.push(index);
        if source == start_register {
            return (members.len() >= 2).then_some((members, swaps));
        }
        swaps.push((current, source));
        current = source;
    }
}

/// Schedule copies without clobbering any still-needed source.
///
/// Acyclic rows become direct moves, two-register cycles become one swap, and
/// longer or stack cycles use one target-provided temporary location.
pub fn resolve_parallel_copies<R>(
    rows: &[ParallelCopy<R>],
) -> Result<CopyResolution<R>, CopyResolutionError<R>>
where
    R: Copy + Ord + fmt::Debug,
{
    let mut destination_owner = BTreeSet::new();
    for row in rows {
        if !destination_owner.insert(row.destination) {
            return Err(CopyResolutionError::new(
                "PARALLEL_COPY.NON_UNIQUE_DESTINATION",
                "parallel assignment writes one destination more than once",
            )
            .at(row.destination));
        }
    }

    let mut copies = rows
        .iter()
        .filter(|row| !row.is_identity())
        .map(|row| PendingCopy {
            destination: row.destination,
            source: PendingSource::Value(row.source),
            pending: true,
        })
        .collect::<Vec<_>>();
    let mut work = CopyResolutionWork {
        effective_copies: copies.len(),
        ..CopyResolutionWork::default()
    };
    if copies.is_empty() {
        return Ok(CopyResolution {
            operations: Vec::new(),
            work,
        });
    }

    let destination_index = copies
        .iter()
        .enumerate()
        .map(|(index, copy)| (copy.destination, index))
        .collect::<BTreeMap<_, _>>();
    let mut readers = BTreeMap::<CopyDestination<R>, BTreeSet<usize>>::new();
    for (index, copy) in copies.iter().enumerate() {
        let PendingSource::Value(source) = copy.source else {
            continue;
        };
        if let Some(location) = source_as_destination(source) {
            readers.entry(location).or_default().insert(index);
        }
    }

    let mut ready = VecDeque::new();
    let mut queued = vec![false; copies.len()];
    for (index, copy) in copies.iter().enumerate() {
        if !readers.contains_key(&copy.destination) {
            ready.push_back(index);
            queued[index] = true;
        }
    }

    let mut operations = Vec::with_capacity(copies.len());
    let mut remaining = copies.len();
    let mut temporary_live = false;
    let mut cycle_search_start = 0usize;
    while remaining != 0 {
        while let Some(index) = ready.pop_front() {
            queued[index] = false;
            if !copies[index].pending {
                continue;
            }
            work.ready_queue_pops += 1;
            match copies[index].source {
                PendingSource::Value(source) => {
                    operations.push(CopyOperation::Move {
                        destination: copies[index].destination,
                        source,
                    });
                    work.direct_moves += 1;
                    if let Some(location) = source_as_destination(source) {
                        let released = readers.get_mut(&location).is_some_and(|location_readers| {
                            location_readers.remove(&index);
                            location_readers.is_empty()
                        });
                        if released {
                            readers.remove(&location);
                            work.dependency_releases += 1;
                            if let Some(&writer) = destination_index.get(&location)
                                && copies[writer].pending
                                && !queued[writer]
                            {
                                ready.push_back(writer);
                                queued[writer] = true;
                            }
                        }
                    }
                }
                PendingSource::Temporary => {
                    if !temporary_live {
                        return Err(CopyResolutionError::new(
                            "PARALLEL_COPY.TEMPORARY_STATE",
                            "resolver attempted to restore an inactive temporary",
                        )
                        .at(copies[index].destination));
                    }
                    operations.push(CopyOperation::RestoreTemporary(copies[index].destination));
                    temporary_live = false;
                }
            }
            copies[index].pending = false;
            remaining -= 1;
        }

        if remaining == 0 {
            break;
        }
        if temporary_live {
            return Err(CopyResolutionError::new(
                "PARALLEL_COPY.TEMPORARY_STATE",
                "resolver stalled while a temporary was live",
            ));
        }
        let Some(cycle) = copies
            .iter()
            .enumerate()
            .skip(cycle_search_start)
            .find_map(|(index, copy)| copy.pending.then_some(index))
        else {
            return Err(CopyResolutionError::new(
                "PARALLEL_COPY.RESOLVER_STATE",
                "nonzero pending count has no pending row",
            ));
        };
        cycle_search_start = cycle + 1;

        if let Some((members, swaps)) = register_cycle(&copies, &destination_index, cycle)
            && members.len() == 2
        {
            for (left, right) in swaps.iter().copied() {
                operations.push(CopyOperation::SwapRegisters { left, right });
            }
            for &member in &members {
                readers.remove(&copies[member].destination);
                copies[member].pending = false;
                queued[member] = false;
            }
            remaining -= members.len();
            work.register_swaps += swaps.len();
            work.cycle_breaks += 1;
            work.dependency_releases += members.len();
            continue;
        }

        let saved = copies[cycle].destination;
        let saved_readers = readers.remove(&saved).unwrap_or_default();
        if saved_readers.len() != 1 {
            return Err(CopyResolutionError::new(
                "PARALLEL_COPY.CYCLE_SHAPE",
                format!("stalled cycle location has {} readers", saved_readers.len()),
            )
            .at(saved));
        }
        let reader = *saved_readers
            .iter()
            .next()
            .expect("one cycle reader was checked above");
        copies[reader].source = PendingSource::Temporary;
        operations.push(CopyOperation::SaveTemporary(saved));
        work.cycle_breaks += 1;
        work.temporary_cycle_breaks += 1;
        work.dependency_releases += 1;
        temporary_live = true;
        if !queued[cycle] {
            ready.push_back(cycle);
            queued[cycle] = true;
        }
    }

    if temporary_live {
        return Err(CopyResolutionError::new(
            "PARALLEL_COPY.TEMPORARY_STATE",
            "resolver left its temporary live",
        ));
    }
    Ok(CopyResolution { operations, work })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register_copy(destination: u8, source: u8) -> ParallelCopy<u8> {
        ParallelCopy {
            destination: CopyDestination::Register(destination),
            source: CopySource::Register(source),
        }
    }

    #[test]
    fn orders_an_acyclic_chain_backwards() {
        let result = resolve_parallel_copies(&[register_copy(0, 1), register_copy(2, 0)]).unwrap();
        assert_eq!(
            result.operations,
            vec![
                CopyOperation::Move {
                    destination: CopyDestination::Register(2),
                    source: CopySource::Register(0),
                },
                CopyOperation::Move {
                    destination: CopyDestination::Register(0),
                    source: CopySource::Register(1),
                },
            ]
        );
    }

    #[test]
    fn swaps_a_two_register_cycle() {
        let result = resolve_parallel_copies(&[register_copy(0, 1), register_copy(1, 0)]).unwrap();
        assert_eq!(result.work.register_swaps, 1);
        assert!(matches!(
            result.operations.as_slice(),
            [CopyOperation::SwapRegisters { .. }]
        ));
    }

    #[test]
    fn breaks_a_long_cycle_with_one_temporary() {
        let result = resolve_parallel_copies(&[
            register_copy(0, 1),
            register_copy(1, 2),
            register_copy(2, 0),
        ])
        .unwrap();
        assert_eq!(result.work.temporary_cycle_breaks, 1);
        assert!(matches!(
            result.operations.first(),
            Some(CopyOperation::SaveTemporary(_))
        ));
        assert!(matches!(
            result.operations.last(),
            Some(CopyOperation::RestoreTemporary(_))
        ));
    }

    #[test]
    fn rejects_duplicate_destinations_even_when_one_row_is_identity() {
        let error =
            resolve_parallel_copies(&[register_copy(0, 0), register_copy(0, 1)]).unwrap_err();
        assert_eq!(error.rule, "PARALLEL_COPY.NON_UNIQUE_DESTINATION");
    }
}
