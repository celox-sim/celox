//! Interface modport ownership (IEEE 1800-2023 25.5).
use super::*;

impl Parser<'_> {
    fn modport_group_start(&self) -> bool {
        matches!(
            self.peek(),
            "input" | "output" | "inout" | "ref" | "import" | "export" | "clocking" | "(*"
        )
    }

    pub(super) fn modport_declaration(&mut self) -> Result<ModportDeclarationId> {
        let start = self.at;
        self.expect("modport")?;
        let mut modports = Vec::new();
        loop {
            let item_start = self.at;
            let name = self.name()?;
            self.expect("(")?;
            let mut ports = Vec::new();
            loop {
                self.attributes()?;
                let group = match self.peek() {
                    "input" | "output" | "inout" | "ref" => {
                        let direction = match self.peek() {
                            "input" => Direction::Input,
                            "output" => Direction::Output,
                            "inout" => Direction::Inout,
                            _ => Direction::Ref,
                        };
                        self.at += 1;
                        let mut members = Vec::new();
                        loop {
                            let member = if self.eat(".") {
                                let name = self.name()?;
                                self.expect("(")?;
                                let expression = if self.peek() == ")" {
                                    None
                                } else {
                                    Some(self.expression(0)?)
                                };
                                self.expect(")")?;
                                ModportMember::Expression { name, expression }
                            } else {
                                ModportMember::Named(self.name()?)
                            };
                            members.push(member);
                            if self.peek() != ","
                                || matches!(
                                    self.nth(1),
                                    "input"
                                        | "output"
                                        | "inout"
                                        | "ref"
                                        | "import"
                                        | "export"
                                        | "clocking"
                                        | "(*"
                                )
                            {
                                break;
                            }
                            self.at += 1;
                        }
                        ModportPorts::Members {
                            direction,
                            ports: members,
                        }
                    }
                    "import" | "export" => {
                        let export = self.eat("export");
                        if !export {
                            self.expect("import")?;
                        }
                        let mut subroutines = Vec::new();
                        loop {
                            let header_start = self.at;
                            let port = if matches!(self.peek(), "function" | "task") {
                                let task = self.eat("task");
                                if !task {
                                    self.expect("function")?;
                                }
                                let return_type = if !task && self.subroutine_return_type() {
                                    Some(self.data_type()?)
                                } else {
                                    None
                                };
                                let name = self.name()?;
                                let ports = if self.peek() == "(" {
                                    Some(self.subroutine_ports(true)?)
                                } else {
                                    None
                                };
                                ModportSubroutine::Prototype {
                                    kind: if task {
                                        SubroutineKind::Task
                                    } else {
                                        SubroutineKind::Function
                                    },
                                    return_type,
                                    name,
                                    ports,
                                    tokens: self.range(header_start),
                                }
                            } else {
                                ModportSubroutine::Named(self.name()?)
                            };
                            subroutines.push(port);
                            if self.peek() != ","
                                || matches!(
                                    self.nth(1),
                                    "input"
                                        | "output"
                                        | "inout"
                                        | "ref"
                                        | "import"
                                        | "export"
                                        | "clocking"
                                        | "(*"
                                )
                            {
                                break;
                            }
                            self.at += 1;
                        }
                        ModportPorts::Subroutines {
                            export,
                            ports: subroutines,
                        }
                    }
                    "clocking" => {
                        self.at += 1;
                        ModportPorts::Clocking(self.name()?)
                    }
                    _ => {
                        return Err(self.error(
                            "expected modport direction, import/export or clocking declaration",
                        ));
                    }
                };
                ports.push(group);
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
                if !self.modport_group_start() {
                    return Err(self.error("expected modport declaration group"));
                }
            }
            modports.push(Modport {
                name,
                ports,
                tokens: self.range(item_start),
            });
            if modports.len() >= 1_000_000 {
                return Err(
                    self.unsupported("native modport declaration exceeds one million items")
                );
            }
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")?;
        let id = ModportDeclarationId(self.modport_declarations.len());
        self.modport_declarations.push(ModportDeclaration {
            modports,
            tokens: self.range(start),
        });
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_modports_preserve_groups_members_and_declaration_owners() {
        let tree = Declarations::parse(
            "interface I;logic a,b,c;modport r(input a,b,output c,import f,g),w(output a,b,input c);endinterface",
            Path::new("native-modports.sv"),
        ).unwrap();
        let BodyItemKind::Modport(id) = tree.body_item(tree.units[0].items[1]).kind else {
            panic!()
        };
        let declaration = tree.modport_declaration(id);
        assert_eq!(declaration.modports.len(), 2);
        let reader = &declaration.modports[0];
        assert_eq!(reader.name.value(), "r");
        assert!(
            matches!(&reader.ports[0], ModportPorts::Members { direction: Direction::Input, ports } if ports.len()==2 && matches!(&ports[1],ModportMember::Named(name) if name.value()=="b"))
        );
        assert!(
            matches!(&reader.ports[1], ModportPorts::Members { direction: Direction::Output, ports } if ports.len()==1)
        );
        assert!(
            matches!(&reader.ports[2], ModportPorts::Subroutines { export: false, ports } if ports.len()==2 && matches!(&ports[1],ModportSubroutine::Named(name) if name.value()=="g"))
        );
        assert_eq!(
            tree.token_text(declaration.modports[1].tokens),
            "w(output a,b,input c)"
        );
    }

    #[test]
    fn unsupported_modport_contracts_keep_distinct_native_boundaries() {
        let tree = Declarations::parse(
            "interface I;logic a;modport m(ref a,inout .x(a+1),input .unused(),export task t(input int x),import function int f(int),clocking cb);endinterface",
            Path::new("native-modport-boundaries.sv"),
        ).unwrap();
        let ports = &tree.modport_declarations[0].modports[0].ports;
        assert!(matches!(
            &ports[0],
            ModportPorts::Members {
                direction: Direction::Ref,
                ..
            }
        ));
        let ModportPorts::Members {
            direction: Direction::Inout,
            ports: explicit,
        } = &ports[1]
        else {
            panic!()
        };
        assert!(
            matches!(&explicit[0],ModportMember::Expression { expression: Some(id),.. } if matches!(tree.expression(*id).kind,ExprKind::Binary {..}))
        );
        assert!(
            matches!(&ports[2],ModportPorts::Members { ports,.. } if matches!(&ports[0],ModportMember::Expression { expression: None,.. }))
        );
        assert!(
            matches!(&ports[3],ModportPorts::Subroutines { export:true,ports } if matches!(&ports[0],ModportSubroutine::Prototype { kind:SubroutineKind::Task,ports:Some(arguments),.. } if arguments.len()==1))
        );
        assert!(
            matches!(&ports[4],ModportPorts::Subroutines { export:false,ports } if matches!(&ports[0],ModportSubroutine::Prototype { kind:SubroutineKind::Function,return_type:Some(_),.. }))
        );
        assert!(matches!(&ports[5],ModportPorts::Clocking(name) if name.value()=="cb"));
        assert!(tree.subroutines.is_empty());
    }
}
