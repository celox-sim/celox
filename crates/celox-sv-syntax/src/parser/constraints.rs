//! Class constraint syntax validation. Constraints retain their opaque class
//! owner. Simple expression blocks also retain their expression nodes.
use super::*;

enum Job {
    Block(bool),
    Expression,
    Set,
    Guard {
        condition: ExprId,
        first: usize,
        start: usize,
        negate: bool,
        has_else: bool,
    },
}

impl Parser<'_> {
    pub(super) fn class_constraint_start(&self) -> bool {
        let mut cursor = self.at;
        let word = |cursor: usize| {
            self.indices
                .get(cursor)
                .map(|index| self.tokens[*index].text.as_str())
                .unwrap_or("")
        };
        if matches!(word(cursor), "extern" | "pure") {
            cursor += 1;
        }
        if word(cursor) == "static" {
            cursor += 1;
        }
        word(cursor) == "constraint"
    }

    pub(super) fn class_constraint(&mut self) -> Result<()> {
        if let Some(class) = self.class_stack.last_mut() {
            class.has_constraints = true;
        }
        let prototype = self.eat("extern") || self.eat("pure");
        let is_static = self.eat("static");
        self.expect("constraint")?;
        let name = self.name()?;
        if self.class_stack.last().is_some_and(|class| {
            class
                .constraints
                .iter()
                .any(|block| block.name.spelling == name.spelling)
        }) {
            return Err(self.error("duplicate constraint block"));
        }
        if self.eat(";") {
            if let Some(class) = self.class_stack.last_mut() {
                class.constraints.push(ClassConstraint {
                    name,
                    expressions: None,
                });
            }
            return Ok(());
        }
        if prototype {
            return Err(self.error("extern or pure constraint requires a prototype"));
        }
        let expressions = self.constraint_block()?;
        if let Some(class) = self.class_stack.last_mut() {
            class.constraints.push(ClassConstraint {
                name,
                expressions: if is_static { None } else { expressions },
            });
        }
        Ok(())
    }

    pub(super) fn constraint_block(&mut self) -> Result<Option<Vec<ExprId>>> {
        let mut expressions = Vec::new();
        let mut supported = true;
        self.expect("{")?;
        let mut jobs = vec![Job::Block(true)];
        while let Some(job) = jobs.pop() {
            while self.directive()? {}
            match job {
                Job::Block(solve) => {
                    if self.eat("}") {
                        continue;
                    }
                    jobs.push(Job::Block(solve));
                    if solve && self.eat("solve") {
                        supported = false;
                        self.constraint_primary_list()?;
                        self.expect("before")?;
                        self.constraint_primary_list()?;
                        self.expect(";")?;
                    } else {
                        jobs.push(Job::Expression);
                    }
                }
                Job::Set => {
                    if self.eat("{") {
                        jobs.push(Job::Block(false));
                    } else {
                        jobs.push(Job::Expression);
                    }
                }
                Job::Guard {
                    condition,
                    first,
                    start,
                    negate,
                    has_else,
                } => {
                    let guard = if negate {
                        self.node(
                            start,
                            ExprKind::Unary {
                                operator: "!".into(),
                                expression: condition,
                                postfix: false,
                            },
                        )?
                    } else {
                        condition
                    };
                    let body: Vec<_> = expressions.drain(first..).collect();
                    for expression in body {
                        expressions.push(self.node(
                            start,
                            ExprKind::Binary {
                                left: guard,
                                operator: "||".into(),
                                right: expression,
                            },
                        )?);
                    }
                    if has_else && self.eat("else") {
                        jobs.push(Job::Guard {
                            condition,
                            first: expressions.len(),
                            start,
                            negate: false,
                            has_else: false,
                        });
                        jobs.push(Job::Set);
                    }
                }
                Job::Expression => {
                    let start = self.at;
                    if self.eat("if") {
                        self.expect("(")?;
                        let condition = self.expression(0)?;
                        self.expect(")")?;
                        jobs.push(Job::Guard {
                            condition,
                            first: expressions.len(),
                            start,
                            negate: true,
                            has_else: true,
                        });
                        jobs.push(Job::Set);
                    } else if self.eat("foreach") {
                        supported = false;
                        self.foreach_header()?;
                        jobs.push(Job::Set);
                    } else if self.eat("disable") {
                        supported = false;
                        self.expect("soft")?;
                        self.constraint_primary()?;
                        self.expect(";")?;
                    } else if self.eat("unique") {
                        supported = false;
                        self.constraint_ranges(false)?;
                        self.expect(";")?;
                    } else {
                        let soft = self.eat("soft");
                        if soft {
                            supported = false;
                            self.expression(0)?;
                        } else {
                            let (expression, simple) = self.constraint_condition()?;
                            expressions.push(expression);
                            supported &= simple;
                        }
                        if !soft && self.eat("->") {
                            let condition = expressions.pop().expect("condition retained");
                            jobs.push(Job::Guard {
                                condition,
                                first: expressions.len(),
                                start,
                                negate: true,
                                has_else: false,
                            });
                            jobs.push(Job::Set);
                        } else {
                            if self.eat("dist") {
                                supported = false;
                                self.constraint_ranges(true)?;
                            }
                            self.expect(";")?;
                        }
                    }
                }
            }
        }
        Ok(supported.then_some(expressions))
    }

    fn constraint_condition(&mut self) -> Result<(ExprId, bool)> {
        // Keep the constraint-set arrow for the outer continuation. Equivalence
        // shares its expression precedence, so consume it explicitly here.
        let expression = self.expression(2)?;
        let mut simple = true;
        while self.eat("<->") {
            simple = false;
            self.expression(2)?;
        }
        Ok((expression, simple))
    }

    fn constraint_ranges(&mut self, distribution: bool) -> Result<()> {
        self.expect("{")?;
        loop {
            if self.peek() == "default" {
                return Err(self.unsupported(
                    "default distribution weights are outside the existing constraint grammar",
                ));
            }
            if self.peek() == "[" {
                let dimension = self.dimension()?;
                if !matches!(dimension.kind, DimensionKind::Range { .. }) {
                    return Err(self.error("constraint value range requires two endpoints"));
                }
            } else {
                self.expression(0)?;
            }
            if distribution && (self.eat(":=") || self.eat(":/")) {
                self.expression(0)?;
            }
            if self.eat("}") {
                return Ok(());
            }
            self.expect(",")?;
        }
    }

    fn constraint_primary_list(&mut self) -> Result<()> {
        self.constraint_primary()?;
        while self.eat(",") {
            self.constraint_primary()?;
        }
        Ok(())
    }

    fn constraint_primary(&mut self) -> Result<()> {
        let mut id = self.expression(14)?;
        let mut receiver = false;
        loop {
            match &self.expressions[id.0].kind {
                ExprKind::Member { expression, .. }
                | ExprKind::Select { expression, .. }
                | ExprKind::Parameterized { expression, .. } => {
                    receiver = true;
                    id = *expression;
                }
                ExprKind::Name(name)
                    if !self.reserved_word(&name.spelling)
                        || receiver && matches!(name.spelling.as_str(), "this" | "super") =>
                {
                    return Ok(());
                }
                _ => return Err(self.error("expected a constraint variable reference")),
            }
        }
    }
}
