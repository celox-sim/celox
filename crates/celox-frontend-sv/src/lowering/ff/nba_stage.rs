//! Staging of the variables an edge-triggered process writes both by
//! blocking and by nonblocking assignments: the nonblocking value is kept
//! apart with a mask of the written bits, and merged when the nonblocking
//! results are published.

use super::*;

/// The staging of a variable a process writes both by blocking and by
/// nonblocking assignments: the nonblocking value and a mask of the bits
/// the nonblocking assignments wrote.
pub(super) struct NbaStage {
    pub(super) id: SourceVarId,
    pub(super) name: String,
    pub(super) value: SourceVarId,
    pub(super) value_name: String,
    pub(super) mask: SourceVarId,
    pub(super) mask_name: String,
    /// The intra-assignment delay of the assignments this stage captures;
    /// zero for an ordinary nonblocking assignment. A delayed stage is not
    /// merged at the edge: its commit hands the staged value to a timed
    /// nonblocking site (IEEE 1800-2023 9.4.5, 10.4.2).
    pub(super) delay: u64,
    /// For a delayed stage, the persistent (value, pending) objects of its
    /// timed nonblocking site, which publication queues for the due time.
    pub(super) site: Option<(SourceVarId, SourceVarId)>,
    /// For an unpacked array, the sites of its nonblocking writes, when each
    /// runs at most once an evaluation: the merge then publishes only the
    /// elements they wrote. Without them every element is merged.
    pub(super) sites: Option<Vec<NbaArraySite>>,
}

/// A nonblocking write of a staged unpacked array: the bit range it wrote,
/// in the array's flattened bits, and whether it ran.
pub(super) struct NbaArraySite {
    pub(super) lsb: SourceVarId,
    pub(super) lsb_name: String,
    pub(super) msb: SourceVarId,
    pub(super) msb_name: String,
    pub(super) hit: SourceVarId,
    pub(super) hit_name: String,
}

/// The number of nonblocking writes of the unpacked array `id` in `stmts`,
/// when each is a select [`stage_nonblocking`] stages and none is in a loop,
/// so that each runs at most once an evaluation.
pub(super) fn array_nba_site_count(
    stmts: &[sv::ir::Stmt],
    id: SourceVarId,
    pm: &ProcModule<'_>,
) -> Option<usize> {
    let writes = |stmt: &sv::ir::Stmt| {
        let mut found = false;
        stmt.walk(&mut |stmt| match stmt {
            sv::ir::Stmt::Assign {
                lhs,
                nonblocking: true,
                ..
            } => found |= pm.id(lhs.name()) == Some(id),
            sv::ir::Stmt::AssignConcat {
                parts,
                nonblocking: true,
                ..
            } => found |= parts.iter().any(|part| pm.id(part.name()) == Some(id)),
            _ => {}
        });
        found
    };
    let mut count = 0;
    for stmt in stmts {
        match stmt {
            sv::ir::Stmt::Assign {
                lhs,
                nonblocking: true,
                ..
            } if pm.id(lhs.name()) == Some(id) => match lhs {
                sv::ir::LValue::Select {
                    msb,
                    lsb,
                    array_slice_width: None,
                    ..
                } if expr_from_const_expr(msb).is_some() && expr_from_const_expr(lsb).is_some() => {
                    count += 1;
                }
                _ => return None,
            },
            sv::ir::Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                count += array_nba_site_count(then_body, id, pm)?;
                count += array_nba_site_count(else_body, id, pm)?;
            }
            sv::ir::Stmt::Case { items, default, .. } => {
                for item in items {
                    count += array_nba_site_count(&item.body, id, pm)?;
                }
                if let Some(default) = default {
                    count += array_nba_site_count(default, id, pm)?;
                }
            }
            sv::ir::Stmt::Block { body, .. } => {
                count += array_nba_site_count(body, id, pm)?;
            }
            other if writes(other) => return None,
            _ => {}
        }
    }
    Some(count)
}

