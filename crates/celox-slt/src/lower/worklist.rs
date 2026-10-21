//! Heap continuations for expression descent. Registers travel with the
//! continuation rather than through the node cache: fold-local bindings do
//! not permit that cache, and CFG arm values must not escape their block.

use super::*;
use celox_sir::BlockId;

enum Frame {
    Visit(NodeId),
    Cache(NodeId),
    VisitSlice(NodeId, BitAccess),
    CacheSlice(NodeId, BitAccess),
    SliceRegister(BitAccess),
    SliceBinaryLeft(NodeId, BinaryOp, BitAccess),
    SliceBinaryRight(BinaryOp, BitAccess, RegisterId),
    BinaryLeft(NodeId, NodeId, BinaryOp, NodeId),
    BinaryRight(NodeId, NodeId, BinaryOp, NodeId, RegisterId),
    Wildcard(NodeId, BinaryOp, NodeId),
    Unary(NodeId, UnaryOp, usize),
    Cast(usize),
    MuxCondition {
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        width: usize,
        access: Option<BitAccess>,
    },
    MuxThen {
        cond: RegisterId,
        else_expr: NodeId,
        width: usize,
        access: Option<BitAccess>,
    },
    MuxElse {
        cond: RegisterId,
        then_value: RegisterId,
        width: usize,
    },
    CfgThen {
        result: RegisterId,
        else_expr: NodeId,
        else_block: BlockId,
        merge_block: BlockId,
        width: usize,
        transaction: LowerCacheTransaction,
        access: Option<BitAccess>,
    },
    CfgElse {
        result: RegisterId,
        merge_block: BlockId,
        width: usize,
        transaction: LowerCacheTransaction,
    },
}

impl Frame {
    fn arm(node: NodeId, access: Option<BitAccess>) -> Self {
        match access {
            Some(access) => Self::VisitSlice(node, access),
            None => Self::Visit(node),
        }
    }
}

