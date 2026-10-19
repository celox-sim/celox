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
    pub(super) fn node(
        parser: &mut Parser<'_>,
        start: usize,
        kind: BodyItemKind,
    ) -> Result<BodyItemId> {
        if parser.body_items.len() >= 1_000_000 {
            return Err(parser.unsupported("native body-item arena exceeds one million entries"));
        }
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
            let end_label = if parser.eat(":") {
                Some(parser.name()?)
            } else {
                None
            };
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
