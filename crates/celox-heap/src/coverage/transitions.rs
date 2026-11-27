//! Transition bins (IEEE 1800-2023 19.5.2): bins of sequences of values of
//! a coverpoint, and the `default sequence` bin.
//!
//! A sequence is a list of steps, each a set of values with a repetition:
//! consecutive (`[* low:high]`, and one sample without one), goto
//! (`[-> low:high]`) or nonconsecutive (`[= low:high]`). It is matched as an
//! automaton over the samples of its coverpoint: an instance keeps, for each
//! coverpoint with transitions, the states that the samples so far left the
//! sequences in, each with the sample it started at. A sample takes each
//! state to the states it goes on to, starts each sequence whose first step
//! has the value, and counts a bin once for each sample at which one of its
//! sequences matched. Two threads of one sequence in one state go on alike,
//! so they are one: of their starts the latest is kept, which is the one
//! that ignored and illegal sequences take the fewest matches from. The
//! threads of a consecutive repetition take the same samples, so they are
//! kept as one run by when each entered it, and a sample costs the same
//! however long the repetition is. After a nonconsecutive repetition the
//! next step may start at any sample of its gap (19.5.2); the gap is a
//! thread with the continuations it started, and ends with the first match
//! of one of them.
//!
//! The values of a coverpoint do not change how a transition goes on: "an
//! ignored value has no effect on a transition that includes the value"
//! (19.5.5, and the same of illegal values, 19.5.6). A sequence of
//! `ignore_bins` or `illegal_bins` removes from the other bins each match
//! that cannot be made without matching it (19.5.5): a match of a `bins`
//! sequence does not count when an ignored or illegal sequence matched
//! within the samples it took. A bin none of whose sequences can be
//! matched without that is empty and takes no part in coverage (19.11).
//!
//! `name[]` gives a bin to each sequence of values that a transition of
//! the declaration stands for, which 19.5.2 names `name[4=>5=>6]`; such a
//! transition is of steps of a fixed number of samples, and a repetition
//! `[* low:high]` stands for its lengths. Those bins are made when the
//! instance is built, as those of an array of values are, so their number
//! is bounded by [`MAX_BINS_PER_ITEM`].

use std::collections::VecDeque;

use fxhash::{FxHashMap, FxHashSet};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

use super::descriptor::{BinKind, Constant, PointDescriptor, PointType, Repeat};
use super::shape::{
    BinClass, BinId, BinShape, Item, NameTag, Seen, ValueKey, operand_value, planes, show,
    show_sequence,
};
use super::value::{
    Exact, Interval, KeyValue, Matcher, Values, Wild, bias, low_ones, numeric, ones,
};
use super::{
    MAX_BINS_PER_ITEM, MAX_TRANSITION_STATES, MAX_TRANSITION_THREADS, MAX_TRANSITION_WORK, big,
};

/// No bin: the bin of a sequence that is an element of an array is found
/// from the values it matched.
const NONE: u32 = u32::MAX;

/// What makes the samples of a repetition count (19.5.2, 16.9.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum StepKind {
    /// `[*]`: the samples follow each other.
    Consecutive,
    /// `[->]`: samples of other values may come before each; the next step
    /// follows the last at once.
    Goto,
    /// `[=]`: as `Goto`, and the next step may come after more samples of
    /// other values.
    NonConsecutive,
}

/// A step of a sequence: a set of values with its repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Step {
    /// The set, by its number among the sets of the coverpoint.
    set: u32,
    low: u32,
    high: u32,
    kind: StepKind,
}

/// A step as a construction evaluated it, with the values of its set.
pub(crate) struct StepItems {
    items: Vec<Item>,
    pub low: u32,
    pub high: u32,
    kind: StepKind,
}

/// The step of the values `items` with the repetition `repeat`, whose
/// counts are evaluated from the constants and `inputs`.
pub(crate) fn step(
    items: Vec<Item>,
    repeat: &Repeat,
    inputs: &[Option<Constant>],
) -> Result<StepItems, String> {
    let (kind, low, high) = match repeat {
        Repeat::Once => {
            return Ok(StepItems {
                items,
                low: 1,
                high: 1,
                kind: StepKind::Consecutive,
            });
        }
        Repeat::Consecutive(low, high) => (StepKind::Consecutive, low, high),
        Repeat::Goto(low, high) => (StepKind::Goto, low, high),
        Repeat::NonConsecutive(low, high) => (StepKind::NonConsecutive, low, high),
    };
    let count = |operand| -> Result<u32, String> {
        let constant = operand_value(operand, inputs)?;
        if planes(constant).1.bits() != 0 {
            return Err(format!("has the repetition count {}", show(constant)));
        }
        let value = numeric(&planes(constant).0, constant.ty.width, constant.ty.signed);
        if value > num_bigint::BigInt::from(u32::MAX) {
            // Such a step has more states than any coverpoint may have.
            return Err(format!(
                "has the repetition count {value}, more than the {MAX_TRANSITION_STATES} states of \
                 the transitions of a coverpoint allow"
            ));
        }
        u32::try_from(&value)
            .ok()
            .filter(|&count| count >= 1)
            .ok_or_else(|| format!("has the repetition count {value}"))
    };
    let (low, high) = (count(low)?, count(high)?);
    if high < low {
        return Err(format!("has the repetition range {low}:{high}"));
    }
    Ok(StepItems {
        items,
        low,
        high,
        kind,
    })
}

/// Where a thread of a sequence is: at step `step`, after `count` samples
/// of its values, the last of the samples it took being one of them where
/// `occurred` is set, which is only noted for a goto repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
struct State {
    step: u32,
    count: u32,
    occurred: bool,
}

/// The state before the first sample of a sequence.
const START: State = State {
    step: 0,
    count: 0,
    occurred: false,
};

/// A sequence of a transition bin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Sequence {
    /// The bin a match counts in, by its index among the bins of the
    /// coverpoint, or [`NONE`] for an element of an array.
    bin: u32,
    kind: BinKind,
    steps: Vec<Step>,
    /// For an element of an array, the bin of each sequence of values.
    elements: Option<Vec<u32>>,
}

/// Where a thread of a sequence is.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
enum Node {
    /// In a step: for a nonconsecutive repetition, waiting for another
    /// sample of its values.
    At(State),
    /// The gap after the last sample of a nonconsecutive repetition of step
    /// `step`, which is not the last step: each sample may start the next
    /// step (`next` holds where those continuations are) as long as it is
    /// `open`, which a sample of the values of the repetition ends. The gap
    /// ends with the first match of a continuation.
    Gap {
        step: u32,
        open: bool,
        next: Vec<Node>,
    },
}

