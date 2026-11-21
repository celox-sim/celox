//! Compare the selected outputs of a settle group while its inputs are active.
use super::*;

#[derive(Clone, Copy)]
enum Sample {
    Signal(AbsoluteAddr, usize),
    Word(usize),
}

impl Sample {
    fn read(self, g: &mut Generator<'_>) -> RegisterId {
        match self {
            Self::Signal(addr, width) => g.load_state(addr, 0, width),
            Self::Word(offset) => {
                let offset = g.constant(offset as u64, 64);
                packed_commits::read(&mut g.b, offset)
            }
        }
    }
}

impl Generator<'_> {
    pub(super) fn run_publishing_group(
        &mut self,
        active: RegisterId,
        bit: usize,
        mut outputs: &[(AbsoluteAddr, u64)],
        run: impl FnOnce(&mut Self),
    ) -> RegisterId {
        let cond = self.mask_bit(active, bit);
        let body = self.b.new_block();
        let result = self.b.alloc_bit(64, false);
        let next = self.b.new_block_with(vec![result]);
        self.b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (body, vec![]),
            false_block: (next, vec![active]),
        });
        self.b.switch_to_block(body);
        let layout = self.program.layout();
        let pack = self.outlined.is_some() && !layout.four_state && layout.trace.is_none();
        let mut samples = Vec::new();
        while let Some((&(addr, readers), rest)) = outputs.split_first() {
            let offset = layout.offsets[&addr];
            // Eight adjacent bytes for the same consumers can be compared as
            // one word. The word contains only the sampled objects.
            let word = pack
                && outputs.get(..8).is_some_and(|lanes| {
                    lanes.iter().enumerate().all(|(i, &(a, mask))| {
                        readers == mask
                            && layout.widths[&a] <= 8
                            && layout.offsets[&a] == offset + i
                    })
                });
            let sample = if word {
                outputs = &outputs[8..];
                Sample::Word(offset)
            } else {
                outputs = rest;
                Sample::Signal(addr, layout.widths[&addr])
            };
            samples.push((readers, sample, sample.read(self)));
        }
        run(self);
        let mut updated = active;
        // The planner orders samples by their reader mask. Publish once for
        // the entire set even when many independently stored lanes change.
        for group in samples.chunk_by(|left, right| left.0 == right.0) {
            let mut changed = None;
            for &(_, sample, before) in group {
                let after = sample.read(self);
                let differs = self.binary(1, before, BinaryOp::NeCase, after);
                changed = Some(match changed {
                    None => differs,
                    Some(any) => self.binary(1, any, BinaryOp::Or, differs),
                });
            }
            let bits = self.constant(group[0].0, 64);
            let set = self.binary(64, updated, BinaryOp::Or, bits);
            let value = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Mux(value, changed.unwrap(), set, updated));
            updated = value;
        }
        self.b.seal_block(SIRTerminator::Jump(next, vec![updated]));
        self.b.switch_to_block(next);
        result
    }
}