impl SLTToSIRLowerer {
    pub(super) fn lower_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        root: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        self.lower_worklist(builder, Frame::Visit(root), arena, cache, env, allow_cache)
    }

    fn lower_worklist<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        request: Frame,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        let mut work = vec![request];
        let mut value = None;
        while let Some(frame) = work.pop() {
            match frame {
                Frame::Visit(node) => {
                    if allow_cache && let Some(&reg) = cache.get(&node) {
                        value = Some(reg);
                        continue;
                    }
                    if allow_cache {
                        work.push(Frame::Cache(node));
                    }
                    if env.is_none()
                        && let Some(reg) =
                            self.try_lower_count_idiom(builder, node, arena, cache, allow_cache)
                    {
                        value = Some(reg);
                        continue;
                    }
                    match arena.get(node) {
                        SLTNode::Binary(lhs, op, rhs)
                            if !self.four_state
                                && matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard)
                                && wildcard_pattern(*rhs, arena).is_some() =>
                        {
                            work.push(Frame::Wildcard(node, *op, *rhs));
                            work.push(Frame::Visit(*lhs));
                        }
                        SLTNode::Binary(lhs, op, rhs) => {
                            work.push(Frame::BinaryLeft(node, *lhs, *op, *rhs));
                            work.push(Frame::Visit(*lhs));
                        }
                        SLTNode::Unary(op, inner) => {
                            work.push(Frame::Unary(node, *op, self.get_width(node, arena)));
                            work.push(Frame::Visit(*inner));
                        }
                        SLTNode::Capture { expr, .. } => work.push(Frame::Visit(*expr)),
                        SLTNode::Slice { expr, access } => {
                            work.push(Frame::VisitSlice(*expr, *access))
                        }
                        SLTNode::Mux {
                            cond,
                            then_expr,
                            else_expr,
                        } => {
                            self.with_mux_stats(|stats| stats.normal_seen += 1);
                            let width = self
                                .get_width(*then_expr, arena)
                                .max(self.get_width(*else_expr, arena));
                            if let Some(take_then) = Self::constant_condition(*cond, arena) {
                                self.with_mux_stats(|stats| stats.constant_folded += 1);
                                work.push(Frame::Cast(width));
                                work.push(Frame::Visit(if take_then {
                                    *then_expr
                                } else {
                                    *else_expr
                                }));
                            } else {
                                work.push(Frame::MuxCondition {
                                    cond: *cond,
                                    then_expr: *then_expr,
                                    else_expr: *else_expr,
                                    width,
                                    access: None,
                                });
                                work.push(Frame::Visit(*cond));
                            }
                        }
                        _ => {
                            value =
                                Some(self.lower_leaf(builder, node, arena, cache, env, allow_cache))
                        }
                    }
                }
                Frame::VisitSlice(expr, access) => {
                    // A previously materialized full value is a snapshot:
                    // projecting it must not reload memory after a Store.
                    if allow_cache && let Some(&full) = cache.get(&expr) {
                        value = Some(
                            if access.lsb == 0 && access.msb + 1 == self.get_width(expr, arena) {
                                full
                            } else {
                                self.slice_reg(builder, full, &access)
                            },
                        );
                        continue;
                    }
                    if let Some((constant, mask)) =
                        self.projected_mask_constant(expr, access, arena)
                    {
                        let width = access.msb - access.lsb + 1;
                        let result = if mask.is_zero() {
                            builder.alloc_bit(width, false)
                        } else {
                            builder.alloc_logic(width)
                        };
                        builder.emit(SIRInstruction::Imm(
                            result,
                            SIRValue::new_four_state(constant, mask),
                        ));
                        value = Some(result);
                        continue;
                    }
                    // Neither annihilated operand may be evaluated; a dead
                    // operand can divide by zero or read loop-carried state.
                    if let SLTNode::Binary(lhs, BinaryOp::And, rhs) = arena.get(expr)
                        && (self
                            .projected_mask_constant(*lhs, access, arena)
                            .is_some_and(|(value, mask)| value.is_zero() && mask.is_zero())
                            || self
                                .projected_mask_constant(*rhs, access, arena)
                                .is_some_and(|(value, mask)| value.is_zero() && mask.is_zero())
                            || slt_const_u64(*lhs, arena) == Some(0)
                            || slt_const_u64(*rhs, arena) == Some(0))
                    {
                        let result = builder.alloc_bit(access.msb - access.lsb + 1, false);
                        builder.emit(SIRInstruction::Imm(result, SIRValue::new(0u8)));
                        value = Some(result);
                        continue;
                    }
                    let cache_projection = allow_cache
                        && env.is_none()
                        && self
                            .cost_cache
                            .borrow()
                            .fanout
                            .get(expr.0)
                            .is_some_and(|fanout| *fanout > 1)
                        && Self::is_nontrivial_node(expr, arena);
                    if cache_projection {
                        if let Some(&reg) = self.region_slice_cache.borrow().get(&(expr, access)) {
                            value = Some(reg);
                            continue;
                        }
                        work.push(Frame::CacheSlice(expr, access));
                    }
                    match arena.get(expr) {
                        SLTNode::Input {
                            variable,
                            index,
                            access: input_access,
                            ..
                        } if access.msb <= input_access.msb - input_access.lsb => {
                            let composed = BitAccess::new(
                                input_access.lsb + access.lsb,
                                input_access.lsb + access.msb,
                            );
                            let bound = env.and_then(|env| {
                                self.lookup_override(
                                    builder, expr, arena, cache, env, variable, index, &composed,
                                )
                            });
                            value = Some(bound.unwrap_or_else(|| {
                                self.lower_input_for_node(
                                    builder, expr, variable, index, &composed, arena, cache, env,
                                )
                            }));
                        }
                        SLTNode::Slice {
                            expr: inner,
                            access: inner_access,
                        } if access.msb <= inner_access.msb - inner_access.lsb => {
                            work.push(Frame::VisitSlice(
                                *inner,
                                BitAccess::new(
                                    inner_access.lsb + access.lsb,
                                    inner_access.lsb + access.msb,
                                ),
                            ));
                        }
                        SLTNode::Binary(lhs, op, rhs)
                            if (matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Xor)
                                || (access.lsb == 0
                                    && matches!(
                                        op,
                                        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul
                                    )))
                                && access.msb < self.get_width(*lhs, arena)
                                && access.msb < self.get_width(*rhs, arena) =>
                        {
                            work.push(Frame::SliceBinaryLeft(*rhs, *op, access));
                            work.push(Frame::VisitSlice(*lhs, access));
                        }
                        SLTNode::Unary(
                            op @ (UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::BitNot),
                            inner,
                        ) if access.msb < self.get_width(*inner, arena) => {
                            work.push(Frame::Unary(expr, *op, access.msb - access.lsb + 1));
                            work.push(Frame::VisitSlice(*inner, access));
                        }
                        SLTNode::Mux {
                            cond,
                            then_expr,
                            else_expr,
                        } if access.msb < self.get_width(*then_expr, arena)
                            && access.msb < self.get_width(*else_expr, arena) =>
                        {
                            self.with_mux_stats(|stats| stats.slice_seen += 1);
                            if let Some(take_then) = Self::constant_condition(*cond, arena) {
                                self.with_mux_stats(|stats| stats.constant_folded += 1);
                                work.push(Frame::VisitSlice(
                                    if take_then { *then_expr } else { *else_expr },
                                    access,
                                ));
                            } else {
                                work.push(Frame::MuxCondition {
                                    cond: *cond,
                                    then_expr: *then_expr,
                                    else_expr: *else_expr,
                                    width: access.msb - access.lsb + 1,
                                    access: Some(access),
                                });
                                work.push(Frame::Visit(*cond));
                            }
                        }
                        _ => {
                            work.push(Frame::SliceRegister(access));
                            work.push(Frame::Visit(expr));
                        }
                    }
                }
                Frame::CacheSlice(node, access) => {
                    let key = (node, access);
                    let previous = self
                        .region_slice_cache
                        .borrow_mut()
                        .insert(key, value.expect("projection produces a register"));
                    debug_assert!(previous.is_none());
                    if previous.is_none() {
                        self.region_slice_cache_insert_log.borrow_mut().push(key);
                    }
                }
                Frame::SliceRegister(access) => {
                    value = Some(self.slice_reg(builder, value.take().unwrap(), &access));
                }
                Frame::SliceBinaryLeft(rhs, op, access) => {
                    work.push(Frame::SliceBinaryRight(op, access, value.take().unwrap()));
                    work.push(Frame::VisitSlice(rhs, access));
                }
                Frame::SliceBinaryRight(op, access, lhs) => {
                    let result = builder.alloc_logic(access.msb - access.lsb + 1);
                    builder.emit(SIRInstruction::Binary(
                        result,
                        lhs,
                        op,
                        value.take().unwrap(),
                    ));
                    value = Some(result);
                }
                Frame::Cache(node) => {
                    let reg = value.expect("lowered node produces a register");
                    let previous = cache.insert(node, reg);
                    debug_assert!(previous.is_none());
                    self.cache_insert_log.borrow_mut().push(node);
                }
                Frame::BinaryLeft(node, lhs, op, rhs) => {
                    work.push(Frame::BinaryRight(
                        node,
                        lhs,
                        op,
                        rhs,
                        value.take().unwrap(),
                    ));
                    work.push(Frame::Visit(rhs));
                }
                Frame::BinaryRight(node, lhs, op, rhs, left) => {
                    value = Some(self.emit_binary(
                        builder,
                        node,
                        &lhs,
                        &op,
                        &rhs,
                        left,
                        value.take().unwrap(),
                        arena,
                    ));
                }
                Frame::Wildcard(node, op, rhs) => {
                    value = Some(self.emit_wildcard_binary(
                        builder,
                        node,
                        &op,
                        &rhs,
                        value.take().unwrap(),
                        arena,
                    ));
                }
                Frame::Unary(node, op, width) => {
                    let inner = value.take().unwrap();
                    let result = if matches!(op, UnaryOp::ToTwoState | UnaryOp::Float { .. }) {
                        builder.alloc_bit(width, self.get_bound_signed(node, arena))
                    } else {
                        builder.alloc_logic(width)
                    };
                    builder.emit(SIRInstruction::Unary(result, op, inner));
                    value = Some(result);
                }
                Frame::Cast(width) => {
                    value = Some(self.cast_reg_width(builder, value.take().unwrap(), width));
                }
                Frame::MuxCondition {
                    cond,
                    then_expr,
                    else_expr,
                    width,
                    access,
                } => {
                    let cond_reg = value.take().unwrap();
                    let plan = if let Some(access) = &access {
                        self.mux_slice_cfg_plan(
                            cond,
                            then_expr,
                            else_expr,
                            access,
                            arena,
                            cache,
                            allow_cache,
                        )
                    } else {
                        self.mux_cfg_plan(
                            cond,
                            then_expr,
                            else_expr,
                            width,
                            arena,
                            cache,
                            allow_cache,
                        )
                    };
                    if let Some(plan) = plan {
                        self.hoist_shared_mux_nodes(builder, &plan, arena, cache, env, allow_cache);
                        let result = builder.alloc_logic(width);
                        let then_block = builder.new_block();
                        let else_block = builder.new_block();
                        let merge_block = builder.new_block_with(vec![result]);
                        builder.seal_block(SIRTerminator::Branch {
                            cond: cond_reg,
                            true_block: (then_block, vec![]),
                            false_block: (else_block, vec![]),
                        });
                        let transaction = self.cache_transaction();
                        builder.switch_to_block(then_block);
                        work.push(Frame::CfgThen {
                            result,
                            else_expr,
                            else_block,
                            merge_block,
                            width,
                            transaction,
                            access,
                        });
                        work.push(Frame::arm(then_expr, access));
                    } else {
                        work.push(Frame::MuxThen {
                            cond: cond_reg,
                            else_expr,
                            width,
                            access,
                        });
                        work.push(Frame::arm(then_expr, access));
                    }
                }
                Frame::MuxThen {
                    cond,
                    else_expr,
                    width,
                    access,
                } => {
                    let then_value = self.cast_reg_width(builder, value.take().unwrap(), width);
                    work.push(Frame::MuxElse {
                        cond,
                        then_value,
                        width,
                    });
                    work.push(Frame::arm(else_expr, access));
                }
                Frame::MuxElse {
                    cond,
                    then_value,
                    width,
                } => {
                    let else_value = self.cast_reg_width(builder, value.take().unwrap(), width);
                    let result = builder.alloc_logic(width);
                    builder.emit(SIRInstruction::Mux(result, cond, then_value, else_value));
                    value = Some(result);
                }
                Frame::CfgThen {
                    result,
                    else_expr,
                    else_block,
                    merge_block,
                    width,
                    transaction,
                    access,
                } => {
                    let then_value = self.cast_reg_width(builder, value.take().unwrap(), width);
                    builder.seal_block(SIRTerminator::Jump(merge_block, vec![then_value]));
                    if allow_cache {
                        self.rollback_cache(cache, transaction);
                    }
                    let transaction = self.cache_transaction();
                    builder.switch_to_block(else_block);
                    work.push(Frame::CfgElse {
                        result,
                        merge_block,
                        width,
                        transaction,
                    });
                    work.push(Frame::arm(else_expr, access));
                }
                Frame::CfgElse {
                    result,
                    merge_block,
                    width,
                    transaction,
                } => {
                    let else_value = self.cast_reg_width(builder, value.take().unwrap(), width);
                    builder.seal_block(SIRTerminator::Jump(merge_block, vec![else_value]));
                    if allow_cache {
                        self.rollback_cache(cache, transaction);
                    }
                    builder.switch_to_block(merge_block);
                    value = Some(result);
                }
            }
        }
        value.expect("root produces a register")
    }
}
