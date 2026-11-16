//! Native n-input, n-output and three-state gate declarations (IEEE
//! 1800-2023 28.4, 28.5, 28.6) retain terminals and unsupported modifiers.
use super::*;

impl Parser<'_> {
    pub(super) fn logic_gate_start(&self) -> bool {
        matches!(
            self.peek(),
            "and"
                | "nand"
                | "or"
                | "nor"
                | "xor"
                | "xnor"
                | "buf"
                | "not"
                | "bufif0"
                | "bufif1"
                | "notif0"
                | "notif1"
        )
    }

    pub(super) fn logic_gate(&mut self) -> Result<LogicGate> {
        let kind = self.peek();
        let kind = kind.strip_prefix('\\').unwrap_or(kind).to_owned();
        self.at += 1;
        let strength = self.drive_strength()?;
        let delay = if self.peek() == "#" && self.nth(1) != "(" {
            let start = self.at;
            self.at += 1;
            let value_start = self.at;
            let mut value = if self
                .token()
                .is_some_and(|token| token.kind == TokenKind::Number)
            {
                let text = self.peek().to_owned();
                if !super::numbers::literal(&text) {
                    return Err(self.error("invalid gate delay literal"));
                }
                self.at += 1;
                self.node(value_start, ExprKind::Literal(text))?
            } else {
                let name = self.name()?;
                self.node(value_start, ExprKind::Name(name))?
            };
            while self.eat("::") {
                let name = self.name()?;
                value = self.node(
                    value_start,
                    ExprKind::Member {
                        expression: value,
                        separator: "::".into(),
                        name,
                    },
                )?;
            }
            Some(AssignmentDelay {
                values: vec![value],
                parenthesized: false,
                tokens: self.range(start),
            })
        } else {
            self.assignment_delay()?
        };
        // A three-state gate also has a delay of transitions to z (28.6).
        let three_state = matches!(kind.as_str(), "bufif0" | "bufif1" | "notif0" | "notif1");
        let (most, words) = if three_state {
            (3, "three")
        } else {
            (2, "two")
        };
        if delay
            .as_ref()
            .is_some_and(|delay| delay.values.len() > most)
        {
            return Err(self.error(format!("{kind} gate delay has more than {words} values")));
        }
        // An n-output gate's terminals are its outputs and then one input.
        let outputs = matches!(kind.as_str(), "buf" | "not");
        let mut instances = Vec::new();
        loop {
            let start = self.at;
            let name = if self.peek() == "(" {
                None
            } else {
                Some(self.name()?)
            };
            let dimensions = self.dimensions()?;
            self.validate_unpacked_dimensions(&dimensions)?;
            self.expect("(")?;
            if matches!(self.peek(), "," | ")" | ";") {
                return Err(self.error("expected gate output terminal"));
            }
            let output = self.expression(0)?;
            if !self.variable_lvalue(output) {
                return Err(self.error("expected gate output lvalue"));
            }
            self.expect(",")?;
            let mut terminals = vec![output];
            loop {
                if matches!(self.peek(), "," | ")" | ";") {
                    return Err(self.error("expected gate input terminal"));
                }
                let terminal = self.expression(0)?;
                if outputs && self.peek() == "," && !self.variable_lvalue(terminal) {
                    return Err(self.error_at(
                        self.expressions[terminal.0].tokens,
                        "expected gate output lvalue",
                    ));
                }
                terminals.push(terminal);
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
            }
            // A three-state gate has one output, one data input and one
            // control input (28.6).
            if three_state && terminals.len() != 3 {
                return Err(self.error_at(
                    self.range(start),
                    format!("{kind} gate needs an output, a data and a control terminal"),
                ));
            }
            instances.push(LogicGateInstance {
                name,
                dimensions,
                terminals,
                tokens: self.range(start),
            });
            if self.eat(";") {
                break;
            }
            self.expect(",")?;
        }
        Ok(LogicGate {
            kind,
            strength,
            delay,
            instances,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn property_and_sequence_operators_are_not_gate_declarations() {
        for declaration in [
            "property p; 1'b1 and 1'b1 or 1'b0; endproperty",
            "sequence s; 1'b1 and 1'b1 or 1'b0; endsequence",
            "checker c(input a,b); property p; a and b or a; endproperty endchecker",
            "checker c(input a,b); sequence s; a and b or a; endsequence endchecker",
        ] {
            let source = format!(
                "{declaration} module Top(input a,b, output wire y); and (y,a,b); endmodule"
            );
            let tree = Declarations::parse(&source, Path::new("property.sv")).unwrap();
            assert!(
                tree.units[0]
                    .items
                    .iter()
                    .any(|&id| matches!(tree.body_item(id).kind, BodyItemKind::LogicGate(_)))
            );
        }
    }

    #[test]
    fn native_gates_preserve_modifiers_instances_and_terminal_order() {
        let tree = Declarations::parse("module M(input a,b, output y,z); and (strong1,weak0) #(1,2) named[1:0](y,a,b), (z,b); endmodule", Path::new("gates.sv")).unwrap();
        let gate = tree.units[0]
            .items
            .iter()
            .find_map(|&id| match &tree.body_item(id).kind {
                BodyItemKind::LogicGate(gate) => Some(gate),
                _ => None,
            })
            .unwrap();
        assert_eq!(gate.kind, "and");
        assert!(gate.strength.is_some());
        assert_eq!(gate.delay.as_ref().unwrap().values.len(), 2);
        assert_eq!(gate.instances.len(), 2);
        assert_eq!(gate.instances[0].name.as_ref().unwrap().value(), "named");
        assert_eq!(gate.instances[0].dimensions.len(), 1);
        assert_eq!(gate.instances[0].terminals.len(), 3);
        assert!(gate.instances[1].name.is_none());
        assert_eq!(gate.instances[1].terminals.len(), 2);
    }
    #[test]
    fn bare_gate_delays_do_not_consume_the_terminal_list_as_a_call() {
        for delay in ["1", "2ns", "D", "pkg::D"] {
            let source = format!("module M(input a,b,output y); and #{delay} (y,a,b); endmodule");
            let tree = Declarations::parse(&source, Path::new("delay.sv")).unwrap();
            let gate = tree.units[0]
                .items
                .iter()
                .find_map(|&id| match &tree.body_item(id).kind {
                    BodyItemKind::LogicGate(gate) => Some(gate),
                    _ => None,
                })
                .unwrap();
            assert_eq!(gate.instances[0].terminals.len(), 3);
            assert!(!gate.delay.as_ref().unwrap().parenthesized);
        }
    }
    #[test]
    fn three_state_gates_have_three_terminals_and_up_to_three_delays() {
        let parse = |gate: &str| {
            Declarations::parse(
                &format!("module M(input d,c, output y); {gate}; endmodule"),
                Path::new("three_state.sv"),
            )
        };
        for kind in ["bufif0", "bufif1", "notif0", "notif1"] {
            assert!(parse(&format!("{kind} #(1,2,3) g(y,d,c)")).is_ok());
            assert!(parse(&format!("{kind} g(y,d)")).is_err());
            assert!(parse(&format!("{kind} g(y,d,c,d)")).is_err());
            assert!(parse(&format!("{kind} #(1,2,3,4) g(y,d,c)")).is_err());
        }
        assert!(parse("and #(1,2,3) g(y,d,c)").is_err());
    }
    #[test]
    fn gate_terminals_declare_implicit_nets_and_honor_default_nettype() {
        for prefix in ["", "`default_nettype none\n"] {
            let parsed = Declarations::parse(
                &format!("{prefix}module M; and g(y,a,b); endmodule"),
                Path::new("nets.sv"),
            );
            if prefix.is_empty() {
                assert_eq!(parsed.unwrap().net_declarations.len(), 3);
            } else {
                assert!(parsed.is_err());
            }
        }
    }
}
