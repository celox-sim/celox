//! Clocking declarations (IEEE 1800-2023 clause 14, A.6.11) as typed
//! nodes, each read by one call from its first keyword through its
//! `endclocking`, or through the `;` of a default clocking specification.
use super::*;

impl Parser<'_> {
    /// Whether a clocking declaration or default clocking specification
    /// starts at the cursor. `default disable iff` is not one.
    pub(super) fn clocking_start(&self) -> bool {
        match self.peek() {
            "clocking" => true,
            "default" | "global" => self.nth(1) == "clocking",
            _ => false,
        }
    }

    /// The clocking declaration or default clocking specification at the
    /// cursor.
    pub(super) fn clocking_declaration(&mut self) -> Result<ClockingId> {
        let start = self.at;
        let kind = if self.eat("global") {
            ClockingKind::Global
        } else {
            ClockingKind::Block {
                default: self.eat("default"),
            }
        };
        self.expect("clocking")?;
        let name = if self.peek() == "@" {
            None
        } else {
            Some(self.name()?)
        };
        // `default clocking name ;` (14.12).
        if kind == (ClockingKind::Block { default: true }) && self.peek() == ";" {
            let Some(name) = name else {
                return Err(self.error("expected a clocking block name"));
            };
            self.at += 1;
            return Ok(self.push_clocking(ClockingDeclaration {
                kind: ClockingKind::DefaultReference,
                name: Some(name),
                event: None,
                items: Vec::new(),
                end_label: None,
                tokens: self.range(start),
            }));
        }
        if name.is_none() && kind == (ClockingKind::Block { default: false }) {
            return Err(
                self.error("only a default clocking block may be unnamed (IEEE 1800-2023 14.3)")
            );
        }
        if self.peek() != "@" {
            return Err(self.error("expected the clocking event"));
        }
        let event = self.typed_timing_control()?;
        if matches!(
            self.timing_controls[event.0].kind,
            TimingControlKind::Event(EventControl::Wildcard)
        ) {
            return Err(self.error("a clocking event cannot be a wildcard event"));
        }
        self.expect(";")?;
        let mut items = Vec::new();
        loop {
            while self.directive()? {}
            match self.peek() {
                "" => return Err(self.error("expected `endclocking`")),
                "endclocking" => break,
                _ => {}
            }
            self.attributes()?;
            items.push(self.clocking_item()?);
            if kind == ClockingKind::Global {
                return Err(self.error_at(
                    self.range(start),
                    "a global clocking declaration has no clocking items (IEEE 1800-2023 14.14)",
                ));
            }
        }
        self.expect("endclocking")?;
        let end_label = self.end_label(name.as_ref())?;
        Ok(self.push_clocking(ClockingDeclaration {
            kind,
            name,
            event: Some(event),
            items,
            end_label,
            tokens: self.range(start),
        }))
    }

    fn push_clocking(&mut self, declaration: ClockingDeclaration) -> ClockingId {
        let id = ClockingId(self.clockings.len());
        self.clockings.push(declaration);
        id
    }

    fn clocking_item(&mut self) -> Result<ClockingItem> {
        let start = self.at;
        match self.peek() {
            "default" => {
                self.at += 1;
                let mut input = None;
                let mut output = None;
                if self.eat("input") {
                    input = Some(
                        self.clocking_skew()?
                            .ok_or_else(|| self.error("expected the default input skew"))?,
                    );
                }
                if self.eat("output") {
                    output = Some(
                        self.clocking_skew()?
                            .ok_or_else(|| self.error("expected the default output skew"))?,
                    );
                }
                if input.is_none() && output.is_none() {
                    return Err(self.error("expected `input` or `output` after `default`"));
                }
                self.expect(";")?;
                Ok(ClockingItem::DefaultSkew {
                    input,
                    output,
                    tokens: self.range(start),
                })
            }
            "property" | "sequence" => {
                let end = if self.peek() == "property" {
                    "endproperty"
                } else {
                    "endsequence"
                };
                while !self.eat(end) {
                    if self.peek().is_empty() || self.peek() == "endclocking" {
                        return Err(self.error(format!("expected `{end}`")));
                    }
                    self.at += 1;
                }
                if self.eat(":") {
                    self.name()?;
                }
                Ok(ClockingItem::Assertion {
                    tokens: self.range(start),
                })
            }
            "let" => {
                while !self.eat(";") {
                    if self.peek().is_empty() || self.peek() == "endclocking" {
                        return Err(self.error("expected `;`"));
                    }
                    self.at += 1;
                }
                Ok(ClockingItem::Assertion {
                    tokens: self.range(start),
                })
            }
            _ => {
                let direction = if self.eat("inout") {
                    ClockingDirection::Inout
                } else if self.eat("input") {
                    let input = self.clocking_skew()?;
                    if self.eat("output") {
                        ClockingDirection::InputOutput(input, self.clocking_skew()?)
                    } else {
                        ClockingDirection::Input(input)
                    }
                } else if self.eat("output") {
                    ClockingDirection::Output(self.clocking_skew()?)
                } else {
                    return Err(self.error("expected a clocking item"));
                };
                let mut signals = Vec::new();
                loop {
                    let signal = self.at;
                    let name = self.name()?;
                    let expression = if self.eat("=") {
                        Some(self.expression(0)?)
                    } else {
                        None
                    };
                    signals.push(ClockingSignal {
                        name,
                        expression,
                        tokens: self.range(signal),
                    });
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect(";")?;
                Ok(ClockingItem::Signals {
                    direction,
                    signals,
                    tokens: self.range(start),
                })
            }
        }
    }

    /// The `clocking_skew` at the cursor, if any (A.6.11).
    fn clocking_skew(&mut self) -> Result<Option<ClockingSkew>> {
        let start = self.at;
        let edge = match self.peek() {
            "posedge" => Some(TimingEdge::Pos),
            "negedge" => Some(TimingEdge::Neg),
            "edge" => Some(TimingEdge::Both),
            _ => None,
        };
        if edge.is_some() {
            self.at += 1;
        }
        let delay = if self.peek() != "#" {
            None
        } else if self.nth(1) == "1" && self.nth(2) == "step" {
            // `1step` lexes as the number `1` and the identifier `step`.
            self.at += 3;
            Some(SkewDelay::Step)
        } else {
            let control = self.typed_timing_control()?;
            let TimingControlKind::Delay { value, .. } = self.timing_controls[control.0].kind
            else {
                return Err(self.error("expected a skew delay"));
            };
            Some(SkewDelay::Value(value))
        };
        if edge.is_none() && delay.is_none() {
            return Ok(None);
        }
        Ok(Some(ClockingSkew {
            edge,
            delay,
            tokens: self.range(start),
        }))
    }
}
