//! The effects of calls of function units ([`CallEffects`]).
//!
//! A function's effects are those of its body, frame slots excluded, and of
//! the functions it calls, transitively, so recursion reaches a fixpoint.
//! They are computed once per function and shared by its calls, which every
//! analysis reads instead of the callee's body.

use std::collections::BTreeSet;
use std::hash::Hash;
use std::sync::Arc;

use crate::{CallEffects, ExecutionUnit, HashSet, SIRInstruction, SirProgram};

/// Effects being collected, as sets.
#[derive(Clone, PartialEq, Eq)]
struct Collected<A> {
    reads: BTreeSet<A>,
    writes: BTreeSet<A>,
    triggers: BTreeSet<A>,
    host: bool,
    kernel: bool,
}

impl<A: Ord> Default for Collected<A> {
    fn default() -> Self {
        Self {
            reads: BTreeSet::new(),
            writes: BTreeSet::new(),
            triggers: BTreeSet::new(),
            host: false,
            kernel: false,
        }
    }
}

impl<A: Ord + Clone> Collected<A> {
    fn absorb(&mut self, other: &Collected<A>) {
        self.reads.extend(other.reads.iter().cloned());
        self.writes.extend(other.writes.iter().cloned());
        self.triggers.extend(other.triggers.iter().cloned());
        self.host |= other.host;
        self.kernel |= other.kernel;
    }

    fn into_effects(self) -> CallEffects<A> {
        CallEffects {
            reads: self.reads.into_iter().collect(),
            writes: self.writes.into_iter().collect(),
            triggers: self.triggers.into_iter().collect(),
            host: self.host,
            kernel: self.kernel,
        }
    }
}

/// The effects of `unit` itself, not counting the calls it makes, which
/// `callees` receives. `frame` holds the addresses that are frame slots.
fn own_effects<A: Ord + Clone + Hash>(
    unit: &ExecutionUnit<A>,
    frame: &HashSet<&A>,
    callees: &mut Vec<usize>,
) -> Collected<A> {
    let mut collected = Collected::default();
    for block in unit.blocks.values() {
        for instruction in &block.instructions {
            if let SIRInstruction::Call { function, .. } = instruction {
                callees.push(function.0 as usize);
                continue;
            }
            if let Some(access) = instruction.memory_read()
                && !frame.contains(access.addr)
            {
                collected.reads.insert(access.addr.clone());
            }
            if let Some(access) = instruction.memory_write()
                && !frame.contains(access.addr)
            {
                collected.writes.insert(access.addr.clone());
                let triggers = match instruction {
                    SIRInstruction::Store(_, _, _, _, triggers, _)
                    | SIRInstruction::Commit(_, _, _, _, triggers) => !triggers.is_empty(),
                    _ => false,
                };
                if triggers {
                    collected.triggers.insert(access.addr.clone());
                }
            }
            collected.kernel |= instruction.accesses_unknown_state();
            collected.host |= instruction.is_host_interaction();
        }
    }
    collected
}

impl<EventAddr, StateAddr: Ord + Clone + Hash> SirProgram<EventAddr, StateAddr> {
    /// The effects of a call of each function, by [`crate::FunctionId`].
    pub fn call_effects(&self) -> Vec<CallEffects<StateAddr>> {
        let frame: HashSet<&StateAddr> = self
            .functions
            .iter()
            .flat_map(|function| &function.frame.slots)
            .map(|slot| &slot.addr)
            .collect();
        let mut callees = vec![Vec::new(); self.functions.len()];
        let own: Vec<Collected<StateAddr>> = self
            .functions
            .iter()
            .zip(&mut callees)
            .map(|(function, callees)| own_effects(&function.unit, &frame, callees))
            .collect();
        // Grow each function's effects by its callees' until none changes;
        // the sets only grow and are bounded, so this ends.
        let mut effects = own.clone();
        let mut changed = true;
        while changed {
            changed = false;
            for function in 0..effects.len() {
                let mut grown = effects[function].clone();
                for &callee in &callees[function] {
                    if let Some(callee) = effects.get(callee) {
                        grown.absorb(callee);
                    }
                }
                if grown != effects[function] {
                    effects[function] = grown;
                    changed = true;
                }
            }
        }
        effects.into_iter().map(Collected::into_effects).collect()
    }

    /// Whether the program has function units or calls. Analyses and
    /// transformations that do not handle calls yet are gated on this: they
    /// reject or skip such a program rather than misread its calls.
    pub fn has_calls(&self) -> bool {
        !self.functions.is_empty()
            || self.units().any(|unit| {
                unit.blocks.values().any(|block| {
                    block
                        .instructions
                        .iter()
                        .any(|instruction| matches!(instruction, SIRInstruction::Call { .. }))
                })
            })
    }

