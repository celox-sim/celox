//! Exact live-set cardinality without copying unchanged live-through values.
use crate::HashMap;
use crate::native::mir::VReg;

pub(super) struct LiveCount<F> {
    initial_contains: F,
    changed: HashMap<VReg, bool>,
    count: usize,
}

impl<F: Fn(&VReg) -> bool> LiveCount<F> {
    pub(super) fn new(count: usize, initial_contains: F) -> Self {
        Self {
            initial_contains,
            changed: HashMap::default(),
            count,
        }
    }

    pub(super) fn set(&mut self, value: VReg, present: bool) {
        let previous = self
            .changed
            .entry(value)
            .or_insert_with(|| (self.initial_contains)(&value));
        if *previous != present {
            if present {
                self.count += 1;
            } else {
                self.count -= 1;
            }
            *previous = present;
        }
    }

    pub(super) fn len(&self) -> usize {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn local_updates_match_materialized_sets() {
        for seed in 0..32u32 {
            let initial = (0..8192)
                .filter(|value| (value + seed) % 3 != 0)
                .map(VReg)
                .collect::<BTreeSet<_>>();
            let mut expected = initial.clone();
            let mut actual = LiveCount::new(initial.len(), |value| initial.contains(value));
            for step in 0..1024u32 {
                let value = VReg((step * 37 + seed * 19) % 128 + 8150);
                let present = (step + seed) % 5 < 3;
                if present {
                    expected.insert(value);
                } else {
                    expected.remove(&value);
                }
                actual.set(value, present);
                assert_eq!(actual.len(), expected.len(), "seed={seed} step={step}");
            }
            assert!(actual.changed.len() <= 128);
        }
        let mut empty = LiveCount::new(0, |_| false);
        empty.set(VReg(0), false);
        empty.set(VReg(0), true);
        empty.set(VReg(0), true);
        assert_eq!(empty.len(), 1);
        empty.set(VReg(0), false);
        assert_eq!(empty.len(), 0);
    }
}
