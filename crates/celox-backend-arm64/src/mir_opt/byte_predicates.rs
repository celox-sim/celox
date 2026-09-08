//! Evaluate invariant boolean columns for up to eight byte lanes at once.

use super::*;
use crate::mir::{BaseReg, BlockId, OpSize, SpillDesc};

#[derive(Clone, Copy)]
enum Expression {
    Load(i32, OpSize),
    Copy(VReg),
    Mask(VReg, u8),
    Not(VReg),
    Binary(VReg, VReg, u8),
}

impl Expression {
    fn sources(self) -> Vec<VReg> {
        match self {
            Self::Load(..) => vec![],
            Self::Copy(src) | Self::Mask(src, _) | Self::Not(src) => vec![src],
            Self::Binary(lhs, rhs, _) => vec![lhs, rhs],
        }
    }
}

#[derive(Clone, Copy)]
struct Node {
    index: VReg,
    expression: Expression,
    boolean: bool,
}

struct Plan {
    header: BlockId,
    entry: BlockId,
    index: VReg,
    nodes: BTreeMap<VReg, Node>,
    order: Vec<VReg>,
    roots: BTreeSet<VReg>,
    needed: BTreeSet<VReg>,
}

fn find_plan(function: &MFunction) -> Option<Plan> {
    let positions = function
        .blocks
        .iter()
        .enumerate()
        .map(|(at, block)| (block.id, at))
        .collect::<HashMap<_, _>>();
    let successors = function
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|id| positions.get(&id).copied())
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()?;
    let cfg =
        celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, 0).ok()?;
    let zeros = known_bits::known_zeros(function);
    for region in &cfg.loops {
        let header = &function.blocks[region.header];
        let entries = cfg.predecessors[region.header]
            .iter()
            .copied()
            .filter(|pred| !region.blocks.contains(pred))
            .collect::<Vec<_>>();
        let &[entry] = entries.as_slice() else {
            continue;
        };
        if !matches!(function.blocks[entry].terminator(), Some(MInst::Jump { target }) if *target == header.id)
        {
            continue;
        }
        let mut nodes = BTreeMap::<VReg, Node>::new();
        let mut order = Vec::new();
        // The layout need not be topological. A bounded fixed point also
        // handles arithmetic shared across the loop's conditional arms.
        for _ in 0..8 {
            let before = nodes.len();
            for &block in &region.blocks {
                for inst in &function.blocks[block].insts {
                    let Some(dst) = inst.def() else { continue };
                    if nodes.contains_key(&dst) {
                        continue;
                    }
                    let unary = |src, expression, boolean| {
                        nodes.get(&src).map(|node| Node {
                            index: node.index,
                            expression,
                            boolean,
                        })
                    };
                    let node = match *inst {
                        MInst::LoadIndexed {
                            base: BaseReg::SimState,
                            offset,
                            index,
                            scale: 1,
                            size: OpSize::S8,
                            alias_range: Some(range),
                            ..
                        } if range.offset() == offset
                            && header.phis.iter().any(|phi| phi.dst == index) =>
                        {
                            let size = match range.byte_len() {
                                2 => OpSize::S16,
                                4 => OpSize::S32,
                                8 => OpSize::S64,
                                _ => continue,
                            };
                            if !zeros[index.0 as usize] >= range.byte_len() as u64
                                || region
                                    .blocks
                                    .iter()
                                    .flat_map(|&block| &function.blocks[block].insts)
                                    .any(|inst| {
                                        counted_loop::may_write(
                                            inst,
                                            i64::from(offset),
                                            i64::from(offset) + range.byte_len() as i64,
                                        )
                                    })
                            {
                                continue;
                            }
                            Some(Node {
                                index,
                                expression: Expression::Load(offset, size),
                                boolean: false,
                            })
                        }
                        MInst::Mov { src, .. } | MInst::Mov32 { src, .. } => {
                            nodes.get(&src).map(|node| Node {
                                expression: Expression::Copy(src),
                                ..*node
                            })
                        }
                        MInst::AndImm { src, imm, .. } => unary(
                            src,
                            Expression::Mask(src, imm as u8),
                            imm & 255 <= 1 || nodes.get(&src).is_some_and(|node| node.boolean),
                        ),
                        MInst::AndImm32 { src, imm, .. } => unary(
                            src,
                            Expression::Mask(src, imm as u8),
                            imm & 255 <= 1 || nodes.get(&src).is_some_and(|node| node.boolean),
                        ),
                        MInst::BitExtract {
                            src,
                            lsb: 0,
                            width: 1,
                            ..
                        } => unary(src, Expression::Mask(src, 1), true),
                        MInst::CmpImm {
                            lhs,
                            imm: 0 | 1,
                            kind: CmpKind::Eq | CmpKind::Ne,
                            ..
                        } if nodes.get(&lhs).is_some_and(|node| node.boolean) => {
                            let MInst::CmpImm { imm, kind, .. } = *inst else {
                                unreachable!()
                            };
                            unary(
                                lhs,
                                if (imm == 0) == (kind == CmpKind::Eq) {
                                    Expression::Not(lhs)
                                } else {
                                    Expression::Copy(lhs)
                                },
                                true,
                            )
                        }
                        MInst::And { lhs, rhs, .. }
                        | MInst::And32 { lhs, rhs, .. }
                        | MInst::Or { lhs, rhs, .. }
                        | MInst::Or32 { lhs, rhs, .. }
                        | MInst::Xor { lhs, rhs, .. }
                        | MInst::Xor32 { lhs, rhs, .. } => {
                            if let Some((left, right)) = nodes.get(&lhs).zip(nodes.get(&rhs))
                                && left.index == right.index
                            {
                                let kind = match inst {
                                    MInst::And { .. } | MInst::And32 { .. } => 0,
                                    MInst::Or { .. } | MInst::Or32 { .. } => 1,
                                    _ => 2,
                                };
                                Some(Node {
                                    index: left.index,
                                    expression: Expression::Binary(lhs, rhs, kind),
                                    boolean: if kind == 0 {
                                        left.boolean || right.boolean
                                    } else {
                                        left.boolean && right.boolean
                                    },
                                })
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(node) = node {
                        nodes.insert(dst, node);
                        order.push(dst);
                    }
                }
            }
            if nodes.len() == before {
                break;
            }
        }
        let mut escaped = BTreeSet::new();
        for block in &function.blocks {
            escaped.extend(
                block
                    .phis
                    .iter()
                    .flat_map(|phi| phi.sources.iter().map(|&(_, source)| source))
                    .filter(|value| nodes.contains_key(value)),
            );
            for inst in &block.insts {
                if inst.def().is_none_or(|dst| !nodes.contains_key(&dst)) {
                    escaped.extend(
                        inst.uses()
                            .into_iter()
                            .filter(|value| nodes.contains_key(value)),
                    );
                }
            }
        }
        for index in nodes
            .values()
            .map(|node| node.index)
            .collect::<BTreeSet<_>>()
        {
            let roots = escaped
                .iter()
                .copied()
                .filter(|value| nodes[value].index == index && nodes[value].boolean)
                .collect::<BTreeSet<_>>();
            if roots.is_empty() {
                continue;
            }
            let mut needed = roots.clone();
            let mut todo = roots.iter().copied().collect::<Vec<_>>();
            while let Some(value) = todo.pop() {
                for source in nodes[&value].expression.sources() {
                    if needed.insert(source) {
                        todo.push(source);
                    }
                }
            }
            let loads = needed
                .iter()
                .filter(|value| matches!(nodes[value].expression, Expression::Load(..)))
                .count();
            let removed = needed
                .iter()
                .filter(|value| !escaped.contains(value) || roots.contains(value))
                .count();
            if loads < 2 || removed < roots.len() * 2 + 4 {
                continue;
            }
            return Some(Plan {
                header: header.id,
                entry: function.blocks[entry].id,
                index,
                nodes,
                order,
                roots,
                needed,
            });
        }
    }
    None
}

fn temporary(function: &mut MFunction) -> VReg {
    let value = function.vregs.alloc();
    function.spill_descs.push(SpillDesc::transient());
    value
}

pub(crate) fn run(function: &mut MFunction) {
    for _ in 0..16 {
        let Some(plan) = find_plan(function) else {
            break;
        };
        let value_count = function.value_count();
        while (function.vregs.count() as usize) < value_count {
            function.vregs.alloc();
        }
        function
            .spill_descs
            .resize_with(value_count, SpillDesc::transient);
        let mut values = BTreeMap::new();
        let mut setup = Vec::new();
        for &value in &plan.order {
            if !plan.needed.contains(&value) {
                continue;
            }
            let dst = temporary(function);
            let inst = match plan.nodes[&value].expression {
                Expression::Load(offset, size) => MInst::Load {
                    dst,
                    base: BaseReg::SimState,
                    offset,
                    size,
                },
                Expression::Copy(src) => MInst::Mov {
                    dst,
                    src: values[&src],
                },
                Expression::Mask(src, mask) => MInst::AndImm {
                    dst,
                    src: values[&src],
                    imm: u64::from(mask) * 0x0101_0101_0101_0101,
                },
                Expression::Not(src) => {
                    let mask = temporary(function);
                    setup.push(MInst::LoadImm {
                        dst: mask,
                        value: 0x0101_0101_0101_0101,
                    });
                    MInst::Xor {
                        dst,
                        lhs: values[&src],
                        rhs: mask,
                    }
                }
                Expression::Binary(lhs, rhs, kind) => match kind {
                    0 => MInst::And {
                        dst,
                        lhs: values[&lhs],
                        rhs: values[&rhs],
                    },
                    1 => MInst::Or {
                        dst,
                        lhs: values[&lhs],
                        rhs: values[&rhs],
                    },
                    _ => MInst::Xor {
                        dst,
                        lhs: values[&lhs],
                        rhs: values[&rhs],
                    },
                },
            };
            setup.push(inst);
            values.insert(value, dst);
        }
        let count = temporary(function);
        let mut replacements = BTreeMap::new();
        for &root in &plan.roots {
            let shifted = temporary(function);
            replacements.insert(
                root,
                vec![
                    MInst::Shr {
                        dst: shifted,
                        lhs: values[&root],
                        rhs: count,
                    },
                    MInst::AndImm32 {
                        dst: root,
                        src: shifted,
                        imm: 1,
                    },
                ],
            );
        }
        for block in &mut function.blocks {
            if block.id == plan.entry {
                let at = block.insts.len() - 1;
                block.insts.splice(at..at, std::mem::take(&mut setup));
            }
            if block.id == plan.header {
                block.insts.insert(
                    0,
                    MInst::ShlImm {
                        dst: count,
                        src: plan.index,
                        imm: 3,
                    },
                );
            }
            let mut insts = Vec::with_capacity(block.insts.len());
            for inst in std::mem::take(&mut block.insts) {
                if let Some(replacement) = inst.def().and_then(|dst| replacements.remove(&dst)) {
                    insts.extend(replacement);
                } else {
                    insts.push(inst);
                }
            }
            block.insts = insts;
        }
        propagate_exact_copies(function);
        dead_code_eliminate(function);
    }
}