/// `stmts` with every nonblocking assignment to a staged variable writing
/// its staging value instead, and marking the written bits in its mask.
pub(super) fn stage_nonblocking(
    stmts: &[sv::ir::Stmt],
    staged: &[NbaStage],
    pm: &ProcModule<'_>,
    next_site: &mut HashMap<SourceVarId, usize>,
    stmt_delays: &HashMap<usize, u64>,
) -> Vec<sv::ir::Stmt> {
    let stage_of_delay = |lvalue: &sv::ir::LValue, delay: u64| {
        staged
            .iter()
            .find(|stage| stage.delay == delay && pm.id(lvalue.name()) == Some(stage.id))
    };
    let stage_of = |lvalue: &sv::ir::LValue| stage_of_delay(lvalue, 0);
    let renamed = |lvalue: &sv::ir::LValue, name: &str| match lvalue {
        sv::ir::LValue::Ident(_) => sv::ir::LValue::Ident(name.to_string()),
        sv::ir::LValue::Select {
            msb,
            lsb,
            array_slice_width,
            array_slice_reversed,
            window,
            ..
        } => sv::ir::LValue::Select {
            name: name.to_string(),
            msb: msb.clone(),
            lsb: lsb.clone(),
            array_slice_width: array_slice_width.clone(),
            array_slice_reversed: *array_slice_reversed,
            window: window.clone(),
        },
    };
    let mark = |lvalue: &sv::ir::LValue, stage: &NbaStage| sv::ir::Stmt::Assign {
        lhs: renamed(lvalue, &stage.mask_name),
        rhs: sv::ir::Expr::Literal("'1".to_string()),
        nonblocking: true,
    };
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        match stmt {
            sv::ir::Stmt::Assign {
                lhs,
                rhs,
                nonblocking: true,
            } if stage_of(lhs).is_some() => {
                let stage = stage_of(lhs).unwrap();
                if let (Some(sites), sv::ir::LValue::Select { msb, lsb, .. }) = (&stage.sites, lhs)
                {
                    let index = next_site.entry(stage.id).or_default();
                    let site = &sites[*index];
                    *index += 1;
                    let record = |name: &str, rhs: sv::ir::Expr| sv::ir::Stmt::Assign {
                        lhs: sv::ir::LValue::Ident(name.to_string()),
                        rhs,
                        nonblocking: false,
                    };
                    out.push(record(
                        &site.lsb_name,
                        expr_from_const_expr(lsb).expect("a staged site's bounds are expressions"),
                    ));
                    out.push(record(
                        &site.msb_name,
                        expr_from_const_expr(msb).expect("a staged site's bounds are expressions"),
                    ));
                    out.push(record(
                        &site.hit_name,
                        sv::ir::Expr::Literal("1'b1".to_string()),
                    ));
                }
                out.push(sv::ir::Stmt::Assign {
                    lhs: renamed(lhs, &stage.value_name),
                    rhs: rhs.clone(),
                    nonblocking: true,
                });
                out.push(mark(lhs, stage));
            }
            sv::ir::Stmt::AssignConcat {
                parts,
                rhs,
                nonblocking: true,
            } if parts.iter().any(|part| stage_of(part).is_some()) => {
                out.push(sv::ir::Stmt::AssignConcat {
                    parts: parts
                        .iter()
                        .map(|part| match stage_of(part) {
                            Some(stage) => renamed(part, &stage.value_name),
                            None => part.clone(),
                        })
                        .collect(),
                    rhs: rhs.clone(),
                    nonblocking: true,
                });
                for part in parts {
                    if let Some(stage) = stage_of(part) {
                        out.push(mark(part, stage));
                    }
                }
            }
            sv::ir::Stmt::DelayedNonblocking { assignment, .. }
                if stmt_delays.contains_key(&(std::ptr::from_ref(stmt) as usize)) =>
            {
                let delay = stmt_delays[&(std::ptr::from_ref(stmt) as usize)];
                match assignment.as_ref() {
                    sv::ir::Stmt::Assign {
                        lhs,
                        rhs,
                        nonblocking: true,
                    } if stage_of_delay(lhs, delay).is_some() => {
                        let stage = stage_of_delay(lhs, delay).unwrap();
                        out.push(sv::ir::Stmt::Assign {
                            lhs: renamed(lhs, &stage.value_name),
                            rhs: rhs.clone(),
                            nonblocking: true,
                        });
                        out.push(mark(lhs, stage));
                    }
                    sv::ir::Stmt::AssignConcat {
                        parts,
                        rhs,
                        nonblocking: true,
                    } if parts
                        .iter()
                        .all(|part| stage_of_delay(part, delay).is_some()) =>
                    {
                        out.push(sv::ir::Stmt::AssignConcat {
                            parts: parts
                                .iter()
                                .map(|part| {
                                    renamed(part, &stage_of_delay(part, delay).unwrap().value_name)
                                })
                                .collect(),
                            rhs: rhs.clone(),
                            nonblocking: true,
                        });
                        for part in parts {
                            out.push(mark(part, stage_of_delay(part, delay).unwrap()));
                        }
                    }
                    _ => out.push(stmt.clone()),
                }
            }
            sv::ir::Stmt::If {
                condition,
                then_body,
                else_body,
            } => out.push(sv::ir::Stmt::If {
                condition: condition.clone(),
                then_body: stage_nonblocking(then_body, staged, pm, next_site, stmt_delays),
                else_body: stage_nonblocking(else_body, staged, pm, next_site, stmt_delays),
            }),
            sv::ir::Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => out.push(sv::ir::Stmt::Case {
                kind: *kind,
                selector: selector.clone(),
                items: items
                    .iter()
                    .map(|item| sv::ir::CaseItem {
                        labels: item.labels.clone(),
                        body: stage_nonblocking(&item.body, staged, pm, next_site, stmt_delays),
                    })
                    .collect(),
                default: default
                    .as_ref()
                    .map(|body| stage_nonblocking(body, staged, pm, next_site, stmt_delays)),
            }),
            sv::ir::Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => out.push(sv::ir::Stmt::Loop {
                kind: kind.clone(),
                init: init.clone(),
                condition: condition.clone(),
                step: step.clone(),
                body: stage_nonblocking(body, staged, pm, next_site, stmt_delays),
            }),
            sv::ir::Stmt::Block { id, body } => out.push(sv::ir::Stmt::Block {
                id: *id,
                body: stage_nonblocking(body, staged, pm, next_site, stmt_delays),
            }),
            other => out.push(other.clone()),
        }
    }
    out
}

