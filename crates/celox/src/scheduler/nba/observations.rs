//! Occurrence accounting around one NBA update, even when its physical write
//! spans several chunks. A shared probe counter advances once per update.
use super::*;
use celox_frontend_core::watch::{self, GatedCode, WatchCode, WatchSnapshot};

type Range = (AbsoluteAddr, usize, usize);

pub(super) struct Observations {
    watches: Vec<WatchCode<RegionedAbsoluteAddr>>,
    probes: Vec<(AbsoluteAddr, Vec<Range>)>,
}

pub(super) struct Snapshot {
    watches: Vec<WatchSnapshot>,
    probes: Vec<Vec<RegisterId>>,
}

impl Observations {
    pub(super) fn new(program: &LaidOutProgram, target: AbsoluteAddr) -> Self {
        let schema = &program.runtime.runtime_schema;
        let layout = program.layout();
        let shared = program.shared_storage();
        let storage = |signal| shared.get(&signal).copied().unwrap_or(signal);
        let target = storage(target);
        let unknowns =
            |signal| layout.four_state && layout.is_4states.get(&signal).copied().unwrap_or(false);
        let mut watches: Vec<_> = schema
            .watches
            .iter()
            .filter(|w| storage(w.signal) == target)
            .map(|w| WatchCode::new(w, layout.four_state, stable))
            .collect();
        let mut gates = HashSet::default();
        for site in &schema.wait_sites {
            for (index, &(signal, kind)) in site.terms.iter().enumerate() {
                if storage(signal) != target {
                    continue;
                }
                let Some(gate) = site.gates.get(index).copied().flatten() else {
                    continue;
                };
                let Some(counters) = gate.counters else {
                    continue;
                };
                if !gates.insert((
                    kind,
                    storage(gate.signal),
                    storage(counters.gated),
                    storage(counters.all),
                )) {
                    continue;
                }
                watches.push(WatchCode {
                    signal: stable(signal),
                    width: layout.widths[&signal],
                    offset: SIROffset::Static(0),
                    four_state: unknowns(signal),
                    counters: Vec::new(),
                    gated: vec![GatedCode {
                        kind,
                        gate: stable(gate.signal),
                        gate_four_state: unknowns(gate.signal),
                        gated: stable(counters.gated),
                        all: stable(counters.all),
                    }],
                });
            }
        }
        let mut probes: Vec<(AbsoluteAddr, Vec<Range>)> = Vec::new();
        for (source, counter) in &schema.occurrence_probes {
            if storage(source.id) != target {
                continue;
            }
            let counter = storage(*counter);
            let group = match probes.iter().position(|&(known, _)| known == counter) {
                Some(index) => index,
                None => {
                    probes.push((counter, Vec::new()));
                    probes.len() - 1
                }
            };
            let range = (
                source.id,
                source.access.lsb,
                source.access.msb - source.access.lsb + 1,
            );
            if !probes[group].1.contains(&range) {
                probes[group].1.push(range);
            }
        }
        Self { watches, probes }
    }

    fn probe_words(g: &mut Generator<'_>, ranges: &[Range]) -> Vec<RegisterId> {
        ranges
            .iter()
            .flat_map(|&(signal, lsb, width)| {
                (0..width)
                    .step_by(64)
                    .map(move |offset| (signal, lsb + offset, (width - offset).min(64)))
            })
            .map(|(signal, lsb, width)| g.load_state(signal, lsb, width))
            .collect()
    }

    pub(super) fn before(&self, g: &mut Generator<'_>) -> Snapshot {
        Snapshot {
            watches: self
                .watches
                .iter()
                .map(|w| watch::watch_before(&mut g.b, w))
                .collect(),
            probes: self
                .probes
                .iter()
                .map(|(_, ranges)| Self::probe_words(g, ranges))
                .collect(),
        }
    }

    pub(super) fn after(&self, g: &mut Generator<'_>, before: Snapshot) {
        if let Some((_, blocks)) = &mut g.partitioned_glue {
            blocks.insert(g.b.current_block());
        }
        for (watch, snapshot) in self.watches.iter().zip(before.watches) {
            watch::watch_after(&mut g.b, watch, snapshot, None);
        }
        for ((counter, ranges), old) in self.probes.iter().zip(before.probes) {
            let mut changed = g.constant(0, 1);
            for (old, new) in old.into_iter().zip(Self::probe_words(g, ranges)) {
                let differs = g.binary(1, old, BinaryOp::NeCase, new);
                changed = g.binary(1, changed, BinaryOp::Or, differs);
            }
            let width = celox_design::EVENT_COUNTER_WIDTH;
            let old = g.load(*counter, width);
            let add = g.widen(changed, width);
            let new = g.binary(width, old, BinaryOp::Add, add);
            g.store(*counter, width, new);
        }
    }
}
