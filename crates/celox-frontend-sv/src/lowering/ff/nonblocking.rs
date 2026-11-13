//! Nonblocking assignments in a process kernel or a function unit (IEEE
//! 1800-2023 10.4.2): each is a site whose update the runtime queues for the
//! NBA region.

use super::*;

/// A part of a nonblocking assignment in a process: it takes `width` bits
/// of the right-hand side and writes them into `id` as `place` says.
pub(super) struct NbaTarget {
    pub(super) id: SourceVarId,
    pub(super) width: usize,
    /// For a slice of an unpacked array whose range runs against the
    /// array's, the element width: the part's elements are written in
    /// reverse order.
    pub(super) reversed: Option<usize>,
    pub(super) place: NbaPlace,
}

pub(super) enum NbaPlace {
    /// A constant position: the part's bits from `drop` up, `width` of them,
    /// are written at bit `lsb`. A position outside the target writes none.
    Static {
        lsb: usize,
        width: usize,
        drop: usize,
    },
    /// A position evaluated at run time into the variables of `position`,
    /// an offset from bit `lsb`. The part is shifted down first by the
    /// value of the variable `drop`, the bits that fall below the target.
    Dynamic {
        lsb: usize,
        position: NbaPosition<SourceVarId>,
        drop: Option<String>,
    },
}

impl Ff<'_, '_> {
    /// A nonblocking assignment in a process (IEEE 1800-2023 10.4.2): the
    /// positions of the targets and the right-hand side are evaluated now,
    /// once, and each target part becomes an update the runtime queues for
    /// the NBA region.
    pub(super) fn nonblocking(
        &mut self,
        parts: &[sv::ir::LValue],
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        self.nonblocking_delayed(parts, rhs, 0)
    }

    pub(super) fn nonblocking_delayed(
        &mut self,
        parts: &[sv::ir::LValue],
        rhs: &sv::ir::Expr,
        delay: u64,
    ) -> Result<(), sv::AnalyzerError> {
        let mut targets = Vec::with_capacity(parts.len());
        for part in parts {
            let part = self.propagate_lvalue(part);
            let part = self.hoist_lvalue(&part)?;
            targets.push(self.nba_target(&part)?);
        }
        let total: usize = targets.iter().map(|target| target.width).sum();
        let four_state = targets.iter().any(|target| self.m.var(target.id).is_4state);
        let (_, value_name) = self.m.temp("nba", total, false, four_state);
        self.assign(&sv::ir::LValue::Ident(value_name.clone()), rhs)?;
        // The parts, most significant first, take consecutive bits of the
        // value.
        let mut high = total;
        for target in targets {
            let low = high - target.width;
            high = low;
            let id = target.id;
            let select = |name: &str, msb: usize, lsb: usize| sv::ir::Expr::Select {
                expr: Box::new(sv::ir::Expr::Ident(name.to_string())),
                msb: sv::ir::ConstExpr::Literal(msb.to_string()),
                lsb: sv::ir::ConstExpr::Literal(lsb.to_string()),
                signed: false,
            };
            let (source, low) = match target.reversed {
                Some(element_width) => {
                    let (_, name) = self.m.temp("nba_order", target.width, false, four_state);
                    let elements = (0..target.width / element_width)
                        .map(|element| {
                            let bottom = low + element * element_width;
                            select(&value_name, bottom + element_width - 1, bottom)
                        })
                        .collect();
                    self.assign(
                        &sv::ir::LValue::Ident(name.clone()),
                        &sv::ir::Expr::Concat(elements),
                    )?;
                    (name, 0)
                }
                None => (value_name.clone(), low),
            };
            let part = |msb: usize, lsb: usize| select(&source, msb, lsb);
            let (lsb, width, value, position) = match target.place {
                NbaPlace::Static { lsb, width, drop } => {
                    if width == 0 {
                        continue;
                    }
                    let from = low + drop;
                    (lsb, width, part(from + width - 1, from), None)
                }
                NbaPlace::Dynamic {
                    lsb,
                    position,
                    drop,
                } => {
                    let value = part(low + target.width - 1, low);
                    let value = match drop {
                        Some(drop) => sv::ir::Expr::Binary {
                            left: Box::new(value),
                            op: sv::ir::BinaryOp::Shr,
                            right: Box::new(sv::ir::Expr::Ident(drop)),
                        },
                        None => value,
                    };
                    (lsb, target.width, value, Some(position))
                }
            };
            let (staged, staged_name) =
                self.m
                    .temp("nba_part", width, false, self.m.var(id).is_4state);
            self.assign(&sv::ir::LValue::Ident(staged_name), &value)?;
            // A unit stages the update in the objects of one of the
            // module's function NBA sites, which the runtime reads as the
            // unit suspends, as the kernel's own sites without a pending
            // flag are read (see `FunctionSites::nba`).
            if self.kernel.is_none() {
                self.unit_nonblocking(delay, id, lsb, width, staged, position)?;
                continue;
            }
            // The staged value and position have objects of their own: a
            // value still staged from earlier in the time slot is queued
            // before they are replaced. Assembly keeps the staged or the
            // queued path.
            let staged_four_state = self.m.var(staged).is_4state;
            let (value, _) = self.m.temp("nba_value", width, false, staged_four_state);
            let (pending, _) = self.m.temp("nba_pending", 1, false, false);
            let (switch, _) = self.m.temp("nba_staged", 1, false, false);
            let slots = position.map(|_| NbaPosition {
                offset: self
                    .m
                    .temp("nba_offset", NBA_POSITION_WIDTH, false, false)
                    .0,
                width: self.m.temp("nba_width", NBA_POSITION_WIDTH, false, false).0,
            });
            let site = NbaSite {
                delay,
                process: self.sites.process,
                target: id,
                lsb,
                width,
                value,
                position: slots,
                pending: Some(pending),
                unit: false,
            };
            let index = self.sites.nba.len() as u32;
            self.with_kernel(|kernel| {
                kernel.nonblocking_switched(index, &site, switch, |builder| {
                    let load = |builder: &mut SIRBuilder<_>, var, width, four_state| {
                        let register = if four_state {
                            builder.alloc_logic(width)
                        } else {
                            builder.alloc_bit(width, false)
                        };
                        builder.emit(SIRInstruction::Load(
                            register,
                            stable(var),
                            SIROffset::Static(0),
                            width,
                        ));
                        register
                    };
                    let value = load(builder, staged, width, staged_four_state);
                    let position = position.map(|position: NbaPosition<SourceVarId>| {
                        position.map(|var| load(builder, var, NBA_POSITION_WIDTH, false))
                    });
                    Ok::<_, ProcessKernelError>((value, position))
                })
            })
            .map_err(kernel_error)?;
            self.sites.nba_switches.push(NbaSwitch {
                value,
                switch,
                queued: staged,
                queued_position: position,
            });
            self.sites.nba.push(site);
        }
        Ok(())
    }