    /// Whether every function's effects and every call's are the current
    /// ones, which every consumer of call effects requires: the program was
    /// summarized when it was built and after its function bodies last
    /// changed. Programs without functions always are.
    pub fn calls_summarized(&self) -> bool {
        if self.functions.is_empty() {
            return true;
        }
        let effects = self.call_effects();
        let current = |unit: &ExecutionUnit<StateAddr>| {
            unit.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .all(|instruction| match instruction {
                    SIRInstruction::Call {
                        function,
                        effects: call,
                        ..
                    } => effects.get(function.0 as usize) == Some(&**call),
                    _ => true,
                })
        };
        self.functions
            .iter()
            .zip(&effects)
            .all(|(function, effects)| *function.effects == *effects && current(&function.unit))
            && self.units().all(current)
    }

    /// Compute [`FunctionUnit::effects`](crate::FunctionUnit::effects) of
    /// every function and give each [`SIRInstruction::Call`] its callee's.
    /// Run it whenever function bodies or their calls change.
    pub fn summarize_calls(&mut self) {
        if self.functions.is_empty() {
            return;
        }
        let effects: Vec<_> = self.call_effects().into_iter().map(Arc::new).collect();
        for (function, effects) in self.functions.iter_mut().zip(&effects) {
            function.effects = effects.clone();
        }
        let mut give = |unit: &mut ExecutionUnit<StateAddr>| {
            for block in unit.blocks.values_mut() {
                for instruction in &mut block.instructions {
                    if let SIRInstruction::Call {
                        function,
                        effects: call,
                        ..
                    } = instruction
                        && let Some(callee) = effects.get(function.0 as usize)
                    {
                        *call = callee.clone();
                    }
                }
            }
        };
        self.units_mut().for_each(&mut give);
        self.functions
            .iter_mut()
            .for_each(|function| give(&mut function.unit));
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        BasicBlock, BlockId, CallEffects, ExecutionUnit, FrameLayout, FrameSlot, FunctionId,
        FunctionUnit, HashMap, RegisterId, RegisterType, SIRInstruction, SIROffset, SIRTerminator,
        SirProgram, TriggerIdWithKind,
    };

    fn call(function: u32) -> SIRInstruction<u32> {
        SIRInstruction::Call {
            function: FunctionId(function),
            frame: RegisterId(0),
            args: Vec::new(),
            outputs: Vec::new(),
            dst: None,
            effects: Default::default(),
        }
    }

    fn unit(instructions: Vec<SIRInstruction<u32>>) -> ExecutionUnit<u32> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: [(
                RegisterId(0),
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    fn function(instructions: Vec<SIRInstruction<u32>>, slot: u32) -> FunctionUnit<u32> {
        FunctionUnit {
            unit: unit(instructions),
            frame: FrameLayout {
                size: 4,
                slots: vec![FrameSlot {
                    addr: slot,
                    offset: 0,
                    width: 32,
                    four_state: false,
                    handle: false,
                }],
            },
            effects: Default::default(),
        }
    }

    fn store(addr: u32, triggers: Vec<TriggerIdWithKind>) -> SIRInstruction<u32> {
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            32,
            RegisterId(0),
            triggers,
            Vec::new(),
        )
    }

    fn load(addr: u32) -> SIRInstruction<u32> {
        SIRInstruction::Load(RegisterId(0), addr, SIROffset::Static(0), 32)
    }

    #[test]
    fn collects_the_effects_of_every_call_reached() {
        let trigger = TriggerIdWithKind {
            id: 0,
            kind: celox_design::DomainKind::Other,
        };
        // fn0 writes 1 and its slot 100, and calls fn1; fn1 reads 2, stores 3
        // with a trigger, and calls fn0 back; fn2 makes a runtime event.
        let mut program = SirProgram::<u32, u32> {
            eval_comb: vec![unit(vec![call(0)])],
            eval_apply_ffs: HashMap::default(),
            eval_comb_apply_ffs: HashMap::default(),
            merge_comb_apply: false,
            eval_comb_stateful: None,
            eval_only_ffs: HashMap::default(),
            apply_ffs: HashMap::default(),
            parallel: None,
            processes: vec![unit(vec![call(2)])],
            functions: vec![
                function(
                    vec![store(1, Vec::new()), store(100, Vec::new()), call(1)],
                    100,
                ),
                function(vec![load(2), store(3, vec![trigger]), call(0)], 101),
                function(
                    vec![SIRInstruction::RuntimeEvent {
                        site_id: 0,
                        args: Vec::new(),
                    }],
                    102,
                ),
            ],
        };
        assert!(program.has_calls() && !program.calls_summarized());
        program.summarize_calls();
        assert!(program.calls_summarized());
        let recursive = CallEffects {
            reads: vec![2],
            writes: vec![1, 3],
            triggers: vec![3],
            host: false,
            kernel: false,
        };
        assert_eq!(*program.functions[0].effects, recursive);
        assert_eq!(*program.functions[1].effects, recursive);
        assert!(program.functions[2].effects.host);
        // Each call shares its callee's effects.
        let SIRInstruction::Call { effects, .. } =
            &program.eval_comb[0].blocks[&BlockId(0)].instructions[0]
        else {
            unreachable!()
        };
        assert!(std::sync::Arc::ptr_eq(
            effects,
            &program.functions[0].effects
        ));
        program.verify_functions().unwrap();
        program.functions.clear();
        program.eval_comb.clear();
        program.processes.clear();
        assert!(!program.has_calls() && program.calls_summarized());
    }
}