/// Publish the staged nonblocking values: each variable takes its staging
/// value in the bits the mask marks, after its blocking value was
/// committed.
pub(super) fn emit_nba_merges(
    builder: &mut SIRBuilder<Addr>,
    staged: &[NbaStage],
    pm: &ProcModule<'_>,
) {
    for stage in staged {
        if stage.delay > 0 {
            continue;
        }
        let _ = &stage.name;
        let variable = pm.var(stage.id);
        if let Some(element_width) =
            unpacked_element_width(variable).filter(|_| !variable.array_dims.is_empty())
        {
            match &stage.sites {
                Some(sites) => {
                    for site in sites {
                        emit_array_site_merge(builder, stage, site, variable, element_width);
                    }
                }
                None => emit_array_nba_merge(builder, stage, variable, element_width),
            }
            continue;
        }
        let width = variable.width;
        let current = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Load(
            current,
            stable(stage.id),
            SIROffset::Static(0),
            width,
        ));
        let value = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Load(
            value,
            working(stage.value),
            SIROffset::Static(0),
            width,
        ));
        let mask = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Load(
            mask,
            working(stage.mask),
            SIROffset::Static(0),
            width,
        ));
        let keep = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Unary(keep, UnaryOp::BitNot, mask));
        let kept = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Binary(kept, current, BinaryOp::And, keep));
        let taken = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Binary(taken, value, BinaryOp::And, mask));
        let merged = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Binary(merged, kept, BinaryOp::Or, taken));
        builder.emit(SIRInstruction::Store(
            stable(stage.id),
            SIROffset::Static(0),
            width,
            merged,
            Vec::new(),
            Vec::new(),
        ));
    }
}

