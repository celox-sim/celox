//! Body-item continuations. Child handles and pending control frames keep
//! parsing, cloning and dropping independent of generate nesting depth.
use super::*;

#[derive(Default)]
pub(super) struct Builder {
    pub roots: Vec<BodyItemId>,
    frames: Vec<Frame>,
    pub pending: Option<usize>,
}
enum Frame {
    Region {
        start: usize,
        items: Vec<BodyItemId>,
    },
    Block {
        start: usize,
        leading: Option<Name>,
        label: Option<Name>,
        items: Vec<BodyItemId>,
    },
    Then {
        start: usize,
        condition: ExprId,
    },
    Otherwise {
        start: usize,
        condition: ExprId,
        then_body: BodyItemId,
    },
    Case {
        start: usize,
        selector: ExprId,
        arms: Vec<GenerateCaseArm>,
    },
    CaseBody {
        start: usize,
        selector: ExprId,
        arms: Vec<GenerateCaseArm>,
        labels: Vec<ExprId>,
        colon: bool,
    },
    Loop {
        start: usize,
        declared: bool,
        variable: Name,
        initial: ExprId,
        condition: ExprId,
        step: ForStep,
    },
}
impl Builder {
    /// Whether an item here is in the design element's own scope: in no
    /// generate block, conditional, case or loop (IEEE 1800-2023 27.3).
    pub(super) fn element_scope(&self) -> bool {
        self.frames
            .iter()
            .all(|frame| matches!(frame, Frame::Region { .. }))
    }
    pub(super) fn node(
        parser: &mut Parser<'_>,
        start: usize,
        kind: BodyItemKind,
    ) -> Result<BodyItemId> {
        if let BodyItemKind::Unconverted { category, .. } = &kind {
            parser.validate_gate_boundary(category, parser.range(start))?;
        }
        let id = BodyItemId(parser.body_items.len());
        parser.body_items.push(BodyItem {
            kind,
            tokens: parser.range(start),
        });
        Ok(id)
    }
    pub fn complete(
        &mut self,
        parser: &mut Parser<'_>,
        start: usize,
        kind: BodyItemKind,
    ) -> Result<()> {
        let start = self.pending.take().unwrap_or(start);
        // A generate block holds generate items, which a specparam
        // declaration is not (IEEE 1800-2023 27.3, Syntax 27-1).
        if matches!(&kind, BodyItemKind::Unconverted { category, .. } if category == "specparam")
            && !self.element_scope()
        {
            return Err(parser.error_at(
                parser.range(start),
                "specparam declaration in a generate block",
            ));
        }
        let id = Self::node(parser, start, kind)?;
        self.feed(parser, id)
    }
    fn feed(&mut self, parser: &mut Parser<'_>, mut id: BodyItemId) -> Result<()> {
        loop {
            id = match self.frames.pop() {
                None => {
                    self.roots.push(id);
                    return Ok(());
                }
                Some(Frame::Region { start, mut items }) => {
                    items.push(id);
                    self.frames.push(Frame::Region { start, items });
                    return Ok(());
                }
                Some(Frame::Block {
                    start,
                    leading,
                    label,
                    mut items,
                }) => {
                    items.push(id);
                    self.frames.push(Frame::Block {
                        start,
                        leading,
                        label,
                        items,
                    });
                    return Ok(());
                }
                Some(Frame::Then { start, condition }) => {
                    if parser.eat("else") {
                        self.frames.push(Frame::Otherwise {
                            start,
                            condition,
                            then_body: id,
                        });
                        return Ok(());
                    }
                    Self::node(
                        parser,
                        start,
                        BodyItemKind::Conditional {
                            condition,
                            then_body: id,
                            otherwise: None,
                        },
                    )?
                }
                Some(Frame::Otherwise {
                    start,
                    condition,
                    then_body,
                }) => Self::node(
                    parser,
                    start,
                    BodyItemKind::Conditional {
                        condition,
                        then_body,
                        otherwise: Some(id),
                    },
                )?,
                Some(Frame::Loop {
                    start,
                    declared,
                    variable,
                    initial,
                    condition,
                    step,
                }) => Self::node(
                    parser,
                    start,
                    BodyItemKind::Loop {
                        declared,
                        variable,
                        initial,
                        condition,
                        step,
                        body: id,
                    },
                )?,
                Some(Frame::CaseBody {
                    start,
                    selector,
                    mut arms,
                    labels,
                    colon,
                }) => {
                    arms.push(GenerateCaseArm {
                        labels,
                        colon,
                        body: id,
                    });
                    self.frames.push(Frame::Case {
                        start,
                        selector,
                        arms,
                    });
                    return Ok(());
                }
                Some(Frame::Case { .. }) => {
                    return Err(parser.error("expected a case-generate label"));
                }
            };
        }
    }
    pub fn finished(&self) -> bool {
        self.frames.is_empty() && self.pending.is_none()
    }
    pub fn active(&self) -> bool {
        !self.frames.is_empty()
    }
    /// Parse only the structural control/block boundary at the current cursor.
    /// Ordinary body items are consumed by their existing grammar exactly once.
    pub fn boundary(
        &mut self,
        parser: &mut Parser<'_>,
        scopes: &mut Vec<&'static str>,
    ) -> Result<bool> {
        if self.frames.len() >= 1_000_000 {
            return Err(parser
                .unsupported("native generate continuation stack exceeds one million entries"));
        }
        if matches!(self.frames.last(), Some(Frame::Case { .. })) {
            let Some(Frame::Case {
                start,
                selector,
                mut arms,
            }) = self.frames.pop()
            else {
                unreachable!()
            };
            if parser.eat("endcase") {
                if arms.is_empty() {
                    return Err(parser.error("expected a case-generate item"));
                }
                if scopes.pop() != Some("endcase") {
                    return Err(parser.error("mismatched case-generate boundary"));
                }
                let id = Self::node(
                    parser,
                    start,
                    BodyItemKind::Case {
                        selector,
                        arms: std::mem::take(&mut arms),
                    },
                )?;
                self.feed(parser, id)?;
                return Ok(true);
            }
            let mut labels = Vec::new();
            let colon = if parser.eat("default") {
                parser.eat(":")
            } else {
                loop {
                    labels.push(parser.expression(0)?);
                    if !parser.eat(",") {
                        break;
                    }
                }
                parser.expect(":")?;
                true
            };
            self.frames.push(Frame::CaseBody {
                start,
                selector,
                arms,
                labels,
                colon,
            });
            return Ok(true);
        }
        if parser.peek() == "end" && matches!(self.frames.last(), Some(Frame::Block { .. })) {
            parser.at += 1;
            if scopes.pop() != Some("end") {
                return Err(parser.error("mismatched generate-block boundary"));
            }
            let Some(Frame::Block {
                start,
                leading,
                label,
                items,
            }) = self.frames.pop()
            else {
                unreachable!()
            };
            let end_label = parser.end_label(label.as_ref().or(leading.as_ref()))?;
            let id = Self::node(
                parser,
                start,
                BodyItemKind::Block {
                    leading_label: leading,
                    label,
                    end_label,
                    items,
                },
            )?;
            self.feed(parser, id)?;
            return Ok(true);
        }
        if parser.peek() == "endgenerate"
            && matches!(self.frames.last(), Some(Frame::Region { .. }))
        {
            parser.at += 1;
            if scopes.pop() != Some("endgenerate") {
                return Err(parser.error("mismatched generate-region boundary"));
            }
            let Some(Frame::Region { start, items }) = self.frames.pop() else {
                unreachable!()
            };
            let id = Self::node(parser, start, BodyItemKind::Region(items))?;
            self.feed(parser, id)?;
            return Ok(true);
        }
        let start = self.pending.take().unwrap_or(parser.at);
        match parser.peek() {
            "if" => {
                parser.at += 1;
                parser.expect("(")?;
                let condition = parser.expression(0)?;
                parser.expect(")")?;
                self.frames.push(Frame::Then { start, condition });
            }
            "for" => {
                parser.at += 1;
                parser.expect("(")?;
                let declared = parser.eat("genvar");
                let variable = parser.name()?;
                // In a loop generate block its index is a localparam, so a
                // nested loop cannot use the same genvar (IEEE 1800-2023
                // 27.4).
                if !declared
                    && self.frames.iter().any(|frame| {
                        matches!(frame, Frame::Loop { variable: outer, .. }
                            if outer.value() == variable.value())
                    })
                {
                    parser.at -= 1;
                    return Err(parser.error(format!(
                        "loop generate nested in a loop generate of the same genvar `{}`",
                        variable.value()
                    )));
                }
                parser.expect("=")?;
                let initial = parser.expression(0)?;
                parser.expect(";")?;
                let condition = parser.expression(0)?;
                parser.expect(";")?;
                let step_start = parser.at;
                let left = parser.expression(14)?;
                let step = if matches!(
                    parser.peek(),
                    "=" | "+="
                        | "-="
                        | "*="
                        | "/="
                        | "%="
                        | "&="
                        | "|="
                        | "^="
                        | "<<="
                        | ">>="
                        | "<<<="
                        | ">>>="
                ) {
                    let operator = parser.peek().to_owned();
                    parser.at += 1;
                    let right = parser.expression(0)?;
                    ForStep::Assignment {
                        left,
                        operator,
                        right,
                        tokens: parser.range(step_start),
                    }
                } else {
                    ForStep::Expression(left)
                };
                parser.expect(")")?;
                self.frames.push(Frame::Loop {
                    start,
                    declared,
                    variable,
                    initial,
                    condition,
                    step,
                });
            }
            "case" => {
                parser.at += 1;
                parser.expect("(")?;
                let selector = parser.expression(0)?;
                parser.expect(")")?;
                scopes.push("endcase");
                self.frames.push(Frame::Case {
                    start,
                    selector,
                    arms: Vec::new(),
                });
            }
            "generate" => {
                // Generate regions do not nest (IEEE 1800-2023 27.3).
                if self
                    .frames
                    .iter()
                    .any(|frame| matches!(frame, Frame::Region { .. }))
                {
                    return Err(parser.error("generate region nested in a generate region"));
                }
                parser.at += 1;
                scopes.push("endgenerate");
                self.frames.push(Frame::Region {
                    start,
                    items: Vec::new(),
                });
            }
            "begin" => {
                parser.at += 1;
                let label = if parser.eat(":") {
                    Some(parser.name()?)
                } else {
                    None
                };
                scopes.push("end");
                self.frames.push(Frame::Block {
                    start,
                    leading: None,
                    label,
                    items: Vec::new(),
                });
            }
            _ if parser.nth(1) == ":" && parser.nth(2) == "begin" => {
                let leading = Some(parser.name()?);
                parser.expect(":")?;
                parser.expect("begin")?;
                let label = if parser.eat(":") {
                    Some(parser.name()?)
                } else {
                    None
                };
                scopes.push("end");
                self.frames.push(Frame::Block {
                    start,
                    leading,
                    label,
                    items: Vec::new(),
                });
            }
            _ => {
                self.pending = Some(start);
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl Parser<'_> {
    /// Reject typedefs of the scope of `unit` that name each other in a
    /// cycle, so that none of them resolves to a data type (IEEE 1800-2023
    /// 6.18).
    pub(super) fn reject_circular_typedefs(&self, unit: &Unit) -> Result<()> {
        let mut aliases = Vec::<(&str, Option<&str>, TokenRange)>::new();
        let mut pending = unit.items.clone();
        while let Some(id) = pending.pop() {
            match &self.body_items[id.0].kind {
                BodyItemKind::Region(items) => pending.extend(items),
                BodyItemKind::TypeAlias(id) => {
                    let alias = &self.type_aliases[id.0];
                    let target = match alias.target {
                        TypeAliasTarget::DataType(id) => match &self.types[id.0].kind {
                            DataTypeKind::Named(path) if path.len() == 1 => {
                                Some(path[0].name.value())
                            }
                            _ => None,
                        },
                        _ => continue,
                    };
                    aliases.push((alias.name.value(), target, alias.tokens));
                }
                _ => {}
            }
        }
        for &(start, _, tokens) in &aliases {
            let mut name = start;
            for _ in 0..aliases.len() {
                let Some(target) = aliases
                    .iter()
                    .find(|(alias, ..)| *alias == name)
                    .and_then(|(_, target, _)| *target)
                else {
                    break;
                };
                if target == start {
                    return Err(self.error_at(
                        tokens,
                        format!("typedef `{start}` names itself through a cycle of typedefs"),
                    ));
                }
                name = target;
            }
        }
        Ok(())
    }

    /// Reject a loop generate whose index is a variable or net of the unit
    /// that no genvar declaration of the unit names: the loop index shall be
    /// declared as a genvar (IEEE 1800-2023 27.4).
    pub(super) fn reject_variable_loop_indices(&self, unit: &Unit) -> Result<()> {
        let mut pending = unit.items.clone();
        let mut loops = Vec::new();
        let mut genvars = fxhash::FxHashSet::default();
        while let Some(id) = pending.pop() {
            let item = &self.body_items[id.0];
            match &item.kind {
                BodyItemKind::Region(items) | BodyItemKind::Block { items, .. } => {
                    pending.extend(items)
                }
                BodyItemKind::Conditional {
                    then_body,
                    otherwise,
                    ..
                } => pending.extend(std::iter::once(*then_body).chain(*otherwise)),
                BodyItemKind::Case { arms, .. } => pending.extend(arms.iter().map(|arm| arm.body)),
                BodyItemKind::Loop {
                    declared,
                    variable,
                    body,
                    ..
                } => {
                    if !declared {
                        loops.push((variable, item.tokens));
                    }
                    pending.push(*body);
                }
                BodyItemKind::Unconverted { category, .. } if category == "genvar" => genvars
                    .extend(
                        self.tokens[item.tokens.start..item.tokens.end]
                            .iter()
                            .filter(|token| token.text != "genvar")
                            .filter_map(Token::identifier),
                    ),
                _ => {}
            }
        }
        for (variable, tokens) in loops {
            let name = variable.value();
            if genvars.contains(name) {
                continue;
            }
            let variable_declared = unit.items.iter().any(|id| {
                let declarators = match &self.body_items[id.0].kind {
                    BodyItemKind::Net(id) => &self.net_declarations[id.0].variables,
                    BodyItemKind::Data(id) => match &self.data_declarations[id.0] {
                        DataDeclaration::Variables(declaration)
                        | DataDeclaration::Constructed(declaration) => &declaration.variables,
                        DataDeclaration::Unconverted(_) => return false,
                    },
                    _ => return false,
                };
                declarators
                    .iter()
                    .any(|declarator| declarator.name.value() == name)
            });
            if variable_declared {
                return Err(self.error_at(
                    tokens,
                    format!("loop generate index `{name}` is a variable, not a genvar"),
                ));
            }
        }
        Ok(())
    }

    /// Reject a generate block whose name another declaration of the
    /// module scope also has: a variable, net or instance, a named block of
    /// a procedure, or a generate block of another generate construct (IEEE
    /// 1800-2023 23.9, 27.4, 27.5). The blocks of one conditional generate
    /// construct may share a name (27.5).
    pub(super) fn reject_duplicate_block_names(&self, unit: &Unit) -> Result<()> {
        if unit.kind == UnitKind::Package {
            return Ok(());
        }
        // Each other declaration of the scope, and the generate blocks of
        // each generate construct, with where they are.
        let mut declared = Vec::<(&str, TokenRange)>::new();
        let mut blocks = Vec::<Vec<(&str, TokenRange)>>::new();
        let mut pending = unit.items.clone();
        pending.reverse();
        while let Some(id) = pending.pop() {
            let item = &self.body_items[id.0];
            match &item.kind {
                BodyItemKind::Region(items) => pending.extend(items.iter().rev()),
                BodyItemKind::Net(id) => declared.extend(
                    self.net_declarations[id.0]
                        .variables
                        .iter()
                        .map(|variable| (variable.name.value(), item.tokens)),
                ),
                BodyItemKind::Data(id) => {
                    if let DataDeclaration::Variables(declaration)
                    | DataDeclaration::Constructed(declaration) = &self.data_declarations[id.0]
                    {
                        declared.extend(
                            declaration
                                .variables
                                .iter()
                                .map(|variable| (variable.name.value(), item.tokens)),
                        );
                    }
                }
                BodyItemKind::Instance(id) => declared.extend(
                    self.instantiations[id.0]
                        .instances
                        .iter()
                        .map(|instance| (instance.name.value(), item.tokens)),
                ),
                BodyItemKind::Procedure(id) => {
                    let statement = &self.statements[self.procedures[id.0].body.0];
                    let label = match &statement.kind {
                        StatementKind::Block { label, .. }
                        | StatementKind::Parallel { label, .. } => {
                            label.as_ref().or(statement.label.as_ref())
                        }
                        _ => None,
                    };
                    declared.extend(label.map(|label| (label.value(), item.tokens)));
                }
                BodyItemKind::Loop { body, .. } => {
                    blocks.push(self.generate_block_name(*body).into_iter().collect());
                }
                BodyItemKind::Conditional { .. } | BodyItemKind::Case { .. } => {
                    let mut names = Vec::new();
                    let mut alternatives = vec![id];
                    while let Some(id) = alternatives.pop() {
                        match &self.body_items[id.0].kind {
                            // An `else if` continues the construct (27.5).
                            BodyItemKind::Conditional {
                                then_body,
                                otherwise,
                                ..
                            } => {
                                names.extend(self.generate_block_name(*then_body));
                                if let Some(otherwise) = otherwise {
                                    match &self.body_items[otherwise.0].kind {
                                        BodyItemKind::Conditional { .. } => {
                                            alternatives.push(*otherwise)
                                        }
                                        _ => names.extend(self.generate_block_name(*otherwise)),
                                    }
                                }
                            }
                            BodyItemKind::Case { arms, .. } => names.extend(
                                arms.iter()
                                    .filter_map(|arm| self.generate_block_name(arm.body)),
                            ),
                            _ => {}
                        }
                    }
                    names.sort_unstable_by_key(|(name, _)| *name);
                    names.dedup_by_key(|(name, _)| *name);
                    blocks.push(names);
                }
                _ => {}
            }
        }
        let mut seen = fxhash::FxHashSet::default();
        for (name, _) in &declared {
            seen.insert(*name);
        }
        let mut generate = fxhash::FxHashSet::default();
        for (name, tokens) in blocks.into_iter().flatten() {
            if seen.contains(name) || !generate.insert(name) {
                return Err(self.error_at(
                    tokens,
                    format!(
                        "generate block `{name}` has the name of another declaration of `{}`",
                        unit.name.value()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The name of the generate block `id` of a generate construct, if it is
    /// a named block.
    fn generate_block_name(&self, id: BodyItemId) -> Option<(&str, TokenRange)> {
        let item = &self.body_items[id.0];
        match &item.kind {
            BodyItemKind::Block {
                leading_label,
                label,
                ..
            } => label
                .as_ref()
                .or(leading_label.as_ref())
                .map(|label| (label.value(), item.tokens)),
            _ => None,
        }
    }
}
