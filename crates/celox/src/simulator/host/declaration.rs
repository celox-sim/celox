//! Runtime static declarations precede ordinary initial and always procedures.
use super::*;
use celox_design::{InitialStateData, InitialStateValue, VarAtomBase};

impl<B: SimBackend> Simulator<B> {
    fn apply_declaration_records(&mut self, records: &[InitialStateValue<crate::AbsoluteAddr>]) {
        for record in records {
            let signal = self.backend.resolve_signal(&record.address);
            match &record.data {
                InitialStateData::Packed {
                    value,
                    mask,
                    written_mask,
                } => {
                    let all = (BigUint::from(1u8) << signal.width) - 1u8;
                    let written = written_mask & &all;
                    let keep = &all ^ &written;
                    let (previous, previous_mask) = self.backend.get_four_state(signal);
                    let value = (previous & &keep) | (value & &written);
                    let mask = (previous_mask & keep) | (mask & written);
                    if self.backend.layout().four_state && signal.is_4state {
                        self.backend.set_four_state(signal, value, mask);
                    } else {
                        self.backend
                            .set_wide(signal, value & (&all ^ (mask & &all)));
                    }
                }
                InitialStateData::Writes(runs) => self.apply_initial_memory_writes(signal, runs),
            }
        }
        self.dirty |= !records.is_empty();
    }

    fn declaration_range(&self, source: VarAtomBase<crate::AbsoluteAddr>) -> (BigUint, BigUint) {
        let signal = self.backend.resolve_signal(&source.id);
        let (value, mask) = self.backend.get_four_state(signal);
        let width = source.access.msb - source.access.lsb + 1;
        let all = (BigUint::from(1u8) << width) - 1u8;
        (
            (value >> source.access.lsb) & &all,
            (mask >> source.access.lsb) & all,
        )
    }

    pub(crate) fn reset_declaration_state(&mut self) {
        self.declaration_phase = true;
        if let Some(startup) = self.program.runtime_schema.declaration_startup.clone() {
            self.apply_declaration_records(&startup.reset);
        }
    }

    pub(crate) fn prime_declaration_state(&mut self) {
        // Every initializer has completed. A read while an initializer was
        // stopped may have cleared dirty without evaluating anything.
        self.declaration_phase = false;
        self.dirty = true;
        let Some(startup) = self.program.runtime_schema.declaration_startup.clone() else {
            return;
        };
        for (source, saved) in startup.snapshots {
            let (value, mask) = self.declaration_range(source);
            let saved = self.backend.resolve_signal(&saved);
            if self.backend.layout().four_state && saved.is_4state {
                self.backend.set_four_state(saved, value, mask);
            } else {
                let all = (BigUint::from(1u8) << saved.width) - 1u8;
                self.backend.set_wide(saved, value & (&all ^ mask));
            }
        }
        for (counter, saved) in startup.counter_snapshots {
            let value = self.backend.get(self.backend.resolve_signal(&counter));
            self.backend
                .set_wide(self.backend.resolve_signal(&saved), value);
        }
    }

    pub(crate) fn apply_ordinary_initial_state(&mut self) {
        let Some(startup) = self.program.runtime_schema.declaration_startup.clone() else {
            return;
        };
        let watches = self.program.runtime_schema.watches.clone();
        for record in startup.ordinary {
            let before: Vec<_> = startup
                .probes
                .iter()
                .map(|(source, _)| self.declaration_range(*source))
                .collect();
            let watched_before: Vec<_> = watches
                .iter()
                .map(|watch| {
                    self.backend
                        .get_four_state(self.backend.resolve_signal(&watch.signal))
                })
                .collect();
            self.apply_declaration_records(std::slice::from_ref(&record));
            // One counter can observe several transparent aliases. Count a
            // write once when any of its observed projections changed.
            let mut changed = crate::HashSet::default();
            for ((source, counter), before) in startup.probes.iter().zip(before) {
                if before != self.declaration_range(*source) {
                    changed.insert(*counter);
                }
            }
            for counter in changed {
                let counter = self.backend.resolve_signal(&counter);
                let value = self.backend.get_as::<u32>(counter).wrapping_add(1);
                self.backend.set(counter, value);
            }
            for (watch, before) in watches.iter().zip(watched_before) {
                let after = self
                    .backend
                    .get_four_state(self.backend.resolve_signal(&watch.signal));
                for (kind, counter) in watch.counters() {
                    if celox_runtime::timing::occurs(kind, &before, &after) {
                        let counter = self.backend.resolve_signal(&counter);
                        let value = self.backend.get_as::<u32>(counter).wrapping_add(1);
                        self.backend.set(counter, value);
                    }
                }
            }
        }
    }
}