/// Hand every delayed stage that ran to its timed nonblocking site: the
/// staged value moves to the site's persistent object and its pending flag
/// is set, so publication queues the update for the due time instead of
/// landing it in this slot's NBA region (IEEE 1800-2023 9.4.5, 10.4.2).
pub(super) fn emit_delayed_publications(
    builder: &mut SIRBuilder<Addr>,
    staged: &[NbaStage],
    pm: &ProcModule<'_>,
) {
    for stage in staged {
        let Some((site_value, pending)) = stage.site else {
            continue;
        };
        let variable = pm.var(stage.id);
        let width = variable.width;
        let value = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Load(
            value,
            working(stage.value),
            SIROffset::Static(0),
            width,
        ));
        builder.emit(SIRInstruction::Store(
            stable(site_value),
            SIROffset::Static(0),
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
        let mask = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Load(
            mask,
            working(stage.mask),
            SIROffset::Static(0),
            width,
        ));
        let zero = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let ran = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Binary(ran, mask, BinaryOp::Ne, zero));
        let was = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Load(
            was,
            stable(pending),
            SIROffset::Static(0),
            1,
        ));
        let set = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Binary(set, was, BinaryOp::Or, ran));
        builder.emit(SIRInstruction::Store(
            stable(pending),
            SIROffset::Static(0),
            1,
            set,
            Vec::new(),
            Vec::new(),
        ));
    }
}

/// [`emit_nba_merges`] for one write site of an unpacked array: when it ran,
/// a loop merges the elements its bit range covers, and clears their mask
/// bits and its hit for the next evaluation. Another site that wrote the
/// same element merged it already, and then finds its mask clear.
pub(super) fn emit_array_site_merge(
    builder: &mut SIRBuilder<Addr>,
    stage: &NbaStage,
    site: &NbaArraySite,
    variable: &SvVariable,
    element_width: usize,
) {
    let count = variable.width / element_width;
    let hit = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Load(
        hit,
        working(site.hit),
        SIROffset::Static(0),
        1,
    ));
    let first = builder.new_block();
    let done = builder.new_block();
    let next = builder.new_block();
    builder.seal_block(SIRTerminator::Branch {
        cond: hit,
        true_block: (first, Vec::new()),
        false_block: (next, Vec::new()),
    });

    builder.switch_to_block(first);
    let load = |builder: &mut SIRBuilder<Addr>, id| {
        let value = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Load(
            value,
            working(id),
            SIROffset::Static(0),
            64,
        ));
        value
    };
    let lsb = load(builder, site.lsb);
    let msb = load(builder, site.msb);
    let reversed = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(reversed, msb, BinaryOp::LtU, lsb));
    let low = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Mux(low, reversed, msb, lsb));
    let high = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Mux(high, reversed, lsb, msb));
    let width = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(
        width,
        SIRValue::new(element_width as u64),
    ));
    let start = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Binary(start, low, BinaryOp::DivU, width));
    let end = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Binary(end, high, BinaryOp::DivU, width));
    let index = builder.alloc_bit(64, false);
    let header = builder.new_block_with(vec![index]);
    let body_index = builder.alloc_bit(64, false);
    let body = builder.new_block_with(vec![body_index]);
    builder.seal_block(SIRTerminator::Jump(header, vec![start]));

    builder.switch_to_block(header);
    let limit = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(limit, SIRValue::new(count as u64)));
    let inside = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(inside, index, BinaryOp::LtU, limit));
    let covered = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(covered, index, BinaryOp::LeU, end));
    let more = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(more, inside, BinaryOp::And, covered));
    builder.seal_block(SIRTerminator::Branch {
        cond: more,
        true_block: (body, vec![index]),
        false_block: (done, Vec::new()),
    });

    builder.switch_to_block(body);
    let offset = SIROffset::Element {
        index: body_index,
        element_width,
        bit_offset: 0,
        dynamic_bit_offset: None,
    };
    emit_element_merge(builder, stage, variable, element_width, &offset);
    let clear = builder.alloc_bit(element_width, false);
    builder.emit(SIRInstruction::Imm(clear, SIRValue::new(0u8)));
    builder.emit(SIRInstruction::Store(
        working(stage.mask),
        offset,
        element_width,
        clear,
        Vec::new(),
        Vec::new(),
    ));
    let one = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
    let following = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Binary(
        following,
        body_index,
        BinaryOp::Add,
        one,
    ));
    builder.seal_block(SIRTerminator::Jump(header, vec![following]));

    builder.switch_to_block(done);
    let unset = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Imm(unset, SIRValue::new(0u8)));
    builder.emit(SIRInstruction::Store(
        working(site.hit),
        SIROffset::Static(0),
        1,
        unset,
        Vec::new(),
        Vec::new(),
    ));
    builder.seal_block(SIRTerminator::Jump(next, Vec::new()));

    builder.switch_to_block(next);
}