impl Node {
    /// The number of nodes it is, itself and its continuations.
    fn size(&self) -> usize {
        match self {
            Self::At(_) => 1,
            Self::Gap { next, .. } => 1 + next.iter().map(Self::size).sum::<usize>(),
        }
    }
}

impl Sequence {
    /// Call `next` with each node that a sample takes node `at` to, where
    /// `has` tells whether the value is in a set: whether the sequence
    /// matches with the sample.
    ///
    /// The next step follows a step once it has its least number of
    /// samples, for a goto repetition only with the sample after the last
    /// of them. Before each sample of a goto or nonconsecutive repetition
    /// samples of other values may come. After a sample of a nonconsecutive
    /// repetition that gives it enough, a gap starts in which "the
    /// transition following the nonconsecutive repetition may occur after
    /// any number of sample points so long as the repetition value does not
    /// occur again" (19.5.2); its first match ends it, which is why `b7` of
    /// 19.5.2 counts on the 13th sample only. A sequence matches with the
    /// sample that its last step has enough samples with, also where the
    /// step is a repetition of more samples, which go on to match again. A
    /// thread that has taken no sample of the first step is the thread that
    /// each sample starts, so it is no node.
    fn advance(&self, at: &Node, has: &dyn Fn(u32) -> bool, next: &mut Vec<Node>) -> bool {
        match at {
            Node::At(state) => self.advance_at(*state, has, next),
            Node::Gap {
                step,
                open,
                next: continuations,
            } => {
                let mut matched = false;
                let mut going = Vec::with_capacity(continuations.len() + 1);
                for continuation in continuations {
                    matched |= self.advance(continuation, has, &mut going);
                }
                if *open {
                    matched |= self.advance_at(Self::entered(*step + 1), has, &mut going);
                }
                if matched {
                    return true;
                }
                let open = *open && !has(self.steps[*step as usize].set);
                if open || !going.is_empty() {
                    going.sort_unstable();
                    going.dedup();
                    next.push(Node::Gap {
                        step: *step,
                        open,
                        next: going,
                    });
                }
                false
            }
        }
    }

    /// The state of a thread that is to take its first sample of step
    /// `step`.
    fn entered(step: u32) -> State {
        State { step, ..START }
    }

    /// [`Sequence::advance`] of a thread in a step.
    fn advance_at(&self, at: State, has: &dyn Fn(u32) -> bool, next: &mut Vec<Node>) -> bool {
        let mut matched = false;
        let index = at.step as usize;
        let step = &self.steps[index];
        let last = index + 1 == self.steps.len();
        let enough = at.count >= step.low;
        let follows = match step.kind {
            StepKind::Consecutive => true,
            StepKind::Goto => at.occurred,
            // The step after a nonconsecutive repetition follows its gap.
            StepKind::NonConsecutive => false,
        };
        if !last && enough && follows {
            matched |= self.advance_at(Self::entered(at.step + 1), has, next);
        }
        let inside = has(step.set);
        if inside && at.count < step.high {
            let count = at.count + 1;
            if last && count >= step.low {
                matched = true;
            }
            if !last || count < step.high {
                // Only a goto repetition looks at whether the last sample
                // was one of its values; a nonconsecutive one waits for
                // more only while it may have more.
                if step.kind != StepKind::NonConsecutive || count < step.high {
                    next.push(Node::At(State {
                        step: at.step,
                        count,
                        occurred: step.kind == StepKind::Goto,
                    }));
                }
            }
            if step.kind == StepKind::NonConsecutive && !last && count >= step.low {
                next.push(Node::Gap {
                    step: at.step,
                    open: true,
                    next: Vec::new(),
                });
            }
        } else if !inside && step.kind != StepKind::Consecutive {
            if at.count < step.high && (at.step, at.count) != (0, 0) {
                next.push(Node::At(State {
                    occurred: false,
                    ..at
                }));
            }
        }
        matched
    }

    /// The same as [`Sequence::advance_at`] where the sequence stands for
    /// the union of the matches of its paths (16.9.2), without the end of a
    /// gap after a nonconsecutive repetition at its first match: whether a
    /// sequence of values has a match is the same either way, as the first
    /// match of a gap took no more samples than a later one. The states are
    /// those of [`Sequence::advance_at`], with a nonconsecutive repetition
    /// that has enough samples both waiting for more and in its gap.
    fn advance_union(
        &self,
        at: State,
        has: &dyn Fn(u32) -> bool,
        next: &mut dyn FnMut(State),
    ) -> bool {
        let mut matched = false;
        let index = at.step as usize;
        let step = &self.steps[index];
        let last = index + 1 == self.steps.len();
        let enough = at.count >= step.low;
        if !last && enough && (step.kind != StepKind::Goto || at.occurred) {
            matched |= self.advance_union(Self::entered(at.step + 1), has, next);
        }
        let inside = has(step.set);
        if inside && at.count < step.high {
            let count = at.count + 1;
            if last && count >= step.low {
                matched = true;
            }
            if !last || count < step.high {
                next(State {
                    step: at.step,
                    count,
                    occurred: step.kind == StepKind::Goto,
                });
            }
        } else if !inside && step.kind != StepKind::Consecutive {
            let waiting = at.count < step.high;
            let trailing = step.kind == StepKind::NonConsecutive && !last && enough;
            if (waiting || trailing) && (at.step, at.count) != (0, 0) {
                next(State {
                    occurred: false,
                    ..at
                });
            }
        }
        matched
    }

    /// The most samples a match takes, where it is bounded.
    fn length(&self) -> Option<u64> {
        self.steps
            .iter()
            .map(|step| (step.kind == StepKind::Consecutive).then_some(u64::from(step.high)))
            .sum()
    }
}

/// The distinct values of the set of a step of an array, with their ranks:
/// the values in the order of their first appearance (19.5.2: 7, 8, 9, 10
/// for `[7:9],10`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Ranks {
    /// Disjoint ranges of keys in key order, each with the rank of its low
    /// key.
    ranges: Vec<(KeyValue, KeyValue, u64)>,
    /// The values with x or z bits, as the bits of their two planes, with
    /// their ranks.
    exact: Vec<(KeyValue, KeyValue, u64)>,
    count: u64,
}

/// The offset of key `key` from key `low`, which is not above it.
fn offset(key: &KeyValue, low: &KeyValue) -> u64 {
    match (key, low) {
        (KeyValue::Narrow(key), KeyValue::Narrow(low)) => key - low,
        _ => u64::try_from(key.key() - low.key()).unwrap_or(u64::MAX),
    }
}

