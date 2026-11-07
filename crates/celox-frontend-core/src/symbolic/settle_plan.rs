//! A readable dump of a partitioned settle's plan, with a lint of its
//! dependencies ([`format()`]).
//!
//! Every partition is listed with its cost, the bits it reads, what it
//! compares after it ran and whom each compared chunk marks. The summary
//! first counts what makes partitions run: reader edges, backward edges
//! (a reader earlier in the order, or the partition itself, runs in the
//! next settle pass, not this one) and readers marked for bits they do not read, which must be none.
use std::fmt::Write as _;

use celox_slt::scheduler::SettlePartition;

use crate::HashMap;

/// The plan of `partitions`, naming objects with `name`.
pub fn format<A: Copy + Eq + std::hash::Hash>(
    partitions: &[SettlePartition<A>],
    name: impl Fn(&A) -> String,
) -> String {
    let count = partitions.len();
    let reads: Vec<HashMap<A, &Option<Vec<(usize, usize)>>>> = partitions
        .iter()
        .map(|partition| {
            partition
                .plan
                .read_bits
                .iter()
                .map(|(address, bits)| (*address, bits))
                .collect()
        })
        .collect();
    // Whether `reader` reads some bit of `lsb..lsb + width` of `object`:
    // `None` when it records no read of the object.
    let reads_bits = |reader: usize, object: &A, lsb: usize, width: usize| {
        reads[reader].get(object).map(|bits| match bits {
            Some(ranges) => ranges
                .iter()
                .any(|&(low, high)| low < lsb + width && lsb <= high),
            None => true,
        })
    };

    let mut summary = Summary::default();
    let mut body = String::new();
    for (index, partition) in partitions.iter().enumerate() {
        let plan = &partition.plan;
        summary.cost += plan.cost;
        summary.pinned += usize::from(partition.pinned);
        let _ = writeln!(
            body,
            "partition {index} cost {}{}",
            plan.cost,
            if partition.pinned { " pinned" } else { "" }
        );
        let read = plan
            .read_bits
            .iter()
            .map(|(address, bits)| match bits {
                Some(ranges) => {
                    let ranges = ranges
                        .iter()
                        .map(|(lsb, msb)| format!("{msb}:{lsb}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("{}[{ranges}]", name(address))
                }
                None => format!("{}[*]", name(address)),
            })
            .collect::<Vec<_>>();
        if !read.is_empty() {
            let _ = writeln!(body, "  reads  {}", read.join(" "));
        }
        let writes = partition.writes.iter().map(&name).collect::<Vec<_>>();
        if !writes.is_empty() {
            let _ = writeln!(body, "  writes {}", writes.join(" "));
        }
        for watch in &plan.watched {
            summary.chunks += 1;
            let mut readers = Vec::new();
            let partition_readers = watch
                .readers
                .iter()
                .filter(|&&reader| reader < count)
                .count();
            summary.single += usize::from(partition_readers == 1);
            for &reader in &watch.readers {
                if reader >= count {
                    summary.flags += 1;
                    readers.push(format!("flag{reader}"));
                    continue;
                }
                summary.edges += 1;
                let mut note = String::new();
                if reader <= index {
                    summary.backward += 1;
                    note.push('<');
                }
                match reads_bits(reader, &watch.object, watch.lsb, watch.width) {
                    Some(true) => {}
                    Some(false) => {
                        summary.imprecise += 1;
                        note.push('!');
                    }
                    None => {
                        summary.unresolved += 1;
                        note.push('?');
                    }
                }
                readers.push(format!("{reader}{note}"));
            }
            let _ = writeln!(
                body,
                "  watch  {}[{}:{}] -> {}",
                name(&watch.object),
                watch.lsb + watch.width - 1,
                watch.lsb,
                readers.join(" ")
            );
        }
        if !plan.marks.is_empty() {
            summary.marks += plan.marks.len();
            let marks = plan
                .marks
                .iter()
                .map(|mark| mark.to_string())
                .collect::<Vec<_>>();
            let _ = writeln!(body, "  marks  {}", marks.join(" "));
        }
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "settle plan: {count} partitions ({} pinned), cost {}",
        summary.pinned, summary.cost
    );
    let _ = writeln!(
        out,
        "compared chunks {} ({} with one reader partition), reader edges {} (+{} wait flags), \
         unconditional marks {}",
        summary.chunks, summary.single, summary.edges, summary.flags, summary.marks
    );
    let _ = writeln!(
        out,
        "backward reader edges (<) {}: each reruns its reader in the next settle pass",
        summary.backward
    );
    let _ = writeln!(
        out,
        "readers marked for bits they do not read (!) {}: must be 0",
        summary.imprecise
    );
    let _ = writeln!(
        out,
        "readers with no recorded read of the object (?) {}",
        summary.unresolved
    );
    out.push('\n');
    out.push_str(&body);
    out
}

#[derive(Default)]
struct Summary {
    cost: usize,
    pinned: usize,
    chunks: usize,
    single: usize,
    edges: usize,
    flags: usize,
    marks: usize,
    backward: usize,
    imprecise: usize,
    unresolved: usize,
}