/// Merge the staged nonblocking value of one element of an unpacked array
/// into its stable state, in the bits its mask marks.
pub(super) fn emit_element_merge(
    builder: &mut SIRBuilder<Addr>,
    stage: &NbaStage,
    variable: &SvVariable,
    element_width: usize,
    offset: &SIROffset,
) {
    let alloc = |builder: &mut SIRBuilder<Addr>| {
        if variable.is_4state {
            builder.alloc_logic(element_width)
        } else {
            builder.alloc_bit(element_width, false)
        }
    };
    let current = alloc(builder);
    builder.emit(SIRInstruction::Load(
        current,
        stable(stage.id),
        offset.clone(),
        element_width,
    ));
    let value = alloc(builder);
    builder.emit(SIRInstruction::Load(
        value,
        working(stage.value),
        offset.clone(),
        element_width,
    ));
    let mask = builder.alloc_bit(element_width, false);
    builder.emit(SIRInstruction::Load(
        mask,
        working(stage.mask),
        offset.clone(),
        element_width,
    ));
    let keep = builder.alloc_bit(element_width, false);
    builder.emit(SIRInstruction::Unary(keep, UnaryOp::BitNot, mask));
    let kept = alloc(builder);
    builder.emit(SIRInstruction::Binary(kept, current, BinaryOp::And, keep));
    let taken = alloc(builder);
    builder.emit(SIRInstruction::Binary(taken, value, BinaryOp::And, mask));
    let merged = alloc(builder);
    builder.emit(SIRInstruction::Binary(merged, kept, BinaryOp::Or, taken));
    builder.emit(SIRInstruction::Store(
        stable(stage.id),
        offset.clone(),
        element_width,
        merged,
        Vec::new(),
        Vec::new(),
    ));
}

/// [`emit_nba_merges`] for an unpacked array: a loop merges each element,
/// since an access may not span elements.
pub(super) fn emit_array_nba_merge(
    builder: &mut SIRBuilder<Addr>,
    stage: &NbaStage,
    variable: &SvVariable,
    element_width: usize,
) {
    let count = variable.width / element_width;
    let first = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(first, SIRValue::new(0u8)));
    let index = builder.alloc_bit(64, false);
    let header = builder.new_block_with(vec![index]);
    let body_index = builder.alloc_bit(64, false);
    let body = builder.new_block_with(vec![body_index]);
    let exit = builder.new_block();
    builder.seal_block(SIRTerminator::Jump(header, vec![first]));

    builder.switch_to_block(header);
    let limit = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(limit, SIRValue::new(count as u64)));
    let more = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(more, index, BinaryOp::LtU, limit));
    builder.seal_block(SIRTerminator::Branch {
        cond: more,
        true_block: (body, vec![index]),
        false_block: (exit, Vec::new()),
    });

    builder.switch_to_block(body);
    let offset = SIROffset::Element {
        index: body_index,
        element_width,
        bit_offset: 0,
        dynamic_bit_offset: None,
    };
    emit_element_merge(builder, stage, variable, element_width, &offset);
    let one = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
    let next = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Binary(next, body_index, BinaryOp::Add, one));
    builder.seal_block(SIRTerminator::Jump(header, vec![next]));

    builder.switch_to_block(exit);
}