impl Ranks {
    /// The rank of `value`, if it is a value of the set.
    fn rank(&self, value: &ValueKey) -> Option<u64> {
        match value {
            ValueKey::Known(key) => {
                let at = self.ranges.partition_point(|(_, high, _)| high < key);
                let (low, _, first) = self.ranges.get(at).filter(|(low, ..)| low <= key)?;
                Some(first + offset(key, low))
            }
            ValueKey::Unknown { value, mask } => self
                .exact
                .iter()
                .find(|(other, other_mask, _)| other == value && other_mask == mask)
                .map(|&(.., rank)| rank),
        }
    }
}

/// The distinct values of a set in the order of their first appearance,
/// each item with the rank of its first value, and their ranks.
struct Distinct {
    items: Vec<(u64, Item)>,
    ranks: Ranks,
}

impl Distinct {
    fn new(items: &[Item]) -> Result<Self, String> {
        let mut seen = Seen::default();
        let mut ordered = Vec::new();
        let mut count = BigUint::ZERO;
        let limit = BigUint::from(MAX_BINS_PER_ITEM);
        let mut push = |item: Item, ordered: &mut Vec<(BigUint, Item)>| {
            let size = item.count();
            ordered.push((count.clone(), item));
            count += size;
            count <= limit
        };
        for item in items {
            let fits = match item {
                Item::Range { low, high } => seen
                    .fresh(low, high)
                    .into_iter()
                    .all(|(low, high)| push(Item::Range { low, high }, &mut ordered)),
                Item::Exact { value, mask } => {
                    !seen.exact.insert((value.clone(), mask.clone()))
                        || push(item.clone(), &mut ordered)
                }
                Item::Wild { .. } => {
                    return Err(format!(
                        "has a wildcard value of more than {} ranges of values, which only a \
                         bin that is no array may have",
                        super::MAX_WILDCARD_EXPANSION
                    ));
                }
            };
            if !fits {
                return Err(format!("has more than {MAX_BINS_PER_ITEM} bins"));
            }
        }
        let count = u64::try_from(&count).unwrap_or(u64::MAX);
        let items: Vec<(u64, Item)> = ordered
            .into_iter()
            .map(|(first, item)| (u64::try_from(&first).unwrap_or(u64::MAX), item))
            .collect();
        let mut ranges = Vec::new();
        let mut exact = Vec::new();
        for (first, item) in &items {
            match item {
                Item::Range { low, high } => {
                    ranges.push((KeyValue::new(low), KeyValue::new(high), *first))
                }
                Item::Exact { value, mask } => {
                    exact.push((KeyValue::new(value), KeyValue::new(mask), *first))
                }
                Item::Wild { .. } => {}
            }
        }
        ranges.sort_unstable();
        Ok(Self {
            items,
            ranks: Ranks {
                ranges,
                exact,
                count,
            },
        })
    }

    /// The value of rank `rank`.
    fn value(&self, rank: u64) -> ValueKey {
        let at = self.items.partition_point(|(first, _)| *first <= rank) - 1;
        match &self.items[at] {
            (first, Item::Range { low, .. }) => {
                ValueKey::Known(KeyValue::new(&(low + (rank - first))))
            }
            (_, Item::Exact { value, mask }) => ValueKey::Unknown {
                value: KeyValue::new(value),
                mask: KeyValue::new(mask),
            },
            (_, Item::Wild { .. }) => unreachable!("no array has a wildcard pattern"),
        }
    }
}

/// The transitions of a coverpoint of an instance: its transition bins and
/// `default sequence` bins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TransitionShape {
    /// The value sets of the steps, as the bins of a matcher: a set never
    /// loses a value to an ignore or illegal bin (19.5.5, 19.5.6).
    sets: Matcher,
    sequences: Vec<Sequence>,
    /// By set, the sequences whose first step is of the set.
    starts: Vec<Vec<u32>>,
    /// By set, the ranks of its values where a step of an array has it.
    ranks: Vec<Option<Ranks>>,
    /// The `default sequence` bins among the bins of the coverpoint.
    defaults: Vec<u32>,
    /// How many of the last sampled values an instance keeps: as many as
    /// the longest match whose values name its bin or an illegal hit.
    history: u32,
    /// The values of the last steps of each transition bin, by the index of
    /// the bin among those of the coverpoint, which `binsof … intersect`
    /// looks at: "the binsof operator uses the last value of the
    /// transition" (19.6.1.1).
    pub last: Matcher,
}

/// How far the transitions of a coverpoint of an instance went: an
/// instance keeps it from sample to sample, also while the coverpoint is
/// not sampled (19.5.2 leaves this open: a sample that is not taken is no
/// sample of the transitions, so they stay where they were).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Progress {
    /// Ordered by sequence and node, one thread for each.
    pub(crate) threads: Vec<Thread>,
    /// The threads in consecutive repetitions outside any gap, one run for
    /// each sequence and step that has one, ordered by both.
    pub(crate) runs: Vec<Run>,
    /// The samples of the coverpoint: the number of the last.
    time: u64,
    /// The latest sample that a match of an ignored or illegal sequence
    /// started at, 0 if none.
    removed: u64,
    /// The last sampled values, oldest first, at most
    /// [`TransitionShape::history`].
    history: VecDeque<ValueKey>,
}

impl Progress {
    /// The bytes it holds beyond its own size.
    pub(crate) fn size(&self) -> usize {
        let nodes: usize = self.threads.iter().map(|thread| thread.node.size()).sum();
        let entries: usize = self
            .runs
            .iter()
            .map(|run| run.pending.len() + run.ready.len())
            .sum();
        nodes * std::mem::size_of::<Thread>()
            + entries * std::mem::size_of::<(u64, u64)>()
            + self.history.len() * std::mem::size_of::<ValueKey>()
    }

    /// Forget the threads and values: no sample can come any more.
    pub(crate) fn release(&mut self) {
        self.threads = Vec::new();
        self.runs = Vec::new();
        self.history = VecDeque::new();
    }
}

/// A thread of a sequence: where it is and the sample it started at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Thread {
    sequence: u32,
    node: Node,
    start: u64,
}

/// The threads of one sequence in one consecutive repetition (`[*
/// low:high]`, and a step without one) that is in no gap. They take the
/// same samples, so a thread is the sample it took the first sample of the
/// step with: its count is the samples since. A sample then costs the same
/// however many there are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Run {
    sequence: u32,
    step: u32,
    /// The threads with fewer than `low` samples of the step, oldest first:
    /// the sample each entered the step with and the one it started at.
    pub(crate) pending: VecDeque<(u64, u64)>,
    /// Of the threads with `low` to `high` samples, those that started
    /// later than every one that entered the step before them, oldest
    /// first: the first started latest of all, and there is one exactly
    /// when some thread has `low` to `high` samples.
    pub(crate) ready: VecDeque<(u64, u64)>,
}