    /// The variable a part of a nonblocking assignment in a process writes
    /// and where. A position is evaluated now; bits outside the target, and
    /// a position with an unknown bit, write nothing (IEEE 1800-2023 7.4.6,
    /// 11.5.1).
    pub(super) fn nba_target(
        &mut self,
        lvalue: &sv::ir::LValue,
    ) -> Result<NbaTarget, sv::AnalyzerError> {
        let id = self
            .m
            .id(lvalue.name())
            .ok_or_else(|| unsupported(format!("assignment target `{}`", lvalue.name())))?;
        let variable = self.m.var(id);
        let var_width = variable.width;
        let (msb, lsb) = match lvalue {
            sv::ir::LValue::Ident(_) => {
                return Ok(NbaTarget {
                    id,
                    width: var_width,
                    reversed: None,
                    place: NbaPlace::Static {
                        lsb: 0,
                        width: var_width,
                        drop: 0,
                    },
                });
            }
            sv::ir::LValue::Select { msb, lsb, .. } => (msb, lsb),
        };
        let constants = self.m.constants;
        let parameter_types = self.m.parameter_types;
        let constant =
            |expr| sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types);
        // A slice of an unpacked array whose range runs against the array's
        // takes the elements of the value in reverse order.
        let reversed = match lvalue {
            sv::ir::LValue::Select {
                array_slice_width: Some(element_width),
                array_slice_reversed: true,
                ..
            } => Some(
                constant(element_width)
                    .and_then(|width| usize::try_from(width).ok())
                    .filter(|width| *width != 0)
                    .ok_or_else(|| unsupported("assignment lvalue order"))?,
            ),
            _ => None,
        };
        if let (Some(msb), Some(lsb)) = (constant(msb), constant(lsb)) {
            let (Some(msb), Some(lsb)) = (
                packed_index_position(variable, msb),
                packed_index_position(variable, lsb),
            ) else {
                return Err(unsupported("nonblocking assignment position"));
            };
            if msb < lsb {
                return Err(unsupported(
                    "nonblocking assignment to a reversed part select in a process",
                ));
            }
            let width = usize::try_from(msb - lsb + 1)
                .map_err(|_| unsupported("nonblocking assignment position"))?;
            let top = i128::try_from(var_width).unwrap_or(i128::MAX);
            let start = lsb.clamp(0, top);
            let end = (msb + 1).clamp(0, top);
            let (start, written) = if start < end {
                (start, end - start)
            } else {
                (0, 0)
            };
            if reversed.is_some_and(|element_width| !width.is_multiple_of(element_width)) {
                return Err(unsupported("assignment lvalue order"));
            }
            return Ok(NbaTarget {
                id,
                width,
                reversed,
                place: NbaPlace::Static {
                    lsb: start as usize,
                    width: written as usize,
                    drop: if written == 0 {
                        0
                    } else {
                        (start - lsb) as usize
                    },
                },
            });
        }
        if reversed.is_some() {
            return Err(unsupported(
                "nonblocking assignment to a reversed array slice at a run-time position",
            ));
        }
        let mut arena = SLTNodeArena::new();
        if let Some((_, element_width, offset, access)) = dynamic_array_element_lvalue(
            lvalue,
            self.m.variables,
            self.m.name_to_id,
            constants,
            parameter_types,
        ) {
            // A run-time element: its whole selected part or nothing.
            let width = access.msb - access.lsb + 1;
            let offset = expr_from_select_position(&offset, constants, parameter_types)
                .ok_or_else(|| unsupported("dynamic assignment index"))?;
            let offset = self.propagate(&offset);
            let (bit_offset, _) = lower_expr_with_context(
                &offset,
                self.m.variables,
                self.m.name_to_id,
                constants,
                parameter_types,
                &mut arena,
                None,
                None,
            )
            .ok_or_else(|| unsupported("dynamic assignment index"))?;
            let bit_offset =
                coerce_node_width(&mut arena, bit_offset, Some(NBA_POSITION_WIDTH), false)
                    .map_err(slt_error)?;
            let stride = slt_constant(&mut arena, BigUint::from(element_width), 64, false)?;
            let index = arena
                .alloc(SLTNode::Binary(bit_offset, BinaryOp::DivU, stride))
                .map_err(slt_error)?;
            let (index, valid) =
                dynamic_array_index_guard_slt(&mut arena, index, var_width / element_width.max(1))
                    .ok_or_else(|| unsupported("dynamic assignment index"))?;
            let offset = arena
                .alloc(SLTNode::Binary(index, BinaryOp::Mul, stride))
                .map_err(slt_error)?;
            let full = slt_constant(&mut arena, BigUint::from(width), 64, false)?;
            let zero = slt_constant(&mut arena, BigUint::zero(), 64, false)?;
            let written = arena
                .alloc(SLTNode::Mux {
                    cond: valid,
                    then_expr: full,
                    else_expr: zero,
                })
                .map_err(slt_error)?;
            let position = NbaPosition {
                offset: self.nba_position_temp("nba_at", &arena, offset)?,
                width: self.nba_position_temp("nba_bits", &arena, written)?,
            };
            return Ok(NbaTarget {
                id,
                width,
                reversed: None,
                place: NbaPlace::Dynamic {
                    lsb: access.lsb,
                    position,
                    drop: None,
                },
            });
        }
        let write = dynamic_packed_write(
            lvalue,
            self.m.variables,
            self.m.name_to_id,
            constants,
            parameter_types,
        )
        .ok_or_else(|| unsupported(format!("assignment target `{}`", lvalue.name())))?;
        // A run-time part select: the bits of it inside the target, or
        // inside the array element it stays within.
        let shift = |expr: &sv::ir::Expr, arena: &mut SLTNodeArena<SourceVarId>| {
            let (node, _) = lower_expr_with_context(
                &self.propagate(expr),
                self.m.variables,
                self.m.name_to_id,
                constants,
                parameter_types,
                arena,
                None,
                Some(false),
            )
            .ok_or_else(|| unsupported("dynamic assignment position"))?;
            coerce_node_width(arena, node, Some(NBA_POSITION_WIDTH), false).map_err(slt_error)
        };
        let up = shift(&write.up, &mut arena)?;
        let down = shift(&write.down, &mut arena)?;
        let window = write
            .window
            .unwrap_or_else(|| BitAccess::new(0, var_width - 1));
        let binary = |arena: &mut SLTNodeArena<SourceVarId>, left, op, right| {
            arena
                .alloc(SLTNode::Binary(left, op, right))
                .map_err(slt_error)
        };
        let mux = |arena: &mut SLTNodeArena<SourceVarId>, cond, then_expr, else_expr| {
            arena
                .alloc(SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                })
                .map_err(slt_error)
        };
        let constant = |arena: &mut SLTNodeArena<SourceVarId>, value: usize| {
            slt_constant(arena, BigUint::from(value), NBA_POSITION_WIDTH, false)
        };
        // The select's bit 0 sits at `up - down`, which may be below the
        // bottom of the window; the comparisons are signed.
        let at = binary(&mut arena, up, BinaryOp::Sub, down)?;
        // A window at a run-time position spans `width` bits above its
        // bottom there.
        let moving = match &write.moving {
            Some(moving) => Some((shift(&moving.at, &mut arena)?, moving.lsb, moving.width)),
            None => None,
        };
        let (bottom, top) = match moving {
            Some((element, lsb, width)) => {
                let lsb = constant(&mut arena, lsb)?;
                let bottom = binary(&mut arena, element, BinaryOp::Add, lsb)?;
                let width = constant(&mut arena, width)?;
                let top = binary(&mut arena, bottom, BinaryOp::Add, width)?;
                // An element past the array has its window past the variable,
                // where nothing is written.
                let limit = constant(&mut arena, var_width)?;
                let past = binary(&mut arena, top, BinaryOp::GtS, limit)?;
                (bottom, mux(&mut arena, past, limit, top)?)
            }
            None => (
                constant(&mut arena, window.lsb)?,
                constant(&mut arena, window.msb + 1)?,
            ),
        };
        let select_width = constant(&mut arena, write.select_width)?;
        let below = binary(&mut arena, at, BinaryOp::LtS, bottom)?;
        let start = mux(&mut arena, below, bottom, at)?;
        let reach = binary(&mut arena, at, BinaryOp::Add, select_width)?;
        let above = binary(&mut arena, reach, BinaryOp::GtS, top)?;
        let end = mux(&mut arena, above, top, reach)?;
        let mut valid = binary(&mut arena, start, BinaryOp::LtS, end)?;
        if self.m.four_state {
            for position in [up, down]
                .into_iter()
                .chain(moving.map(|(element, _, _)| element))
            {
                let two_state = arena
                    .alloc(SLTNode::Unary(UnaryOp::ToTwoState, position))
                    .map_err(slt_error)?;
                let known = binary(&mut arena, position, BinaryOp::EqCase, two_state)?;
                valid = slt_and(&mut arena, &mut ConstCache::default(), valid, known)?;
            }
        }
        let zero = constant(&mut arena, 0)?;
        let span = binary(&mut arena, end, BinaryOp::Sub, start)?;
        let written = mux(&mut arena, valid, span, zero)?;
        let offset = mux(&mut arena, valid, start, zero)?;
        let skipped = binary(&mut arena, start, BinaryOp::Sub, at)?;
        let skipped = mux(&mut arena, valid, skipped, zero)?;
        let position = NbaPosition {
            offset: self.nba_position_temp("nba_at", &arena, offset)?,
            width: self.nba_position_temp("nba_bits", &arena, written)?,
        };
        let (drop, drop_name) = self.m.temp("nba_drop", NBA_POSITION_WIDTH, false, false);
        let skipped = self.lower_slt(&arena, skipped)?;
        self.store(drop, SIROffset::Static(0), NBA_POSITION_WIDTH, skipped);
        Ok(NbaTarget {
            id,
            width: write.select_width,
            reversed: None,
            place: NbaPlace::Dynamic {
                lsb: 0,
                position,
                drop: Some(drop_name),
            },
        })
    }

    /// A new two-state variable of [`NBA_POSITION_WIDTH`] bits holding the
    /// value of `node`, evaluated now.
    pub(super) fn nba_position_temp(
        &mut self,
        purpose: &str,
        arena: &SLTNodeArena<SourceVarId>,
        node: NodeId,
    ) -> Result<SourceVarId, sv::AnalyzerError> {
        let (temp, _) = self.m.temp(purpose, NBA_POSITION_WIDTH, false, false);
        let value = self.lower_slt(arena, node)?;
        self.store(temp, SIROffset::Static(0), NBA_POSITION_WIDTH, value);
        Ok(temp)
    }
}
