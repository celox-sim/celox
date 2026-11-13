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
    Foreach {
        header: ForeachHeader,
        first: usize,
        start: usize,
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
                    defined: false,
                    is_static,
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
                defined: true,
                is_static,
            });
        }
        Ok(())
    }

    /// Whether the tokens at the cursor start an external constraint
    /// block, `constraint C::c { ... }` (18.5.1).
    pub(super) fn external_constraint_start(&self) -> bool {
        if self.peek() != "constraint" {
            return false;
        }
        // A scoped name follows: identifiers and parameter lists joined
        // by `::`, ending at the block.
        let mut offset = 1;
        let mut scoped = false;
        loop {
            match self.nth(offset) {
                "::" => {
                    scoped = true;
                    offset += 1;
                }
                "{" => return scoped,
                "" | ";" => return false,
                _ => offset += 1,
            }
        }
    }

    /// The lexical scope an item at the cursor sits in: its package, if
    /// any, then its open outer classes. This is the scope an external
    /// constraint block must share with its class declaration (18.5.1).
    pub(super) fn lexical_scope(&self, package: Option<String>) -> Vec<String> {
        let mut scope = package.into_iter().collect::<Vec<_>>();
        scope.extend(
            self.class_stack
                .iter()
                .map(|outer| outer.name.spelling.clone()),
        );
        scope
    }

    /// Parse an external constraint block and complete the prototype it
    /// names in its finished class declaration (18.5.1).
    pub(super) fn external_constraint(&mut self, lexical_scope: &[String]) -> Result<()> {
        self.expect("constraint")?;
        let mut names = vec![self.name()?];
        loop {
            if self.eat("#") {
                self.expect("(")?;
                let mut depth = 1usize;
                while depth > 0 {
                    match self.peek() {
                        "" => return Err(self.error("unterminated parameter list")),
                        "(" | "[" | "{" => depth += 1,
                        ")" | "]" | "}" => depth -= 1,
                        _ => {}
                    }
                    self.at += 1;
                }
            }
            if !self.eat("::") {
                break;
            }
            names.push(self.name()?);
        }
        let (Some(constraint), Some(class)) = (names.pop(), names.pop()) else {
            return Err(self.error("external constraint requires a scoped name"));
        };
        // A leading scope names the package and outer classes the class
        // was declared in, or a suffix of them as written from inside that
        // scope (18.5.1); with no scope, the name resolves in the lexical
        // scope of this block, which the class declaration must share.
        let given: Vec<String> = names.iter().map(|name| name.spelling.clone()).collect();
        let expressions = self.constraint_block()?;
        let Some(index) = self
            .classes
            .iter()
            .enumerate()
            .rev()
            .find(|(index, declared)| {
                declared.name.spelling == class.spelling
                    && if given.is_empty() {
                        self.class_scopes[*index].as_slice() == lexical_scope
                    } else {
                        self.class_scopes[*index].ends_with(&given)
                    }
            })
            .map(|(index, _)| index)
        else {
            return Err(self.error("external constraint of an undeclared class"));
        };
        let class = &mut self.classes[index];
        let Some(block) = class
            .constraints
            .iter_mut()
            .find(|block| block.name.spelling == constraint.spelling)
        else {
            return Err(self.error("external constraint without a prototype"));
        };
        if block.defined {
            return Err(self.error("duplicate external constraint block"));
        }
        block.defined = true;
        // A static prototype completed externally stays static, which the
        // analyzer treats as unsupported as it does an in-class static
        // block (18.5.10).
        block.expressions = if block.is_static { None } else { expressions };
        Ok(())
    }

    pub(super) fn constraint_block(&mut self) -> Result<Option<Vec<ExprId>>> {
        let mut expressions: Vec<ExprId> = Vec::new();
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
                        // A guarded soft constraint is a soft constraint of
                        // the guarded expression: discarded, both are true
                        // (18.5.13).
                        let soft = match &self.expressions[expression.0].kind {
                            ExprKind::SoftConstraint { expression } => Some(*expression),
                            _ => None,
                        };
                        let guarded = self.node(
                            start,
                            ExprKind::Binary {
                                left: guard,
                                operator: "||".into(),
                                right: soft.unwrap_or(expression),
                            },
                        )?;
                        expressions.push(match soft {
                            Some(_) => self.node(
                                start,
                                ExprKind::SoftConstraint {
                                    expression: guarded,
                                },
                            )?,
                            None => guarded,
                        });
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
                Job::Foreach {
                    header,
                    first,
                    start,
                } => {
                    let body: Vec<_> = expressions.drain(first..).collect();
                    for body in body {
                        expressions.push(self.node(
                            start,
                            ExprKind::ConstraintForeach {
                                array: header.array,
                                variables: header.variables.clone(),
                                body,
                            },
                        )?);
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
                        let header = self.foreach_header()?;
                        jobs.push(Job::Foreach {
                            header,
                            first: expressions.len(),
                            start,
                        });
                        jobs.push(Job::Set);
                    } else if self.eat("disable") {
                        self.expect("soft")?;
                        let variable = self.constraint_primary()?;
                        expressions.push(self.node(start, ExprKind::DisableSoft { variable })?);
                        self.expect(";")?;
                    } else if self.eat("unique") {
                        supported = false;
                        self.constraint_ranges(false)?;
                        self.expect(";")?;
                    } else {
                        let soft = self.eat("soft");
                        if soft {
                            // `soft` takes an expression or a
                            // distribution, not a constraint set (Syntax
                            // 18-2).
                            // `soft a -> b` is the soft expression
                            // `!a || b`, as a guard is (18.5.6).
                            let (first, simple) = self.constraint_condition()?;
                            supported &= simple;
                            let mut chain = vec![first];
                            while self.eat("->") {
                                let (next, simple) = self.constraint_condition()?;
                                supported &= simple;
                                chain.push(next);
                            }
                            let mut expression = chain.pop().expect("a soft expression");
                            while let Some(condition) = chain.pop() {
                                let negated = self.node(
                                    start,
                                    ExprKind::Unary {
                                        operator: "!".into(),
                                        expression: condition,
                                        postfix: false,
                                    },
                                )?;
                                expression = self.node(
                                    start,
                                    ExprKind::Binary {
                                        left: negated,
                                        operator: "||".into(),
                                        right: expression,
                                    },
                                )?;
                            }
                            expressions
                                .push(self.node(start, ExprKind::SoftConstraint { expression })?);
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

    fn constraint_primary(&mut self) -> Result<ExprId> {
        let primary = self.expression(14)?;
        let mut id = primary;
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
                    return Ok(primary);
                }
                _ => return Err(self.error("expected a constraint variable reference")),
            }
        }
    }
}