impl Run {
    /// The count of a thread that entered with sample `entered` at sample
    /// `now`.
    fn count(now: u64, entered: u64) -> u64 {
        now - entered + 1
    }

    fn push_ready(&mut self, entry: (u64, u64)) {
        if self
            .ready
            .back()
            .is_some_and(|back| back.0 == entry.0 && back.1 >= entry.1)
        {
            return;
        }
        while self.ready.back().is_some_and(|back| back.1 <= entry.1) {
            self.ready.pop_back();
        }
        self.ready.push_back(entry);
    }

    /// A thread that started at sample `start` enters the step with
    /// sample `now`, which is of its values.
    fn enter(&mut self, now: u64, start: u64, low: u32) {
        if low <= 1 {
            self.push_ready((now, start));
        } else if let Some(back) = self.pending.back_mut()
            && back.0 == now
        {
            back.1 = back.1.max(start);
        } else {
            self.pending.push_back((now, start));
        }
    }

    /// Sample `now` is of the values of the step: every thread has one
    /// more sample of it, and those with more than `high` are none.
    fn take(&mut self, now: u64, low: u32, high: u32) {
        while let Some(&front) = self.pending.front() {
            if Self::count(now, front.0) < u64::from(low) {
                break;
            }
            self.pending.pop_front();
            self.push_ready(front);
        }
        self.drop_over(now, u64::from(high));
    }

    /// Forget the threads with more than `most` samples.
    fn drop_over(&mut self, now: u64, most: u64) {
        while self
            .ready
            .front()
            .is_some_and(|front| Self::count(now, front.0) > most)
        {
            self.ready.pop_front();
        }
    }

    /// The latest start of a thread with `low` to `high` samples.
    fn ready_start(&self) -> Option<u64> {
        self.ready.front().map(|front| front.1)
    }

    fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.ready.is_empty()
    }
}

/// What a sample of transitions uses, kept between samples so that a
/// sample allocates nothing where no gap after a nonconsecutive repetition
/// is under way.
#[derive(Clone, Debug, Default)]
pub(crate) struct TransitionScratch {
    /// The sets of the sampled value, ascending.
    sets: Vec<u32>,
    next: Vec<Thread>,
    /// The nodes that one thread goes on to.
    nodes: Vec<Node>,
    /// The threads that enter a consecutive repetition outside a gap: by
    /// sequence and step, the start.
    entering: Vec<(u32, u32, u64)>,
    /// The sequences that matched, with the sample each match started at.
    matched: Vec<(u32, u64)>,
    /// The bins that count the sample, each with the latest start.
    bins: Vec<(u32, u64)>,
}

/// What a sample of a coverpoint is counted with.
pub(crate) struct Counting<'a> {
    pub ty: &'a PointType,
    /// The bins of the coverpoint.
    pub bins: &'a [BinShape],
    pub holds: &'a dyn Fn(u32) -> bool,
    pub counts: &'a mut [u64],
    /// Each illegal bin that counts the sample, with the transition.
    pub illegal: &'a mut Vec<(u32, String)>,
    /// The `bins` bins that count the sample, where crosses need them.
    pub counted: Option<&'a mut Vec<u32>>,
}

