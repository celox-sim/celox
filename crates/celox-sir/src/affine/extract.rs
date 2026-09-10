use super::{Kernel, MemoryObject, Result, StatementBody};
use crate::{
    BinaryOp, BlockId, ExecutionUnit, HashMap, HashSet, RegisterId, SIRInstruction, SIRTerminator,
};
use celox_analysis::polyhedral::{Affine, Domain, Error};
use num_traits::{ToPrimitive, Zero};
use std::hash::Hash;

#[derive(Clone)]
struct Loop {
    induction: RegisterId,
    start: i64,
    end: i64,
    body: BlockId,
    exit: BlockId,
}

#[derive(Clone)]
enum Time {
    Axis(usize),
    Scalar(i64),
}

/// Extract canonical counted memory loops from a complete execution unit.
/// Accepts sequences and imperfect nests, one induction phi per loop, constant
/// bounds, unit increments, and branch-free statement bodies. Scalar carried
/// phis, arbitrary control flow, events, and inter-statement SSA values are
/// rejected instead of being treated as independent memory computations.
pub fn extract<A: Clone + Eq + Hash>(
    unit: &ExecutionUnit<A>,
    objects: &HashMap<A, MemoryObject>,
) -> Result<Kernel<A>> {
    unit.verify_result()
        .map_err(|_| Error::Invalid("input SIR failed verification"))?;
    let cfg = crate::cfg::SirCfg::analyze_forward_structure(unit)
        .map_err(|_| Error::Invalid("SIR control flow"))?;
    let definitions = unit
        .blocks
        .iter()
        .flat_map(|(&block, contents)| {
            contents
                .instructions
                .iter()
                .filter_map(move |inst| inst.defined_register().map(|reg| (reg, (block, inst))))
        })
        .collect::<HashMap<_, _>>();
    let constant = |register: RegisterId| -> Result<i64> {
        let (_, SIRInstruction::Imm(_, value)) = definitions
            .get(&register)
            .ok_or(Error::Invalid("nonconstant loop bound"))?
        else {
            return Err(Error::Invalid("nonconstant loop bound"));
        };
        let ty = &unit.register_map[&register];
        if !value.mask.is_zero() || ty.width() == 0 || ty.width() > 64 {
            return Err(Error::Invalid("loop bound representation"));
        }
        let payload = value
            .payload
            .to_u64()
            .ok_or(Error::Invalid("loop bound width"))?;
        let width = ty.width();
        let value = i128::from(payload) & ((1i128 << width) - 1);
        let value = if ty.is_signed() && value & (1i128 << (width - 1)) != 0 {
            value - (1i128 << width)
        } else {
            value
        };
        i64::try_from(value).map_err(|_| Error::ArithmeticOverflow)
    };
    let mut loops = HashMap::default();
    let mut control = HashSet::default();
    for natural in &cfg.loops {
        let header = cfg.block_ids[natural.header];
        let block = &unit.blocks[&header];
        let [induction] = block.params.as_slice() else {
            return Err(Error::Invalid("SIR loop has carried scalar state"));
        };
        let SIRTerminator::Branch {
            cond,
            true_block: (body, true_args),
            false_block: (exit, false_args),
        } = &block.terminator
        else {
            return Err(Error::Invalid("SIR loop header"));
        };
        let body_index = cfg
            .block_ids
            .iter()
            .position(|id| id == body)
            .ok_or(Error::Invalid("SIR loop body"))?;
        let exit_index = cfg
            .block_ids
            .iter()
            .position(|id| id == exit)
            .ok_or(Error::Invalid("SIR loop exit"))?;
        if !natural.blocks.contains(&body_index) || natural.blocks.contains(&exit_index) {
            return Err(Error::Invalid("SIR loop branch direction"));
        }
        if !true_args.is_empty() || !false_args.is_empty() {
            return Err(Error::Invalid("SIR loop edge arguments"));
        }
        let (_, SIRInstruction::Binary(_, left, op, bound)) = definitions
            .get(cond)
            .ok_or(Error::Invalid("SIR loop comparison"))?
        else {
            return Err(Error::Invalid("SIR loop comparison"));
        };
        if left != induction
            || !matches!(
                op,
                BinaryOp::LtS | BinaryOp::LtU | BinaryOp::LeS | BinaryOp::LeU
            )
        {
            return Err(Error::Invalid("SIR loop direction"));
        }
        let predecessors = &cfg.predecessors[natural.header];
        let outside = predecessors
            .iter()
            .filter(|p| !natural.blocks.contains(p))
            .copied()
            .collect::<Vec<_>>();
        let inside = predecessors
            .iter()
            .filter(|p| natural.blocks.contains(p))
            .copied()
            .collect::<Vec<_>>();
        let ([preheader], [latch]) = (outside.as_slice(), inside.as_slice()) else {
            return Err(Error::Invalid(
                "SIR loop must have one entry and one backedge",
            ));
        };
        let jump_argument = |block: BlockId| -> Result<RegisterId> {
            let SIRTerminator::Jump(target, args) = &unit.blocks[&block].terminator else {
                return Err(Error::Invalid("SIR loop edge"));
            };
            let [argument] = args.as_slice() else {
                return Err(Error::Invalid("SIR loop argument count"));
            };
            if *target != header {
                return Err(Error::Invalid("SIR loop target"));
            }
            Ok(*argument)
        };
        let initial = jump_argument(cfg.block_ids[*preheader])?;
        let start = constant(initial)?;
        let end = constant(*bound)?
            .checked_add(i64::from(matches!(op, BinaryOp::LeS | BinaryOp::LeU)))
            .ok_or(Error::ArithmeticOverflow)?;
        let next = jump_argument(cfg.block_ids[*latch])?;
        let (_, SIRInstruction::Binary(_, current, BinaryOp::Add, step)) =
            definitions
                .get(&next)
                .ok_or(Error::Invalid("SIR induction increment"))?
        else {
            return Err(Error::Invalid("SIR induction increment"));
        };
        if current != induction || constant(*step)? != 1 || start >= end {
            return Err(Error::Invalid("SIR induction progression"));
        }
        let ty = &unit.register_map[induction];
        if ty.width() == 0
            || ty.width() > 64
            || unit.register_map[&initial] != *ty
            || unit.register_map[&next] != *ty
            || unit.register_map[bound] != *ty
            || unit.register_map[step] != *ty
        {
            return Err(Error::Invalid("SIR induction type mismatch"));
        }
        if matches!(op, BinaryOp::LtS | BinaryOp::LeS) != ty.is_signed() {
            return Err(Error::Invalid("SIR comparison signedness"));
        }
        // The final increment is observable in the original CFG too: prove
        // it fits, including the terminating value end, before removing it.
        let proof = Domain::rectangular(vec![
            start..end.checked_add(1).ok_or(Error::ArithmeticOverflow)?,
        ]);
        if !super::fits(&Affine::axis(1, 0), &proof, ty)? {
            return Err(Error::Invalid("SIR induction may wrap"));
        }
        if block.instructions.iter().any(|inst| {
            !matches!(inst, SIRInstruction::Imm(..)) && inst.defined_register() != Some(*cond)
        }) {
            return Err(Error::Invalid("SIR header has computation or effects"));
        }
        control.extend([*cond, next]);
        loops.insert(
            header,
            Loop {
                induction: *induction,
                start,
                end,
                body: *body,
                exit: *exit,
            },
        );
    }
    if loops.is_empty() {
        return Err(Error::Invalid("no canonical SIR memory loops"));
    }
    struct Extractor<'a, A> {
        unit: &'a ExecutionUnit<A>,
        loops: HashMap<BlockId, Loop>,
        control: HashSet<RegisterId>,
        definitions: HashMap<RegisterId, (BlockId, &'a SIRInstruction<A>)>,
        visited: HashSet<BlockId>,
        bodies: Vec<StatementBody<A>>,
    }
    impl<A: Clone> Extractor<'_, A> {
        fn definition(
            &self,
            register: RegisterId,
            block: BlockId,
            induction: &[RegisterId],
            ready: &mut HashSet<RegisterId>,
            instructions: &mut Vec<SIRInstruction<A>>,
            depth: usize,
        ) -> Result<()> {
            if ready.contains(&register) || induction.contains(&register) {
                return Ok(());
            }
            if depth >= 128 {
                return Err(Error::WorkLimit);
            }
            let (origin, instruction) = self
                .definitions
                .get(&register)
                .ok_or(Error::Invalid("SIR live-in is not an induction variable"))?;
            if (*origin != block && matches!(instruction, SIRInstruction::Load(..)))
                || self.control.contains(&register)
            {
                return Err(Error::Invalid("cross-statement SIR value"));
            }
            let mut uses = Vec::new();
            crate::analysis::visit_instruction_uses(instruction, |reg| uses.push(reg));
            for reg in uses {
                self.definition(reg, block, induction, ready, instructions, depth + 1)?;
            }
            ready.insert(register);
            instructions.push((*instruction).clone());
            Ok(())
        }
        fn walk(
            &mut self,
            mut current: BlockId,
            stop: Option<BlockId>,
            axes: &mut Vec<Loop>,
            prefix: &mut Vec<Time>,
        ) -> Result<()> {
            let mut sequence = 0;
            while Some(current) != stop {
                if !self.visited.insert(current) {
                    return Err(Error::Invalid("non-structured SIR loop region"));
                }
                if let Some(loop_) = self.loops.get(&current).cloned() {
                    if axes.len() >= 4 {
                        return Err(Error::WorkLimit);
                    }
                    prefix.extend([Time::Scalar(sequence), Time::Axis(axes.len())]);
                    axes.push(loop_.clone());
                    self.walk(loop_.body, Some(current), axes, prefix)?;
                    axes.pop();
                    prefix.truncate(prefix.len() - 2);
                    current = loop_.exit;
                    sequence += 1;
                    continue;
                }
                let block = &self.unit.blocks[&current];
                if !block.params.is_empty() {
                    return Err(Error::Invalid("SIR carried state outside a loop header"));
                }
                if block
                    .instructions
                    .iter()
                    .any(|inst| matches!(inst, SIRInstruction::Store(..)))
                {
                    let induction = axes.iter().map(|loop_| loop_.induction).collect::<Vec<_>>();
                    let mut instructions = Vec::new();
                    let mut ready = HashSet::default();
                    for instruction in &block.instructions {
                        if instruction
                            .defined_register()
                            .is_some_and(|reg| self.control.contains(&reg))
                        {
                            continue;
                        }
                        if let Some(reg) = instruction.defined_register() {
                            self.definition(
                                reg,
                                current,
                                &induction,
                                &mut ready,
                                &mut instructions,
                                0,
                            )?;
                        } else {
                            let mut uses = Vec::new();
                            crate::analysis::visit_instruction_uses(instruction, |reg| {
                                uses.push(reg)
                            });
                            for reg in uses {
                                self.definition(
                                    reg,
                                    current,
                                    &induction,
                                    &mut ready,
                                    &mut instructions,
                                    0,
                                )?;
                            }
                            instructions.push(instruction.clone());
                        }
                    }
                    let domain = Domain::rectangular(
                        axes.iter().map(|l| l.start..l.end).collect::<Vec<_>>(),
                    );
                    let original_schedule = prefix
                        .iter()
                        .chain([&Time::Scalar(sequence)])
                        .map(|time| match time {
                            Time::Axis(axis) => Affine::axis(axes.len(), *axis),
                            Time::Scalar(value) => Affine::constant(axes.len(), *value),
                        })
                        .collect();
                    self.bodies.push(StatementBody {
                        domain,
                        original_schedule,
                        induction,
                        register_types: self.unit.register_map.clone(),
                        instructions,
                    });
                    sequence += 1;
                } else if block.instructions.iter().any(|inst| {
                    !matches!(
                        inst,
                        SIRInstruction::Imm(..)
                            | SIRInstruction::Binary(..)
                            | SIRInstruction::Unary(..)
                            | SIRInstruction::Concat(..)
                            | SIRInstruction::Slice(..)
                            | SIRInstruction::Mux(..)
                    )
                }) {
                    return Err(Error::Invalid("unmodeled SIR memory access or effect"));
                } else if block.instructions.iter().any(|inst| {
                    matches!(
                        inst,
                        SIRInstruction::Binary(
                            _,
                            _,
                            BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS,
                            _
                        )
                    )
                }) {
                    return Err(Error::Invalid("potential trap outside a SIR statement"));
                }
                match &block.terminator {
                    SIRTerminator::Jump(next, _) => current = *next,
                    SIRTerminator::Return if stop.is_none() => return Ok(()),
                    _ => return Err(Error::Invalid("non-affine SIR control flow")),
                }
            }
            Ok(())
        }
    }
    let mut extractor = Extractor {
        unit,
        loops,
        control,
        definitions,
        visited: HashSet::default(),
        bodies: Vec::new(),
    };
    extractor.walk(unit.entry_block_id, None, &mut Vec::new(), &mut Vec::new())?;
    if extractor.visited.len() != unit.blocks.len() {
        return Err(Error::Invalid("unmodeled SIR blocks"));
    }
    let dimensions = extractor
        .bodies
        .iter()
        .map(|b| b.original_schedule.len())
        .max()
        .unwrap_or(0);
    for body in &mut extractor.bodies {
        body.original_schedule
            .resize(dimensions, Affine::constant(body.domain.bounds.len(), 0));
    }
    Kernel::from_bodies(extractor.bodies, objects)
}
