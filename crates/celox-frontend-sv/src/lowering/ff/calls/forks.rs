//! `fork` in a function unit (IEEE 1800-2023 9.3.2), and the boxes of the
//! variables that the parent and the children share.

use super::*;

impl Ff<'_, '_> {
    /// `fork` in the unit being lowered (IEEE 1800-2023 9.3.2): a
    /// suspension with [`celox_design::ProcessStatus::Fork`] whose entries
    /// are resume points of the unit, one per branch. The runtime starts each
    /// child in a copy of the frames with the unit's resume slot set to its
    /// entry, whose address the word carries (see
    /// [`celox_design::PROCESS_FORK_RESUME_SHIFT`]); the parent continues
    /// at the open block afterwards. A branch that runs to its end ends its
    /// process.
    pub(in crate::lowering::ff) fn fork_in_unit(
        &mut self,
        join: celox_design::ForkJoin,
        branches: &[Vec<sv::ir::Stmt>],
    ) -> Result<(), sv::AnalyzerError> {
        let frame = self.frame.as_ref().expect("lowering a function unit");
        let (own, base) = (frame.function, frame.base);
        let first = frame
            .resume
            .as_ref()
            .ok_or_else(|| unsupported("a fork in a unit that cannot suspend"))?
            .blocks
            .len();
        if first + branches.len() > celox_frontend_core::process::MAX_PROCESS_SUSPENSIONS {
            return Err(unsupported("a function with too many suspension points"));
        }
        let entries: Vec<BlockId> = branches.iter().map(|_| self.b.new_block()).collect();
        self.resume_frame()
            .expect("checked above")
            .blocks
            .extend(entries.iter().copied());
        // The frame is laid out in the order of its slots, and the resume
        // slot follows slots of fixed shape, so its offset is known.
        let functions = self.m.out_of_line().expect("lowering out of line");
        let function = &functions.functions[own];
        let slot = function.resume.expect("a resumable unit");
        let offset = layout(&function.slots[..=slot]).0[slot];
        let frame_base = self.b.alloc_bit(32, false);
        self.b.emit(SIRInstruction::Load(
            frame_base,
            stable(base),
            SIROffset::Static(0),
            32,
        ));
        let high = self.b.alloc_bit(32, false);
        self.b.emit(SIRInstruction::Imm(high, SIRValue::new(0u32)));
        let current = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Concat(current, vec![high, frame_base]));
        let mode = match join {
            celox_design::ForkJoin::All => 0u64,
            celox_design::ForkJoin::Any => 1,
            celox_design::ForkJoin::None => 2,
        };
        let fixed = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::Imm(
            fixed,
            SIRValue::new(
                first as u64
                    | ((branches.len() as u64) << 16)
                    | (mode << 32)
                    | ((offset as u64) << celox_design::PROCESS_FORK_RESUME_SHIFT),
            ),
        ));
        let shift = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::Imm(
            shift,
            SIRValue::new(u64::from(celox_design::PROCESS_FORK_RESUME_SHIFT)),
        ));
        let address = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::Binary(
            address,
            current,
            BinaryOp::Shl,
            shift,
        ));
        let word = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Binary(word, fixed, BinaryOp::Add, address));
        let status = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            status,
            SIRValue::new(celox_design::ProcessStatus::Fork.code()),
        ));
        self.suspend_unit(status, word, None)?;
        // The parent's continuation, empty until the branches are lowered.
        let continuation = self.b.seal_block(SIRTerminator::Return);
        // A branch runs in a process of its own: no loop or subroutine of
        // the parent encloses it.
        let loops = std::mem::take(&mut self.scope.loops);
        let functions = std::mem::take(&mut self.scope.functions);
        let overlay = std::mem::take(&mut self.scope.overlay);
        let timing_boundary = self.call_stack.timing_call_boundary;
        self.call_stack.timing_call_boundary = self.call_stack.active_calls.len();
        let mut result = Ok(());
        for (entry, branch) in entries.into_iter().zip(branches) {
            self.b.switch_to_block(entry);
            match self.exec_block(branch) {
                Ok(true) => {
                    let request = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
                    self.b
                        .emit(SIRInstruction::Imm(request, SIRValue::new(0u64)));
                    if let Err(error) =
                        self.suspend_unit_with(celox_design::ProcessStatus::Done, request)
                    {
                        result = Err(error);
                        break;
                    }
                    // The process ended: its resume point is never taken.
                    self.b.seal_block(SIRTerminator::Return);
                }
                Ok(false) => {}
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        self.call_stack.timing_call_boundary = timing_boundary;
        self.scope.loops = loops;
        self.scope.functions = functions;
        self.scope.overlay = overlay;
        // A branch that failed leaves its block open; the unit is dropped.
        if result.is_ok() {
            self.b.switch_to_block(continuation);
        }
        result
    }

    /// `body` of `subroutine`, being lowered into a unit, with the variables
    /// a fork in it shares in boxes (see [`SharedBox`]), which it pushes to
    /// [`CallStack::shared_boxes`]; the boxes of the formals are made here. A
    /// variable is shared when a branch names it, other than one the branch
    /// declares, and the body writes it besides its declaration from its
    /// first statement that holds a fork on: one that no one writes after a
    /// fork reads the same in every copy. Returns the number of boxes
    /// pushed.
    pub(in crate::lowering::ff) fn share_forked(
        &mut self,
        subroutine: &sv::ir::Subroutine,
        mut body: Vec<sv::ir::Stmt>,
    ) -> Result<(Vec<sv::ir::Stmt>, usize), sv::AnalyzerError> {
        use super::super::super::procedural::expr_idents;
        if self.frame.is_none() || self.kernel.is_some() {
            return Ok((body, 0));
        }
        let mut branches: Vec<&Vec<sv::ir::Stmt>> = Vec::new();
        for stmt in &body {
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::Fork {
                    branches: forked, ..
                } = stmt
                {
                    branches.extend(forked);
                }
            });
        }
        // The variables the body passes to a `ref` formal that a fork of
        // the callee shares, which the callee's children reach through the
        // place the call passes: the variable is shared as its own would be
        // (IEEE 1800-2023 13.5.2).
        let mut passed = HashSet::default();
        for stmt in &body {
            stmt.walk(&mut |stmt| {
                let mut calls = Vec::new();
                stmt_calls(stmt, &mut calls);
                for (callee, args) in calls {
                    let sv::ir::Callee::Subroutine(callee) = callee else {
                        continue;
                    };
                    for (position, arg) in args.iter().enumerate() {
                        // An element of an array reaches the array: the
                        // variable is shared whatever part the call binds.
                        if let Some(name) = arg.as_ref().and_then(actual_root)
                            && forks_through_ref(self.m, &callee, position)
                        {
                            passed.insert(name.to_string());
                        }
                    }
                }
            });
        }
        if branches.is_empty() && passed.is_empty() {
            return Ok((body, 0));
        }
        let lvalue_idents = |lvalue: &sv::ir::LValue, names: &mut HashSet<String>| {
            names.insert(lvalue.name().to_string());
            if let sv::ir::LValue::Select { name, msb, lsb, .. } = lvalue {
                expr_idents(
                    &sv::ir::Expr::Select {
                        expr: Box::new(sv::ir::Expr::Ident(name.clone())),
                        msb: msb.clone(),
                        lsb: lsb.clone(),
                        signed: false,
                    },
                    names,
                );
            }
        };
        let mut named = HashSet::default();
        for branch in branches {
            let mut declared = HashSet::default();
            let mut names = HashSet::default();
            for stmt in branch {
                stmt.walk(&mut |stmt| {
                    if let sv::ir::Stmt::Local { name, .. } = stmt {
                        declared.insert(name.clone());
                    }
                });
                let mut stmt = stmt.clone();
                let mut targets = HashSet::default();
                stmt.visit_mut(
                    &mut |expr| expr_idents(expr, &mut names),
                    &mut |lvalue| lvalue_idents(lvalue, &mut targets),
                    &mut |_| {},
                );
                names.extend(targets);
            }
            named.extend(names.into_iter().filter(|name| !declared.contains(name)));
        }
        // What the body writes, but by declarations: by assignments, and
        // through the arguments that the tasks and subroutines it calls write,
        // from its first statement that holds a fork on. The statements of
        // the body run in order and each child starts in a copy of the
        // parent's frames (see `Ff::fork_in_unit`), so what only statements
        // before it write reads the same in the parent and in every child;
        // a loop that holds the fork is that statement, which counts the
        // writes of later iterations, and of the children.
        let first = body
            .iter()
            .position(|stmt| {
                let mut forks = false;
                stmt.walk(&mut |stmt| forks |= matches!(stmt, sv::ir::Stmt::Fork { .. }));
                forks
            })
            .unwrap_or(body.len());
        let written = self.m.written_names(&body[first..]);
        let mut shared: Vec<&String> = automatic_names(subroutine)
            .filter(|name| {
                named.contains(*name) && written.contains(*name) || passed.contains(*name)
            })
            .collect();
        shared.sort_unstable();
        shared.dedup();
        let (constants, parameter_types) = (self.m.constants, self.m.parameter_types);
        let constant = |position: &sv::ir::ConstExpr| {
            sv::typecheck::eval_const_expr_with_types(position, constants, parameter_types)
        };
        let mut boxes = Vec::new();
        for name in shared {
            let param = subroutine.params.iter().find(|param| param.name == *name);
            if param.is_some_and(|param| param.direction.is_written())
                || subroutine.return_var.as_ref() == Some(name)
            {
                return Err(unsupported(format!(
                    "fork sharing the output `{name}` of `{}`",
                    subroutine.name
                )));
            }
            let id = self
                .m
                .id(name)
                .ok_or_else(|| unsupported(format!("variable `{name}`")))?;
            let variable = self.m.var(id);
            if !variable.array_dims.is_empty() {
                return Err(unsupported(format!(
                    "fork sharing the unpacked array `{name}` of `{}`",
                    subroutine.name
                )));
            }
            let field = sv::ir::ObjectField {
                offset: 0,
                width: variable.width as u32,
                signed: variable.signed,
                handle: match variable.value_kind {
                    celox_design::ValueKind::Bits => None,
                    celox_design::ValueKind::StrRef => Some(sv::ir::HandleProperty::String),
                    celox_design::ValueKind::ObjRef => Some(sv::ir::HandleProperty::Object),
                },
                four_state: variable.is_4state,
                nullable: false,
                real: variable.float.is_some(),
                key: self.box_changes(),
            };
            let (handle_id, handle) =
                self.m
                    .temp("fork_box", celox_design::HANDLE_WIDTH, false, false);
            self.m
                .variables
                .get_mut(&handle_id)
                .expect("a box handle")
                .value_kind = celox_design::ValueKind::ObjRef;
            let alias = super::super::references::Alias {
                formal: name.clone(),
                source_name: name.clone(),
                target: super::super::references::Target::Property {
                    handle: handle.clone(),
                    field,
                },
            };
            body = alias.rewrite(body, &constant, &mut |field| self.part_scratch(field))?;
            boxes.push((
                param.is_some(),
                SharedBox {
                    var: name.clone(),
                    handle,
                    field,
                },
            ));
        }
        // A variable the body does not declare, a formal or a temporary the
        // analyzer made, gets its box at the entry; one it declares, each
        // time the declaration runs.
        let mut declared = HashSet::default();
        for stmt in &body {
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::Local { name, .. } = stmt {
                    declared.insert(name.clone());
                }
            });
        }
        let count = boxes.len();
        for (formal, shared) in boxes {
            if formal || !declared.contains(&shared.var) {
                self.open_box(&shared)?;
            }
            self.call_stack.shared_boxes.push(shared);
        }
        Ok((body, count))
    }

    /// The property key of a new box's field, with a change count of its
    /// own: a store into the box bumps it and a wait that reads the box
    /// watches it, as for a watched class property (see `property_changes`),
    /// so a child's write wakes the parent waiting on their shared variable.
    /// The count is a module variable, which the unit and the units it
    /// passes the box to write (see `ProcModule::box_changes`).
    pub(super) fn box_changes(&mut self) -> sv::ir::ChangeKey {
        let (counter, _) = self.m.module_temp(
            "box_changes",
            super::super::super::ff::PROPERTY_CHANGES_WIDTH,
            false,
            false,
        );
        // Observed as #3284's counts are: not one of a process's hidden
        // variables, whose changes nothing counts.
        self.m
            .variables
            .get_mut(&counter)
            .expect("a box count")
            .hidden = false;
        // The box's own namespace: no class property's count, and nothing
        // that counts every property's stores, such as `randomize`.
        self.m.box_changes.push(counter);
        if let Some(resume) = self.resume_frame() {
            resume.staging.insert(counter);
        }
        sv::ir::ChangeKey::Box(counter.0)
    }

    /// Make the box of `shared` (see [`SharedBox`]) holding the variable's
    /// value. The box is no class object: its creation draws on no RNG.
    pub(in crate::lowering::ff) fn open_box(
        &mut self,
        shared: &SharedBox,
    ) -> Result<(), sv::AnalyzerError> {
        use celox_heap::ops;
        let size = self.constant64(u64::from(shared.field.storage()));
        let object = self.heap_call(
            ops::encode(ops::OBJECT_NEW, u64::from(celox_heap::BOX_CLASS)),
            &[size],
        );
        if let Some(kind) = shared.field.handle {
            let offset = self.constant64(0);
            let kind = self.constant64(match kind {
                sv::ir::HandleProperty::String => 0,
                sv::ir::HandleProperty::Object => 1,
            });
            self.heap_call(ops::OBJECT_HANDLE, &[object, offset, kind]);
        }
        let handle = self.b.alloc_bit(celox_design::HANDLE_WIDTH, false);
        self.b.emit(SIRInstruction::Slice(
            handle,
            object,
            0,
            celox_design::HANDLE_WIDTH,
        ));
        let handle_id = self
            .m
            .id(&shared.handle)
            .ok_or_else(|| unsupported("a box handle"))?;
        self.store(
            handle_id,
            SIROffset::Static(0),
            celox_design::HANDLE_WIDTH,
            handle,
        );
        self.exec(&sv::ir::Stmt::Eval(sv::ir::Expr::Call {
            callee: sv::ir::Callee::Object(Box::new(sv::ir::ObjectOp::Store(shared.field))),
            args: vec![
                Some(sv::ir::Expr::Ident(shared.handle.clone())),
                Some(sv::ir::Expr::Ident(shared.var.clone())),
            ],
            arg_order: Vec::new(),
        }))?;
        Ok(())
    }
}