impl TransitionShape {
    /// Count the value of the words `value` with the x and z bits `mask`
    /// in the transitions, from `progress` on. An error is more threads
    /// under way than [`MAX_TRANSITION_THREADS`].
    ///
    /// A bin counts a sample at which one of its sequences matches once
    /// (19.5.2: "at most once per sample"), unless its guard does not hold
    /// (19.5.1) or, for a `bins` bin, every match of the sample took the
    /// samples of a match of an ignored or illegal sequence (19.5.5,
    /// 19.5.6). A `default sequence` bin counts a sample that is not the
    /// first of the coverpoint where no sequence matches and no thread
    /// that was under way before goes on (19.5.2); ignored and illegal
    /// sequences are of the transition bins it looks at, as a value of an
    /// ignore or illegal bin is no `default` value.
    pub(crate) fn sample(
        &self,
        progress: &mut Progress,
        scratch: &mut TransitionScratch,
        value: &[u64],
        mask: &[u64],
        counting: Counting<'_>,
    ) -> Result<(), String> {
        let TransitionScratch {
            sets,
            next,
            nodes,
            entering,
            matched,
            bins,
        } = scratch;
        let ty = counting.ty;
        sets.clear();
        let keep = self.history != 0;
        let key = match &self.sets {
            Matcher::Narrow(matcher) => {
                let ones = low_ones(ty.width);
                let value = value.first().copied().unwrap_or(0) & ones;
                let mask = mask.first().copied().unwrap_or(0) & ones;
                if mask != 0 {
                    matcher.exact_hits(&value, &mask, |set| sets.push(set));
                    ValueKey::Unknown {
                        value: KeyValue::Narrow(value),
                        mask: KeyValue::Narrow(mask),
                    }
                } else {
                    let key = value ^ if ty.signed { 1 << (ty.width - 1) } else { 0 };
                    matcher.hits(&key, |set, _| sets.push(set));
                    ValueKey::Known(KeyValue::Narrow(key))
                }
            }
            Matcher::Wide(matcher) => {
                let ones = ones(ty.width);
                let value = big(value.iter().copied()) & &ones;
                let mask = big(mask.iter().copied()) & &ones;
                if mask.bits() != 0 {
                    matcher.exact_hits(&value, &mask, |set| sets.push(set));
                    ValueKey::Unknown {
                        value: KeyValue::new(&value),
                        mask: KeyValue::new(&mask),
                    }
                } else {
                    let key = value ^ bias(ty.width, ty.signed);
                    matcher.hits(&key, |set, _| sets.push(set));
                    ValueKey::Known(KeyValue::new(&key))
                }
            }
        };
        sets.sort_unstable();
        sets.dedup();
        progress.time += 1;
        let now = progress.time;
        if keep {
            if progress.history.len() == self.history as usize {
                progress.history.pop_front();
            }
            progress.history.push_back(key);
        }
        let has = |set: u32| sets.binary_search(&set).is_ok();
        next.clear();
        entering.clear();
        matched.clear();
        // A thread goes to a run where it enters a consecutive repetition
        // that is in no gap: such a thread has the one sample it entered
        // with.
        let route = |sequence: u32,
                     nodes: &mut Vec<Node>,
                     start: u64,
                     next: &mut Vec<Thread>,
                     entering: &mut Vec<(u32, u32, u64)>| {
            for node in nodes.drain(..) {
                match node {
                    Node::At(state)
                        if self.sequences[sequence as usize].steps[state.step as usize].kind
                            == StepKind::Consecutive =>
                    {
                        entering.push((sequence, state.step, start))
                    }
                    node => next.push(Thread {
                        sequence,
                        node,
                        start,
                    }),
                }
            }
        };
        // Whether a thread that was under way goes on.
        let mut going_on = false;
        for thread in &progress.threads {
            let sequence = &self.sequences[thread.sequence as usize];
            nodes.clear();
            if sequence.advance(&thread.node, &has, nodes) {
                matched.push((thread.sequence, thread.start));
            }
            going_on |= !nodes.is_empty();
            route(thread.sequence, nodes, thread.start, next, entering);
        }
        for run in &mut progress.runs {
            let sequence = &self.sequences[run.sequence as usize];
            let step = sequence.steps[run.step as usize];
            let last = run.step as usize + 1 == sequence.steps.len();
            // The threads with enough samples may go on to the next step
            // with this one.
            if !last && let Some(start) = run.ready_start() {
                nodes.clear();
                if sequence.advance_at(Sequence::entered(run.step + 1), &has, nodes) {
                    matched.push((run.sequence, start));
                }
                going_on |= !nodes.is_empty();
                route(run.sequence, nodes, start, next, entering);
            }
            if has(step.set) {
                run.take(now, step.low, step.high);
                if last {
                    if let Some(start) = run.ready_start() {
                        matched.push((run.sequence, start));
                    }
                    // Those with `high` samples have no more to take.
                    run.drop_over(now, u64::from(step.high) - 1);
                }
                going_on |= !run.is_empty();
            } else {
                run.pending.clear();
                run.ready.clear();
            }
        }
        progress.runs.retain(|run| !run.is_empty());
        for &set in sets.iter() {
            for &index in &self.starts[set as usize] {
                let sequence = &self.sequences[index as usize];
                nodes.clear();
                if sequence.advance_at(START, &has, nodes) {
                    matched.push((index, now));
                }
                route(index, nodes, now, next, entering);
            }
        }
        // The threads that entered a run with this sample, one for each
        // sequence and step with the latest start.
        entering.sort_unstable_by(|first, second| {
            (first.0, first.1)
                .cmp(&(second.0, second.1))
                .then(second.2.cmp(&first.2))
        });
        entering.dedup_by_key(|entry| (entry.0, entry.1));
        for &(sequence, step, start) in entering.iter() {
            let low = self.sequences[sequence as usize].steps[step as usize].low;
            let at = progress
                .runs
                .binary_search_by_key(&(sequence, step), |run| (run.sequence, run.step));
            let index = at.unwrap_or_else(|at| {
                progress.runs.insert(
                    at,
                    Run {
                        sequence,
                        step,
                        pending: VecDeque::new(),
                        ready: VecDeque::new(),
                    },
                );
                at
            });
            progress.runs[index].enter(now, start, low);
        }
        // The matches of ignored and illegal sequences come first: a match
        // that took their samples is removed (19.5.5, 19.5.6).
        for &(sequence, start) in matched.iter() {
            if self.sequences[sequence as usize].kind != BinKind::Bins {
                progress.removed = progress.removed.max(start);
            }
        }
        bins.clear();
        for &(index, start) in matched.iter() {
            let sequence = &self.sequences[index as usize];
            if sequence.kind == BinKind::Bins && start <= progress.removed {
                continue;
            }
            let bin = match &sequence.elements {
                None => sequence.bin,
                Some(elements) => self.element(sequence, elements, &progress.history),
            };
            if bin != NONE {
                bins.push((bin, start));
            }
        }
        // Each bin once, with the latest start: the shortest match.
        bins.sort_unstable_by(|first, second| first.0.cmp(&second.0).then(second.1.cmp(&first.1)));
        bins.dedup_by_key(|(bin, _)| *bin);
        let Counting {
            ty,
            bins: shapes,
            holds,
            counts,
            illegal,
            mut counted,
        } = counting;
        let window = |start: u64| {
            let length = (now + 1 - start) as usize;
            let skip = progress.history.len().saturating_sub(length);
            show_sequence(progress.history.iter().skip(skip), ty.width, ty.signed)
        };
        for &(bin, start) in bins.iter() {
            let shape = &shapes[bin as usize];
            if shape.empty || shape.iff.is_some_and(|bit| !holds(bit)) {
                continue;
            }
            counts[shape.counter as usize] += 1;
            match shape.kind {
                BinKind::Bins => {
                    if let Some(counted) = counted.as_deref_mut() {
                        counted.push(bin);
                    }
                }
                BinKind::Illegal => illegal.push((bin, format!("transition {}", window(start)))),
                BinKind::Ignore => {}
            }
        }
        if matched.is_empty() && !going_on && now > 1 {
            for &bin in &self.defaults {
                let shape = &shapes[bin as usize];
                if shape.iff.is_some_and(|bit| !holds(bit)) {
                    continue;
                }
                counts[shape.counter as usize] += 1;
                if shape.kind == BinKind::Illegal {
                    illegal.push((bin, format!("transition {}", window(now - 1))));
                }
            }
        }
        // The threads in order, one for each sequence and node.
        next.sort_unstable_by(|first, second| {
            (first.sequence, &first.node)
                .cmp(&(second.sequence, &second.node))
                .then(second.start.cmp(&first.start))
        });
        next.dedup_by(|later, kept| later.sequence == kept.sequence && later.node == kept.node);
        std::mem::swap(&mut progress.threads, next);
        let nodes: usize = progress
            .threads
            .iter()
            .map(|thread| thread.node.size())
            .sum();
        if nodes > MAX_TRANSITION_THREADS {
            return Err(format!(
                "the transitions of a coverpoint have more than {MAX_TRANSITION_THREADS} \
                 threads under way in gaps after nonconsecutive repetitions"
            ));
        }
        Ok(())
    }

    /// The bin of the element of an array that `sequence` matched with
    /// the last of the values `history`.
    fn element(&self, sequence: &Sequence, elements: &[u32], history: &VecDeque<ValueKey>) -> u32 {
        let length = sequence.steps.len();
        let Some(skip) = history.len().checked_sub(length) else {
            return NONE;
        };
        let mut index = 0u64;
        let mut stride = 1u64;
        for (step, value) in sequence.steps.iter().zip(history.iter().skip(skip)) {
            let Some(ranks) = &self.ranks[step.set as usize] else {
                return NONE;
            };
            let Some(rank) = ranks.rank(value) else {
                return NONE;
            };
            index += rank * stride;
            stride *= ranks.count;
        }
        elements.get(index as usize).copied().unwrap_or(NONE)
    }
}

