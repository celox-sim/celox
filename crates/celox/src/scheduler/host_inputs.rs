//! Replay host-written values into native wait snapshots before HDL execution.
use super::*;
use crate::simulator::native_io::ArenaHeader;
use std::mem::offset_of;

pub(crate) fn signals(schema: &celox_design::RuntimeSchema<AbsoluteAddr>) -> Vec<AbsoluteAddr> {
    let mut seen = HashSet::default();
    schema
        .wait_sites
        .iter()
        .flat_map(|site| site.terms.iter().map(|&(signal, _)| signal))
        .filter(|signal| seen.insert(*signal))
        .collect()
}

/// Retain the source snapshot offsets while removing terms the host did not
/// update. Counters and gates already observed host writes at the setter.
fn filtered(wait: &Wait, signal: AbsoluteAddr) -> Wait {
    let selected: Vec<_> = wait
        .terms
        .iter()
        .enumerate()
        .filter_map(|(index, &(term, _))| {
            (term == signal
                && wait.counters[index].is_none()
                && wait.gates.get(index).is_none_or(Option::is_none))
            .then_some(index)
        })
        .collect();
    let mut raw = wait.raw.clone();
    if let Some(raw) = &mut raw {
        raw.terms = selected.iter().map(|&index| raw.terms[index]).collect();
    }
    Wait {
        terms: selected.iter().map(|&index| wait.terms[index]).collect(),
        counters: vec![None; selected.len()],
        gates: vec![None; selected.len()],
        site_terms: wait.site_terms.clone(),
        alternatives: wait
            .alternatives
            .iter()
            .map(|wait| filtered(wait, signal))
            .collect(),
        raw,
        reactive: wait.reactive,
        sites: Vec::new(),
    }
}

pub(super) fn replay(program: &LaidOutProgram, g: &mut Generator<'_>, checks: &[WaitCheck<'_>]) {
    let sources = signals(&program.runtime.runtime_schema);
    if sources.is_empty() {
        return;
    }
    let mut affected: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
    for (index, check) in checks.iter().enumerate() {
        let mut seen = HashSet::default();
        for &(signal, _) in &check.wait.terms {
            if seen.insert(signal) {
                affected.entry(signal).or_default().push(index);
            }
        }
    }
    let data = g.arena_header_load(offset_of!(ArenaHeader, host_values));
    let len = g.arena_header_load(offset_of!(ArenaHeader, host_words));
    let index = g.b.alloc_bit(64, false);
    let again = g.b.new_block_with(vec![index]);
    let dispatch = g.b.new_block();
    let done = g.b.new_block();
    let zero = g.constant(0, 64);
    g.b.seal_block(SIRTerminator::Jump(again, vec![zero]));
    g.b.switch_to_block(again);
    let more = g.binary(1, index, BinaryOp::LtU, len);
    g.b.seal_block(SIRTerminator::Branch {
        cond: more,
        true_block: (dispatch, Vec::new()),
        false_block: (done, Vec::new()),
    });
    g.b.switch_to_block(dispatch);
    let id = g.arena_load(data, index);
    let eight = g.constant(8, 64);
    let bytes = g.binary(64, index, BinaryOp::Mul, eight);
    let record = g.binary(64, data, BinaryOp::Add, bytes);
    let targets: Vec<_> = sources.iter().map(|_| g.b.new_block()).collect();
    let invalid = g.exit_block(SchedulerExit::Internal);
    g.dispatch_sites(id, &targets, invalid);
    for (signal, block) in sources.into_iter().zip(targets) {
        g.b.switch_to_block(block);
        let object = nba::writes::Object {
            signal,
            width: program.layout().widths[&signal],
            element: program
                .layout()
                .unpacked_arrays
                .get(&signal)
                .map(|layout| layout.element_width),
        };
        let words = object.width.div_ceil(64) * if g.keeps_unknowns(signal) { 2 } else { 1 };
        let size = g.constant((16 + words * 2) as u64, 64);
        let next = g.binary(64, index, BinaryOp::Add, size);
        let in_bounds = g.binary(1, next, BinaryOp::LeU, len);
        g.arena_require(in_bounds);
        let width = g.constant(object.width as u64, 64);
        nba::writes::write(g, object, object, Some(record), zero, width);
        for &check_index in affected.get(&signal).map_or(&[][..], Vec::as_slice) {
            let check = &checks[check_index];
            let wait = check.wait;
            if wait.raw.is_some() || !wait.alternatives.is_empty() {
                let filtered = filtered(wait, signal);
                let check = WaitCheck {
                    wait: &filtered,
                    ..*check
                };
                g.check_waits_at(&[check], false);
            } else {
                let mut selected = g.constant(0, 1);
                for (ordinal, &(term, _)) in wait.terms.iter().enumerate() {
                    if term != signal
                        || wait.counters[ordinal].is_some()
                        || wait.gates.get(ordinal).is_some_and(Option::is_some)
                    {
                        continue;
                    }
                    let active = if wait.terms.len() == 1 {
                        g.constant(1, 1)
                    } else {
                        let baseline = g.load(g.slots.baseline[check.index], SCHEDULER_FLAG_WIDTH);
                        let shift =
                            g.constant(u64::from(BASELINE_TERM_SHIFT), SCHEDULER_FLAG_WIDTH);
                        let term = g.binary(SCHEDULER_FLAG_WIDTH, baseline, BinaryOp::Shr, shift);
                        g.equals(term, ordinal as u64)
                    };
                    selected = g.binary(1, selected, BinaryOp::Or, active);
                }
                g.if_bit(selected, 0, |g| g.check_waits_at(&[*check], false));
            }
        }
        let offset = g.constant(((8 + words) * 8) as u64, 64);
        let restore = g.binary(64, record, BinaryOp::Add, offset);
        nba::writes::write(g, object, object, Some(restore), zero, width);
        g.b.seal_block(SIRTerminator::Jump(again, vec![next]));
    }
    g.b.switch_to_block(done);
    g.arena_header_store(offset_of!(ArenaHeader, host_words), zero);
}