/// Whether formal `position` of subroutine `name` is a `ref` formal that a
/// branch of a fork in its body writes (see `ProcModule::written_names`),
/// also by passing it on to such a formal of another subroutine: the
/// children of that fork, which run in copies of the frames, then write the
/// caller's variable through it. A child that only reads it reads the
/// caller's variable while the caller waits in the call: a `ref` formal
/// cannot be named in a fork the call can return before (IEEE 1800-2023
/// 9.3.2).
pub(super) fn forks_through_ref(m: &ProcModule<'_>, name: &str, position: usize) -> bool {
    let mut writes = false;
    let mut pending = vec![(name.to_string(), position)];
    let mut visited = HashSet::default();
    while let Some((name, position)) = pending.pop() {
        if !visited.insert((name.clone(), position)) {
            continue;
        }
        let Some(subroutine) = m.subroutine(&name) else {
            continue;
        };
        let Some(param) = subroutine
            .params
            .get(position)
            .filter(|param| param.direction.is_reference())
        else {
            continue;
        };
        let formal = &param.name;
        for stmt in &subroutine.body {
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::Fork { branches, .. } = stmt {
                    writes |= branches
                        .iter()
                        .any(|branch| m.written_names(branch).contains(formal));
                }
                let mut calls = Vec::new();
                stmt_calls(stmt, &mut calls);
                for (callee, args) in calls {
                    let sv::ir::Callee::Subroutine(callee) = callee else {
                        continue;
                    };
                    for (next, arg) in args.iter().enumerate() {
                        if arg.as_ref().and_then(actual_root) == Some(formal.as_str()) {
                            pending.push((callee.clone(), next));
                        }
                    }
                }
            });
        }
        if writes {
            return true;
        }
    }
    false
}

/// The variable a `ref` actual names, whole or in part: an element or
/// another select of it.
pub(super) fn actual_root(actual: &sv::ir::Expr) -> Option<&str> {
    match actual {
        sv::ir::Expr::Ident(name) => Some(name),
        sv::ir::Expr::Select { expr, .. } => actual_root(expr),
        _ => None,
    }
}