/// What building the transitions of a coverpoint collects, declaration by
/// declaration.
#[derive(Default)]
pub(crate) struct TransitionBuilder {
    /// The items of each set of values.
    sets: Vec<Vec<Item>>,
    set_index: FxHashMap<Vec<Item>, u32>,
    sequences: Vec<Sequence>,
    distinct: Vec<Option<Distinct>>,
    defaults: Vec<u32>,
    history: u32,
    last: Values,
    /// The states of the sequences (see [`MAX_TRANSITION_STATES`]).
    states: u64,
}

impl TransitionBuilder {
    /// The number of the set of the values `items`.
    fn set(&mut self, items: Vec<Item>) -> u32 {
        if let Some(&set) = self.set_index.get(&items) {
            return set;
        }
        let set = self.sets.len() as u32;
        self.sets.push(items.clone());
        self.set_index.insert(items, set);
        self.distinct.push(None);
        set
    }

    /// Count `states` more states against the limit (see
    /// [`MAX_TRANSITION_STATES`]), before anything that grows with them is
    /// made.
    fn spend_states(&mut self, states: u64, point: &PointDescriptor) -> Result<(), String> {
        self.states = self.states.saturating_add(states);
        if self.states > MAX_TRANSITION_STATES as u64 {
            return Err(format!(
                "coverpoint `{}` has transitions of more than {MAX_TRANSITION_STATES} states",
                point.name
            ));
        }
        Ok(())
    }

    /// Add the values `items` to the last values of bin `bin`.
    fn last_values(&mut self, items: &[Item], bin: u32) {
        for item in items {
            match item.clone() {
                Item::Range { low, high } => self.last.intervals.push(Interval { low, high, bin }),
                Item::Exact { value, mask } => self.last.exact.push(Exact { value, mask, bin }),
                Item::Wild { value, care } => self.last.wild.push(Wild { value, care, bin }),
            }
        }
    }

    /// Add the sequence `steps` of the scalar transition bin `bin` of kind
    /// `kind` of `point`.
    pub(crate) fn sequence(
        &mut self,
        point: &PointDescriptor,
        bin: u32,
        kind: BinKind,
        steps: Vec<StepItems>,
    ) -> Result<(), String> {
        let states = steps
            .iter()
            .map(|step| {
                let factor = if step.kind == StepKind::Consecutive {
                    1
                } else {
                    2
                };
                (u64::from(step.high) + 1).saturating_mul(factor)
            })
            .fold(0, u64::saturating_add);
        self.spend_states(states, point)?;
        if let Some(last) = steps.last() {
            self.last_values(&last.items, bin);
        }
        let steps: Vec<Step> = steps
            .into_iter()
            .map(|step| Step {
                low: step.low,
                high: step.high,
                kind: step.kind,
                set: self.set(step.items),
            })
            .collect();
        let sequence = Sequence {
            bin,
            kind,
            steps,
            elements: None,
        };
        if kind == BinKind::Illegal {
            // An illegal hit shows the values of the match.
            let length = sequence.length().unwrap_or(0);
            self.history = self.history.max(u32::try_from(length).unwrap_or(u32::MAX));
        }
        self.sequences.push(sequence);
        Ok(())
    }

    /// Add the `default sequence` bin `bin` of kind `kind`.
    pub(crate) fn default_sequence(&mut self, bin: u32, kind: BinKind) {
        self.defaults.push(bin);
        if kind == BinKind::Illegal {
            self.history = self.history.max(2);
        }
    }

