//! Covergroup declarations (IEEE 1800-2023 clause 19, A.2.11) as typed
//! nodes, each read by one call from `covergroup` through its `endgroup`,
//! and the names of user-defined nettypes.
use super::*;

/// One open group of a select expression: the alternatives of its `||`
/// that are complete, and the operands of the `&&` chain being read.
struct SelectFrame {
    start: usize,
    ors: Vec<SelectId>,
    ands: Vec<SelectId>,
}

impl Parser<'_> {
    /// A covergroup declaration at the cursor; `in_class` when it is an
    /// item of a class, where alone `covergroup extends` is allowed
    /// (A.2.11 footnote 29), and `in_element` when a design element
    /// contains it.
    pub(super) fn covergroup_declaration(
        &mut self,
        in_class: bool,
        in_element: bool,
    ) -> Result<CovergroupId> {
        let start = self.at;
        self.expect("covergroup")?;
        let (name, header, kind) = if self.peek() == "extends" {
            if !in_class {
                return Err(self.error(
                    "covergroup extends outside a class (IEEE 1800-2023 A.2.11 footnote 29)",
                ));
            }
            self.at += 1;
            let name = self.name()?;
            (name, self.range(start), CovergroupKind::Extends)
        } else {
            let name = self.name()?;
            let header = self.range(start);
            // Every formal is named (A.2.11 footnote 28).
            let ports = if self.peek() == "(" {
                Some(self.subroutine_ports(false)?)
            } else {
                None
            };
            let event = self.coverage_event()?;
            (name, header, CovergroupKind::Declared { ports, event })
        };
        self.expect(";")?;
        let mut items = Vec::new();
        loop {
            while self.coverage_directive(in_element)? {}
            if self.peek().is_empty() {
                return Err(self.error("expected `endgroup`"));
            }
            if self.peek() == "endgroup" {
                break;
            }
            let item = self.at;
            self.attributes()?;
            items.push(if self.coverage_option_start() {
                CoverageItem::Option(self.coverage_option(item)?)
            } else if self.peek() == "cross" || self.nth(1) == ":" && self.nth(2) == "cross" {
                CoverageItem::Cross(self.cross(item, in_element)?)
            } else {
                CoverageItem::Coverpoint(self.coverpoint(item, in_element)?)
            });
        }
        self.expect("endgroup")?;
        let end_label = self.end_label(Some(&name))?;
        let id = CovergroupId(self.covergroups.len());
        self.covergroups.push(CovergroupDeclaration {
            name,
            kind,
            items,
            end_label,
            header,
            tokens: self.range(start),
        });
        Ok(id)
    }

    /// A compiler directive between the items of a covergroup or of one of
    /// its bodies. In a design element (`in_element`) a `resetall` is as
    /// illegal there as between the items of the element (22.3).
    fn coverage_directive(&mut self, in_element: bool) -> Result<bool> {
        if in_element && self.peek() == "`" && self.nth(1) == "resetall" {
            return Err(self.error("resetall directive inside a design element"));
        }
        self.directive()
    }

    fn coverage_event(&mut self) -> Result<Option<CoverageEvent>> {
        if self.eat("with") {
            self.expect("function")?;
            self.expect("sample")?;
            return Ok(Some(CoverageEvent::Sample(self.subroutine_ports(false)?)));
        }
        if self.eat("@@") {
            self.expect("(")?;
            let mut events = Vec::new();
            loop {
                let start = self.at;
                let end = !self.eat("begin");
                if end {
                    self.expect("end")?;
                }
                let target = self.coverage_reference()?;
                events.push(BlockEvent {
                    end,
                    target,
                    tokens: self.range(start),
                });
                if !self.eat("or") {
                    break;
                }
            }
            self.expect(")")?;
            return Ok(Some(CoverageEvent::Block(events)));
        }
        if !matches!(self.peek(), "@" | "@*") {
            return Ok(None);
        }
        let id = self.typed_timing_control()?;
        match self.timing_controls[id.0].kind {
            TimingControlKind::Event(EventControl::Wildcard) => {
                return Err(self.error("coverage event cannot be a wildcard event"));
            }
            TimingControlKind::Event(EventControl::Named(name)) => {
                self.validate_coverage_reference(name)?
            }
            _ => {}
        }
        Ok(Some(CoverageEvent::Clocking(id)))
    }

    /// The target of a block event (19.3): a name, member, select or
    /// parameterized chain.
    fn coverage_reference(&mut self) -> Result<ExprId> {
        let id = self.expression(14)?;
        self.validate_coverage_reference(id)?;
        Ok(id)
    }

    fn validate_coverage_reference(&self, mut id: ExprId) -> Result<()> {
        loop {
            match &self.expressions[id.0].kind {
                ExprKind::Name(name) if !self.reserved_word(&name.spelling) => return Ok(()),
                ExprKind::Member { expression, .. }
                | ExprKind::Select { expression, .. }
                | ExprKind::Parameterized { expression, .. } => id = *expression,
                _ => return Err(self.error("expected a coverage event identifier")),
            }
        }
    }

    fn coverage_option_start(&self) -> bool {
        matches!(self.peek(), "option" | "type_option") && self.nth(1) == "."
    }

    /// `option.member = expression;` or `type_option.member =
    /// constant_expression;` (19.7); `start` is where its attributes begin.
    fn coverage_option(&mut self, start: usize) -> Result<CoverageOption> {
        let type_option = self.eat("type_option");
        if !type_option {
            self.expect("option")?;
        }
        self.expect(".")?;
        let member = self.name()?;
        self.expect("=")?;
        let value = self.expression(0)?;
        if type_option {
            self.validate_constant_operators(value)?;
        }
        self.expect(";")?;
        Ok(CoverageOption {
            type_option,
            member,
            value,
            tokens: self.range(start),
        })
    }

    /// The guard `iff ( expression )` of a coverpoint, cross or bin.
    fn coverage_guard(&mut self) -> Result<Option<ExprId>> {
        if !self.eat("iff") {
            return Ok(None);
        }
        Ok(Some(self.parenthesized_expression()?))
    }

    /// An expression of the covergroup grammar that ends where `operand`
    /// says (A.2.11).
    fn coverage_expression(&mut self, operand: CoverageOperand, minimum: u8) -> Result<ExprId> {
        let outer = std::mem::replace(&mut self.coverage_operand, operand);
        let value = self.expression(minimum);
        self.coverage_operand = outer;
        value
    }

    /// A `cover_point` (19.5); `start` is where its attributes begin.
    fn coverpoint(&mut self, start: usize, in_element: bool) -> Result<Coverpoint> {
        let (label, data_type) = if self.peek() == "coverpoint" {
            (None, None)
        } else {
            let data_type = if self.nth(1) != ":" {
                Some(self.data_type()?)
            } else {
                None
            };
            let label = self.name()?;
            self.expect(":")?;
            (Some(label), data_type)
        };
        self.expect("coverpoint")?;
        let expression = self.expression(0)?;
        let iff = self.coverage_guard()?;
        let mut items = Vec::new();
        let body = if self.eat(";") {
            None
        } else {
            let open = self.at;
            self.expect("{")?;
            loop {
                while self.coverage_directive(in_element)? {}
                if self.eat("}") {
                    break;
                }
                if self.peek().is_empty() {
                    return Err(self.error("expected `}`"));
                }
                // A.2.11 places attributes once, before the items; each
                // item may have its own here.
                let item = self.at;
                self.attributes()?;
                items.push(if self.coverage_option_start() {
                    CoverpointItem::Option(self.coverage_option(item)?)
                } else {
                    CoverpointItem::Bins(self.bins(item)?)
                });
            }
            Some(self.range(open))
        };
        Ok(Coverpoint {
            label,
            data_type,
            expression,
            iff,
            items,
            body,
            tokens: self.range(start),
        })
    }

    fn bins_keyword(&mut self) -> Result<BinsKind> {
        let kind = match self.peek() {
            "bins" => BinsKind::Bins,
            "illegal_bins" => BinsKind::IllegalBins,
            "ignore_bins" => BinsKind::IgnoreBins,
            _ => return Err(self.error("expected `bins`, `illegal_bins` or `ignore_bins`")),
        };
        self.at += 1;
        Ok(kind)
    }

    /// A bin declaration of a coverpoint (19.5, Syntax 19-2); `start` is
    /// where its attributes begin.
    fn bins(&mut self, start: usize) -> Result<Bins> {
        let wildcard = self.eat("wildcard");
        let kind = self.bins_keyword()?;
        let name = self.name()?;
        let array = if self.eat("[") {
            Some(if self.eat("]") {
                BinsArray::Unsized
            } else {
                let size = self.expression(0)?;
                self.expect("]")?;
                BinsArray::Sized(size)
            })
        } else {
            None
        };
        self.expect("=")?;
        let value_start = self.range(self.at);
        let value = self.bins_value()?;
        // The array and `wildcard` forms each alternative has (A.2.11).
        match (&value, &array) {
            (BinsValue::Transitions(_), Some(BinsArray::Sized(_))) => {
                return Err(self.error_at(
                    value_start,
                    "an array of transition bins has no size (IEEE 1800-2023 19.5.2)",
                ));
            }
            (BinsValue::DefaultSequence, Some(_)) => {
                return Err(self.error_at(
                    value_start,
                    "a `default sequence` bin is not an array (IEEE 1800-2023 19.5.2)",
                ));
            }
            (BinsValue::Default | BinsValue::DefaultSequence, _) if wildcard => {
                return Err(self.error_at(
                    value_start,
                    "a default bin is not a `wildcard` bin (IEEE 1800-2023 A.2.11)",
                ));
            }
            _ => {}
        }
        let iff = self.coverage_guard()?;
        self.expect(";")?;
        Ok(Bins {
            wildcard,
            kind,
            name,
            array,
            value,
            iff,
            tokens: self.range(start),
        })
    }

    /// What follows the `=` of a coverpoint bin.
    fn bins_value(&mut self) -> Result<BinsValue> {
        if self.eat("default") {
            return Ok(if self.eat("sequence") {
                BinsValue::DefaultSequence
            } else {
                BinsValue::Default
            });
        }
        if self.eat("{") {
            let ranges = self.coverage_ranges(CoverageOperand::None)?;
            self.expect("}")?;
            let with = if self.eat("with") {
                Some(self.parenthesized_expression()?)
            } else {
                None
            };
            return Ok(BinsValue::Ranges { ranges, with });
        }
        // A parenthesis always begins a transition list; whether `(e)` is
        // a parenthesized set expression instead takes the type of `e`
        // (19.5.2 makes a transition of one value illegal).
        if self.peek() == "(" {
            let mut sets = Vec::new();
            loop {
                sets.push(self.transition_set()?);
                if !self.eat(",") {
                    break;
                }
            }
            return Ok(BinsValue::Transitions(sets));
        }
        let identifier = self.token().is_some_and(|token| {
            matches!(
                token.kind,
                TokenKind::Identifier | TokenKind::EscapedIdentifier
            ) && !self.reserved_identifier(token)
        });
        if identifier && self.nth(1) == "with" && self.nth(2) == "(" {
            let coverpoint = self.name()?;
            self.at += 1;
            let with = self.parenthesized_expression()?;
            return Ok(BinsValue::CoverpointWith { coverpoint, with });
        }
        Ok(BinsValue::Set(self.expression(0)?))
    }

    /// A `covergroup_range_list` without its braces; its values end where
    /// `operand` says.
    fn coverage_ranges(&mut self, operand: CoverageOperand) -> Result<Vec<CoverageRange>> {
        let mut ranges = Vec::new();
        loop {
            let start = self.at;
            if matches!(self.peek(), "}" | ")" | "," | "=>") {
                return Err(self.error("expected a value or a value range"));
            }
            let kind = if self.eat("[") {
                // `$` is the lowest or the highest value (6.20.6), not an
                // operand.
                let low = if self.peek() == "$" && self.nth(1) == ":" {
                    self.at += 1;
                    None
                } else {
                    Some(self.expression(0)?)
                };
                let kind = if self.eat(":") {
                    let high = if self.peek() == "$" && self.nth(1) == "]" {
                        self.at += 1;
                        None
                    } else {
                        Some(self.expression(0)?)
                    };
                    if low.is_none() && high.is_none() {
                        return Err(
                            self.error("a value range needs a bound (IEEE 1800-2023 A.2.11)")
                        );
                    }
                    CoverageRangeKind::Range { low, high }
                } else if let Some(center) = low.filter(|_| matches!(self.peek(), "+/-" | "+%-")) {
                    let relative = self.peek() == "+%-";
                    self.at += 1;
                    let tolerance = self.expression(0)?;
                    if self.unbounded(center) || self.unbounded(tolerance) {
                        return Err(self.error(
                            "`$` is not an operand of a tolerance range (IEEE 1800-2023 A.2.11)",
                        ));
                    }
                    CoverageRangeKind::Tolerance {
                        center,
                        tolerance,
                        relative,
                    }
                } else {
                    return Err(self.error(format!(
                        "expected `:`, `+/-` or `+%-`, found `{}`",
                        self.peek()
                    )));
                };
                self.expect("]")?;
                kind
            } else {
                CoverageRangeKind::Value(self.coverage_expression(operand, 0)?)
            };
            ranges.push(CoverageRange {
                kind,
                tokens: self.range(start),
            });
            if !self.eat(",") {
                return Ok(ranges);
            }
        }
    }

    /// A `( trans_set )` (19.5.2): range lists joined by `=>`, each with an
    /// optional repetition that applies to the whole list.
    fn transition_set(&mut self) -> Result<TransitionSet> {
        let start = self.at;
        self.expect("(")?;
        let mut steps = Vec::new();
        loop {
            let step = self.at;
            let ranges = self.coverage_ranges(CoverageOperand::Transition)?;
            let repeat = if self.peek() == "[" {
                let repeat = self.at;
                self.at += 1;
                let kind = match self.peek() {
                    "*" => TransitionRepeatKind::Consecutive,
                    "->" => TransitionRepeatKind::Goto,
                    "=" => TransitionRepeatKind::NonConsecutive,
                    _ => return Err(self.error("expected `*`, `->` or `=`")),
                };
                self.at += 1;
                let low = self.expression(0)?;
                let high = if self.eat(":") {
                    Some(self.expression(0)?)
                } else {
                    None
                };
                self.expect("]")?;
                Some(TransitionRepeat {
                    kind,
                    low,
                    high,
                    tokens: self.range(repeat),
                })
            } else {
                None
            };
            steps.push(TransitionStep {
                ranges,
                repeat,
                tokens: self.range(step),
            });
            if !self.eat("=>") {
                break;
            }
        }
        self.expect(")")?;
        Ok(TransitionSet {
            steps,
            tokens: self.range(start),
        })
    }

    /// A `cover_cross` (19.6); `start` is where its attributes begin.
    fn cross(&mut self, start: usize, in_element: bool) -> Result<Cross> {
        let label = if self.peek() == "cross" {
            None
        } else {
            let label = self.name()?;
            self.expect(":")?;
            Some(label)
        };
        self.expect("cross")?;
        let mut items = vec![self.name()?];
        self.expect(",")?;
        items.push(self.name()?);
        while self.eat(",") {
            items.push(self.name()?);
        }
        let iff = self.coverage_guard()?;
        let mut body_items = Vec::new();
        let body = if self.eat(";") {
            None
        } else {
            let open = self.at;
            self.expect("{")?;
            loop {
                while self.coverage_directive(in_element)? {}
                if self.eat("}") {
                    break;
                }
                match self.peek() {
                    "" => return Err(self.error("expected `}`")),
                    // A function of the cross body is declared for its
                    // bins alone (19.6.1.4), so it joins no scope's
                    // subroutines.
                    "function" => {
                        body_items.push(CrossItem::Function(Box::new(self.subroutine_node(false)?)))
                    }
                    "task" => {
                        return Err(self.error(
                            "a cross body declares functions, not tasks (IEEE 1800-2023 A.2.11)",
                        ));
                    }
                    _ => {
                        let item = self.at;
                        self.attributes()?;
                        body_items.push(if self.coverage_option_start() {
                            CrossItem::Option(self.coverage_option(item)?)
                        } else {
                            CrossItem::Bins(self.cross_bins(item)?)
                        });
                    }
                }
            }
            Some(self.range(open))
        };
        Ok(Cross {
            label,
            items,
            iff,
            body_items,
            body,
            tokens: self.range(start),
        })
    }

    /// A `bins_selection` of a cross (19.6.1); `start` is where its
    /// attributes begin.
    fn cross_bins(&mut self, start: usize) -> Result<CrossBins> {
        if self.peek() == "wildcard" {
            return Err(self.error("a cross bin is not a `wildcard` bin (IEEE 1800-2023 A.2.11)"));
        }
        let kind = self.bins_keyword()?;
        let name = self.name()?;
        if self.peek() == "[" {
            return Err(self.error("a cross bin is not an array (IEEE 1800-2023 A.2.11)"));
        }
        self.expect("=")?;
        let select = self.select_expression()?;
        let iff = self.coverage_guard()?;
        self.expect(";")?;
        Ok(CrossBins {
            kind,
            name,
            select,
            iff,
            tokens: self.range(start),
        })
    }

    fn select_node(&mut self, tokens: TokenRange, kind: SelectKind) -> SelectId {
        let id = SelectId(self.coverage_selects.len());
        self.coverage_selects.push(SelectExpr { kind, tokens });
        id
    }

    /// The left-associative fold of `operands` with the operator `join`
    /// makes.
    fn select_fold(
        &mut self,
        operands: Vec<SelectId>,
        join: fn(SelectId, SelectId) -> SelectKind,
    ) -> SelectId {
        let mut operands = operands.into_iter();
        let mut folded = operands.next().expect("a select operand");
        for right in operands {
            let tokens = TokenRange {
                start: self.coverage_selects[folded.0].tokens.start,
                end: self.coverage_selects[right.0].tokens.end,
            };
            folded = self.select_node(tokens, join(folded, right));
        }
        folded
    }

    /// `[ matches integer_covergroup_expression ]` (19.6.1.2), whose `$`
    /// is every tuple.
    fn select_matches(&mut self) -> Result<Option<MatchCount>> {
        if !self.eat("matches") {
            return Ok(None);
        }
        if self.eat("$") {
            return Ok(Some(MatchCount::All));
        }
        Ok(Some(MatchCount::Value(self.expression(5)?)))
    }

    /// A select expression without `&&`, `||` and a postfix `with`: a
    /// select condition, or a cross identifier or cross set expression.
    fn select_operand(&mut self) -> Result<SelectId> {
        let start = self.at;
        let kind = if self.peek() == "binsof" || self.peek() == "!" && self.nth(1) == "binsof" {
            let negated = self.eat("!");
            self.expect("binsof")?;
            self.expect("(")?;
            let target = self.name()?;
            let bin = if self.eat(".") {
                Some(self.name()?)
            } else {
                None
            };
            self.expect(")")?;
            let intersect = if self.eat("intersect") {
                self.expect("{")?;
                let ranges = self.coverage_ranges(CoverageOperand::None)?;
                self.expect("}")?;
                Some(ranges)
            } else {
                None
            };
            if self.peek() == "matches" {
                return Err(self.error(
                    "`matches` follows a `with` selection or a cross set expression \
                     (IEEE 1800-2023 A.2.11)",
                ));
            }
            SelectKind::Binsof {
                negated,
                target,
                bin,
                intersect,
            }
        } else if self.peek() == "!" {
            // A.2.11 negates a select condition only.
            return Err(if self.nth(1) == "(" {
                self.unsupported(
                    "covergroup cross selection that negates a parenthesized selection (IEEE \
                     1800-2023 19.6.1)",
                )
            } else {
                self.error("`!` negates a `binsof` condition only (IEEE 1800-2023 A.2.11)")
            });
        } else {
            // `&&` and `||` join select expressions, so the expression
            // takes the operators that bind tighter.
            let value = self.coverage_expression(CoverageOperand::Select, 5)?;
            let matches = self.select_matches()?;
            SelectKind::Expression { value, matches }
        };
        let tokens = self.range(start);
        Ok(self.select_node(tokens, kind))
    }

    /// Whether select expression `id` is also an expression: operands that
    /// are expressions, joined by `&&`, `||` and parentheses.
    fn select_is_expression(&self, id: SelectId) -> bool {
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            match &self.coverage_selects[id.0].kind {
                SelectKind::Expression { matches: None, .. } => {}
                SelectKind::Group(inner) => pending.push(*inner),
                SelectKind::And { left, right } | SelectKind::Or { left, right } => {
                    pending.extend([*left, *right])
                }
                _ => return false,
            }
        }
        true
    }

    /// A `select_expression` (19.6.1, Syntax 19-4): `||` binds weaker than
    /// `&&`, which binds weaker than a postfix `with`; both fold to the
    /// left. Its groups nest on an explicit stack.
    fn select_expression(&mut self) -> Result<SelectId> {
        let mut frames = vec![SelectFrame {
            start: self.at,
            ors: Vec::new(),
            ands: Vec::new(),
        }];
        loop {
            while self.peek() == "(" {
                if frames.len() >= 1_000_000 {
                    return Err(self.unsupported(
                        "native select-expression stack exceeds one million entries",
                    ));
                }
                frames.push(SelectFrame {
                    start: self.at,
                    ors: Vec::new(),
                    ands: Vec::new(),
                });
                self.at += 1;
            }
            let mut operand = self.select_operand()?;
            // The postfix forms of the operand, then what joins it to the
            // next one or closes its group.
            loop {
                if self.peek() == "with" {
                    self.at += 1;
                    let with = self.parenthesized_expression()?;
                    let matches = self.select_matches()?;
                    let tokens = TokenRange {
                        start: self.coverage_selects[operand.0].tokens.start,
                        end: self.indices[self.at - 1] + 1,
                    };
                    operand = self.select_node(
                        tokens,
                        SelectKind::With {
                            select: operand,
                            with,
                            matches,
                        },
                    );
                    continue;
                }
                let and = |left, right| SelectKind::And { left, right };
                let frame = frames.last_mut().expect("a select frame");
                frame.ands.push(operand);
                if self.eat("&&") {
                    break;
                }
                if self.eat("||") {
                    let ands = std::mem::take(&mut frame.ands);
                    frame.ors.push(self.select_fold(ands, and));
                    break;
                }
                // A cross set expression is a `covergroup_expression`
                // (A.2.11), so it may be a conditional one, whose condition
                // has been read as a select expression: `c ? s : t`, `(c)
                // ? s : t`, `(c && d) ? s : t`.
                if self.peek() == "?" && self.select_is_expression(operand) {
                    return Err(self.unsupported(
                        "covergroup cross set expression with a conditional operator (IEEE \
                         1800-2023 19.6.1.4)",
                    ));
                }
                // Anything else ends the expression, or inside a group
                // has to close it.
                let group = frames.len() > 1;
                if group && self.peek() != ")" {
                    return Err(self.error(format!("expected `)`, found `{}`", self.peek())));
                }
                let SelectFrame {
                    start,
                    mut ors,
                    ands,
                } = frames.pop().expect("a select frame");
                ors.push(self.select_fold(ands, and));
                let folded = self.select_fold(ors, |left, right| SelectKind::Or { left, right });
                if !group {
                    return Ok(folded);
                }
                self.at += 1;
                let tokens = self.range(start);
                operand = self.select_node(tokens, SelectKind::Group(folded));
            }
        }
    }

    /// Record the nettype declaration from `start` through the `;` just
    /// read (IEEE 1800-2023 6.6.7): its name is the identifier before
    /// `with`, or before the `;`.
    pub(super) fn record_nettype(&mut self, start: usize) {
        let words = (start..self.at)
            .map(|at| &self.tokens[self.indices[at]])
            .collect::<Vec<_>>();
        let mut depth = 0usize;
        let mut end = words.len().saturating_sub(1);
        for (index, word) in words.iter().enumerate() {
            match word.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                "with" if depth == 0 => {
                    end = index;
                    break;
                }
                _ => {}
            }
        }
        let Some(name) = end.checked_sub(1).and_then(|at| words.get(at)) else {
            return;
        };
        if !matches!(
            name.kind,
            crate::TokenKind::Identifier | crate::TokenKind::EscapedIdentifier
        ) {
            return;
        }
        let name = Name {
            spelling: name.text.clone(),
            origin: name.origin,
        };
        let tokens = self.range(start);
        self.nettypes.push(NettypeDeclaration { name, tokens });
    }
}
