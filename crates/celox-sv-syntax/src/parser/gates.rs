//! Native n-input gate declarations retain terminals and unsupported modifiers.
use super::*;

impl Parser<'_> {
    pub(super) fn logic_gate_start(&self) -> bool {
        matches!(self.peek(), "and" | "nand" | "or" | "nor" | "xor" | "xnor")
    }

    pub(super) fn logic_gate(&mut self) -> Result<LogicGate> {
        let kind = self.peek().to_owned();
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
        if delay.as_ref().is_some_and(|delay| delay.values.len() > 2) {
            return Err(self.error("n-input gate delay has more than two values"));
        }
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
                terminals.push(self.expression(0)?);
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
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