    /// Add the sequences of the array of transition bins of declaration
    /// `decl` of kind `kind` of `point`, which `subject` names: a bin for
    /// each distinct sequence of values that `sequences` stand for (19.5.2),
    /// made with `bin`. A repetition `[* low:high]` stands for each of its
    /// numbers of samples (`(3=>3=>3), (3=>3=>3=>3), (3=>3=>3=>3=>3)` for
    /// `3 [* 3:5]`). The bins are in the order of the transitions, and of
    /// the sequences of one in the order of the sequences of its lengths,
    /// the first step counting least, and then of the values of each step,
    /// the first step changing fastest, as the list of `sa` in 19.5.2
    /// (7=>11, 8=>11, 9=>11, 10=>11, 7=>12, ...).
    pub(crate) fn array(
        &mut self,
        point: &PointDescriptor,
        subject: &str,
        decl: u32,
        kind: BinKind,
        sequences: Vec<Vec<StepItems>>,
        bin: &mut dyn FnMut(BinId) -> Result<u32, String>,
    ) -> Result<(), String> {
        // The bin of each sequence of values the declaration has.
        let mut known: FxHashMap<Vec<ValueKey>, u32> = FxHashMap::default();
        let mut listed: u64 = 0;
        for steps in sequences {
            if steps.iter().any(|step| step.kind != StepKind::Consecutive) {
                return Err(format!(
                    "{subject} is a transition bin array with a goto or nonconsecutive \
                     repetition (IEEE 1800-2023 19.5.2)"
                ));
            }
            let sets: Vec<u32> = steps
                .iter()
                .map(|step| self.set(step.items.clone()))
                .collect();
            for &set in &sets {
                if self.distinct[set as usize].is_none() {
                    let distinct = Distinct::new(&self.sets[set as usize])
                        .map_err(|reason| format!("{subject} {reason}"))?;
                    self.distinct[set as usize] = Some(distinct);
                }
            }
            // The number of samples of each step, the first counting least.
            let mut lengths: Vec<u32> = steps.iter().map(|step| step.low).collect();
            'lengths: loop {
                // The limit is checked before the positions are listed.
                let length = lengths.iter().map(|&length| u64::from(length)).sum();
                self.spend_states(length, point)?;
                let positions: Vec<u32> = sets
                    .iter()
                    .zip(&lengths)
                    .flat_map(|(&set, &length)| std::iter::repeat_n(set, length as usize))
                    .collect();
                self.flat(
                    subject,
                    decl,
                    kind,
                    &positions,
                    &mut known,
                    &mut listed,
                    bin,
                )?;
                let mut at = 0;
                loop {
                    let Some(step) = steps.get(at) else {
                        break 'lengths;
                    };
                    if lengths[at] < step.high {
                        lengths[at] += 1;
                        break;
                    }
                    lengths[at] = step.low;
                    at += 1;
                }
            }
        }
        Ok(())
    }

    /// Add the sequence of the sets `positions`, one sample each, of the
    /// array of declaration `decl`, with a bin for each of its sequences of
    /// values that `known` does not have one for yet.
    #[allow(clippy::too_many_arguments)]
    fn flat(
        &mut self,
        subject: &str,
        decl: u32,
        kind: BinKind,
        positions: &[u32],
        known: &mut FxHashMap<Vec<ValueKey>, u32>,
        listed: &mut u64,
        bin: &mut dyn FnMut(BinId) -> Result<u32, String>,
    ) -> Result<(), String> {
        let distinct = |set: u32| {
            self.distinct[set as usize]
                .as_ref()
                .expect("the values of the set of an array are listed")
        };
        let mut count: u64 = 1;
        for &set in positions {
            count = count
                .checked_mul(distinct(set).ranks.count)
                .filter(|&count| listed.saturating_add(count) <= MAX_BINS_PER_ITEM as u64)
                .ok_or_else(|| {
                    format!("{subject} lists more than {MAX_BINS_PER_ITEM} sequences of values")
                })?;
        }
        *listed += count;
        if count == 0 {
            return Ok(());
        }
        let mut elements = Vec::with_capacity(count as usize);
        let mut values = Vec::with_capacity(positions.len());
        for index in 0..count {
            values.clear();
            let mut rest = index;
            for &set in positions {
                let distinct = distinct(set);
                values.push(distinct.value(rest % distinct.ranks.count));
                rest /= distinct.ranks.count;
            }
            let element = match known.get(&values) {
                Some(&element) => element,
                None => {
                    let element = bin(BinId {
                        decl,
                        tag: NameTag::Sequence(values.clone()),
                    })?;
                    known.insert(values.clone(), element);
                    let last = values.last().cloned().expect("a sequence has a step");
                    match last {
                        ValueKey::Known(key) => self.last.intervals.push(Interval {
                            low: key.key(),
                            high: key.key(),
                            bin: element,
                        }),
                        ValueKey::Unknown { value, mask } => self.last.exact.push(Exact {
                            value: value.key(),
                            mask: mask.key(),
                            bin: element,
                        }),
                    }
                    element
                }
            };
            elements.push(element);
        }
        let length = u32::try_from(positions.len()).unwrap_or(u32::MAX);
        self.history = self.history.max(length);
        self.sequences.push(Sequence {
            bin: NONE,
            kind,
            steps: positions
                .iter()
                .map(|&set| Step {
                    set,
                    low: 1,
                    high: 1,
                    kind: StepKind::Consecutive,
                })
                .collect(),
            elements: Some(elements),
        });
        Ok(())
    }

    /// The transitions of `point` once every declaration is added, where
    /// `bins` are the bins of the coverpoint: the transition bins that are
    /// left without a sequence are noted as empty (19.5.5, 19.5.6, 19.11).
    pub(crate) fn finish(
        self,
        point: &PointDescriptor,
        bins: &mut [BinShape],
    ) -> Result<Option<Box<TransitionShape>>, String> {
        if self.sequences.is_empty() && self.defaults.is_empty() {
            return Ok(None);
        }
        let ty = &point.ty;
        // A wildcard value matched as a pattern has values that are not
        // listed, so which matches an ignored or illegal sequence removes
        // could not be told when the bins are made (19.5.5, 19.11.1).
        let removing = self
            .sequences
            .iter()
            .any(|sequence| sequence.kind != BinKind::Bins);
        let patterns = self
            .sets
            .iter()
            .flatten()
            .any(|item| matches!(item, Item::Wild { .. }));
        if removing && patterns {
            return Err(format!(
                "coverpoint `{}` has an ignored or illegal transition and a wildcard transition \
                 value of more than {} ranges of values, which a coverpoint with ignored or \
                 illegal transitions may not have (IEEE 1800-2023 19.5.5, 19.5.6)",
                point.name,
                super::MAX_WILDCARD_EXPANSION
            ));
        }
        let mut values = Values::default();
        for (set, items) in self.sets.iter().enumerate() {
            for item in items.iter().cloned() {
                let bin = set as u32;
                match item {
                    Item::Range { low, high } => values.intervals.push(Interval { low, high, bin }),
                    Item::Exact { value, mask } => values.exact.push(Exact { value, mask, bin }),
                    Item::Wild { value, care } => values.wild.push(Wild { value, care, bin }),
                }
            }
        }
        let built = Matcher::build(
            ty.width,
            ty.signed,
            self.sets.len(),
            values,
            &|_| false,
            false,
        );
        let mut starts = vec![Vec::new(); self.sets.len()];
        for (index, sequence) in self.sequences.iter().enumerate() {
            starts[sequence.steps[0].set as usize].push(index as u32);
        }
        let ranks = self
            .distinct
            .into_iter()
            .map(|distinct| distinct.map(|distinct| distinct.ranks))
            .collect();
        let shape = TransitionShape {
            sets: built.matcher,
            sequences: self.sequences,
            starts,
            ranks,
            defaults: self.defaults,
            history: self.history,
            last: Matcher::build(
                ty.width,
                ty.signed,
                bins.len(),
                self.last,
                &|_| false,
                false,
            )
            .matcher,
        };
        Emptiness {
            shape: &shape,
            ty,
            holds: &built.holds,
            removers: (0..shape.sequences.len() as u32)
                .filter(|&index| shape.sequences[index as usize].kind != BinKind::Bins)
                .collect(),
            classes: None,
            work: 0,
            point: &point.name,
        }
        .mark(bins)?;
        Ok(Some(Box::new(shape)))
    }
}

/// Which transition bins are left with a sequence once the ignored and
/// illegal sequences are removed (19.5.5, 19.5.6).
struct Emptiness<'a> {
    shape: &'a TransitionShape,
    ty: &'a PointType,
    /// By set, whether it has a value.
    holds: &'a [bool],
    /// The sequences of ignore and illegal bins.
    removers: Vec<u32>,
    /// The sets that some value is in, each list once, where a value of no
    /// set is the empty list: lazily.
    classes: Option<Vec<Vec<u32>>>,
    work: u64,
    point: &'a str,
}

impl Emptiness<'_> {
    fn spend(&mut self, steps: usize) -> Result<(), String> {
        self.work = self.work.saturating_add(steps as u64);
        if self.work > MAX_TRANSITION_WORK {
            return Err(format!(
                "finding the sequences of the transition bins of coverpoint `{}` that its \
                 ignored and illegal sequences remove takes more than {MAX_TRANSITION_WORK} steps",
                self.point
            ));
        }
        Ok(())
    }

    /// Note which transition bins of `bins` have no sequence left.
    fn mark(mut self, bins: &mut [BinShape]) -> Result<(), String> {
        let mut kept = vec![false; bins.len()];
        let shape = self.shape;
        for (index, sequence) in shape.sequences.iter().enumerate() {
            if sequence.elements.is_some() || kept[sequence.bin as usize] {
                continue;
            }
            if sequence
                .steps
                .iter()
                .any(|step| !self.holds[step.set as usize])
            {
                continue;
            }
            kept[sequence.bin as usize] = sequence.kind != BinKind::Bins
                || self.removers.is_empty()
                || self.matchable(index as u32)?;
        }
        for (index, bin) in bins.iter_mut().enumerate() {
            if bin.class != BinClass::Transition {
                continue;
            }
            bin.empty = match &bin.id.tag {
                NameTag::Sequence(values) => {
                    bin.kind == BinKind::Bins
                        && !self.removers.is_empty()
                        && self.removed(values)?
                }
                _ => !kept[index],
            };
        }
        Ok(())
    }

    /// The sets of the value `value`, ascending.
    fn sets_of(&self, value: &ValueKey) -> Vec<u32> {
        let mut sets = Vec::new();
        match (&self.shape.sets, value) {
            (Matcher::Narrow(matcher), ValueKey::Known(KeyValue::Narrow(key))) => {
                matcher.hits(key, |set, _| sets.push(set))
            }
            (Matcher::Narrow(_), ValueKey::Known(KeyValue::Wide(_))) => {}
            (Matcher::Wide(matcher), ValueKey::Known(key)) => {
                matcher.hits(&key.key(), |set, _| sets.push(set))
            }
            (matcher, ValueKey::Unknown { value, mask }) => {
                matcher.holding_exact(&value.key(), &mask.key(), &mut |set| sets.push(set))
            }
        }
        sets.sort_unstable();
        sets.dedup();
        sets
    }

    /// Whether an ignored or illegal sequence matches within the sequence
    /// of values `values`, which is then removed.
    fn removed(&mut self, values: &[ValueKey]) -> Result<bool, String> {
        let mut threads: Vec<(u32, State)> = Vec::new();
        let mut next = Vec::new();
        for value in values {
            let sets = self.sets_of(value);
            let has = |set: u32| sets.binary_search(&set).is_ok();
            self.spend(threads.len() + self.removers.len())?;
            next.clear();
            let fresh = self.removers.iter().map(|&index| (index, START));
            for (index, state) in threads.iter().copied().chain(fresh) {
                let sequence = &self.shape.sequences[index as usize];
                if sequence.advance_union(state, &has, &mut |state| next.push((index, state))) {
                    return Ok(true);
                }
            }
            next.sort_unstable();
            next.dedup();
            std::mem::swap(&mut threads, &mut next);
        }
        Ok(false)
    }

    /// Whether sequence `index` of a `bins` bin has a match within which no
    /// ignored or illegal sequence matches: a search over the states of the
    /// sequence with those of the threads of ignored and illegal sequences
    /// that started with it, for each class of values the sets tell apart.
    fn matchable(&mut self, index: u32) -> Result<bool, String> {
        if self.classes.is_none() {
            self.classes = Some(self.shape.sets.classes(self.ty.width, self.ty.four_state));
        }
        let classes = self.classes.take().unwrap_or_default();
        let found = self.search(index, &classes);
        self.classes = Some(classes);
        found
    }

    fn search(&mut self, index: u32, classes: &[Vec<u32>]) -> Result<bool, String> {
        let shape = self.shape;
        let sequence = &shape.sequences[index as usize];
        type Node = (State, Vec<(u32, State)>);
        let mut seen: FxHashSet<Node> = FxHashSet::default();
        let mut queue: Vec<Node> = vec![(START, Vec::new())];
        let mut threads = Vec::new();
        let mut states = Vec::new();
        while let Some((state, removing)) = queue.pop() {
            for class in classes {
                self.spend(1 + removing.len() + self.removers.len())?;
                let has = |set: u32| class.binary_search(&set).is_ok();
                threads.clear();
                let mut removed = false;
                let fresh = self.removers.iter().map(|&remover| (remover, START));
                for (remover, at) in removing.iter().copied().chain(fresh) {
                    removed |=
                        shape.sequences[remover as usize]
                            .advance_union(at, &has, &mut |state| threads.push((remover, state)));
                }
                if removed {
                    continue;
                }
                threads.sort_unstable();
                threads.dedup();
                states.clear();
                if sequence.advance_union(state, &has, &mut |state| states.push(state)) {
                    return Ok(true);
                }
                for &state in &states {
                    let node = (state, threads.clone());
                    if seen.insert(node.clone()) {
                        queue.push(node);
                    }
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequence(steps: &[(u32, u32, u32, StepKind)]) -> Sequence {
        Sequence {
            bin: 0,
            kind: BinKind::Bins,
            steps: steps
                .iter()
                .map(|&(set, low, high, kind)| Step {
                    set,
                    low,
                    high,
                    kind,
                })
                .collect(),
            elements: None,
        }
    }

    /// The samples at which `sequence` matches the values `trace`, each the
    /// one set of the value or none, from the first on.
    fn matches(sequence: &Sequence, trace: &[Option<u32>]) -> Vec<usize> {
        let mut states: Vec<State> = Vec::new();
        let mut found = Vec::new();
        for (at, value) in trace.iter().enumerate() {
            let has = |set: u32| *value == Some(set);
            let mut next = Vec::new();
            let mut matched = false;
            for &state in states.iter().chain([START].iter()) {
                matched |= sequence.advance_union(state, &has, &mut |state| next.push(state));
            }
            if matched {
                found.push(at + 1);
            }
            next.sort_unstable();
            next.dedup();
            states = next;
        }
        found
    }

    #[test]
    fn repetitions_match_as_the_examples_of_19_5_2() {
        use StepKind::*;
        // `3 [-> 3]` is `...=>3...=>3...=>3`: set 0 is 3.
        let three = |values: &[u32]| -> Vec<Option<u32>> {
            values
                .iter()
                .map(|&value| (value == 3).then_some(0))
                .collect()
        };
        let goto = sequence(&[(0, 3, 3, Goto)]);
        assert_eq!(matches(&goto, &three(&[3, 1, 3, 2, 2, 3, 3])), [6, 7]);
        // `3 [= 2]` is `...=>3...=>3...`: it matches with the second 3.
        let nonconsecutive = sequence(&[(0, 2, 2, NonConsecutive)]);
        assert_eq!(matches(&nonconsecutive, &three(&[3, 1, 3, 1, 1])), [3]);
        // `3 [* 3:5]` is `(3=>3=>3), (3=>3=>3=>3), (3=>3=>3=>3=>3)`.
        let consecutive = sequence(&[(0, 3, 5, Consecutive)]);
        assert_eq!(matches(&consecutive, &three(&[3, 3, 3, 3, 1, 3])), [3, 4]);
    }
}
