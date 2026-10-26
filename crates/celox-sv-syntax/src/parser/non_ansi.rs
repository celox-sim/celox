//! Non-ANSI module ports (IEEE 1800-2023 23.2.2.1).
//!
//! A non-ANSI header lists the ports; port declarations in the body give
//! their direction and type, and a net or variable declaration may give the
//! type of a port whose port declaration has none. When every header port is
//! a simple identifier, the declarations are folded into the header ports and
//! the unit takes the ANSI form, so that semantic consumers see one port
//! declaration per port. Other port expressions keep the non-ANSI form.
use super::*;

/// A port declaration of a non-ANSI module body.
struct PortDeclaration {
    direction: Direction,
    net_type: Option<String>,
    variable: bool,
    data_type: TypeId,
    dimensions: Vec<Dimension>,
    default: Option<ExprId>,
    tokens: TokenRange,
}

impl Parser<'_> {
    /// Fold the body port declarations of a non-ANSI `unit` into its header.
    pub(super) fn adopt_non_ansi_ports(&mut self, unit: &mut Unit) -> Result<()> {
        if unit.port_style != PortStyle::NonAnsi
            || !unit
                .ports
                .iter()
                .all(|port| matches!(port.kind, PortKind::Named(_)))
        {
            return Ok(());
        }
        let resume = self.at;
        let mut declarations: Vec<(String, PortDeclaration)> = Vec::new();
        let mut items = Vec::with_capacity(unit.items.len());
        for &item in &unit.items {
            let BodyItem { kind, tokens } = &self.body_items[item.0];
            let BodyItemKind::Unconverted { category, .. } = kind else {
                items.push(item);
                continue;
            };
            let direction = match category.as_str() {
                "input" => Direction::Input,
                "output" => Direction::Output,
                "inout" => Direction::Inout,
                "ref" => Direction::Ref,
                _ => {
                    items.push(item);
                    continue;
                }
            };
            let tokens = *tokens;
            self.at = self.indices.partition_point(|&index| index < tokens.start);
            for (name, declaration) in self.port_declaration(direction)? {
                if declarations.iter().any(|(declared, _)| *declared == name) {
                    self.at = resume;
                    return Err(self.error(format!("port `{name}` is declared more than once")));
                }
                declarations.push((name, declaration));
            }
        }
        for port in &mut unit.ports {
            let PortKind::Named(name) = &port.kind else {
                unreachable!("checked above")
            };
            let name = name.value().to_string();
            let Some(index) = declarations
                .iter()
                .position(|(declared, _)| *declared == name)
            else {
                self.at = resume;
                return Err(self.error(format!("port `{name}` has no port declaration")));
            };
            let (_, declaration) = declarations.swap_remove(index);
            let (net_type, variable, data_type) = if declaration.net_type.is_some()
                || declaration.variable
                || !matches!(
                    self.types[declaration.data_type.0].kind,
                    DataTypeKind::Implicit
                ) {
                // The port is completely declared (IEEE 1800-2023 23.2.2.1).
                if self.declares_object(&items, &name) {
                    self.at = resume;
                    return Err(self.error(format!(
                        "port `{name}` with a net or variable type is declared again"
                    )));
                }
                (
                    declaration.net_type,
                    declaration.variable,
                    declaration.data_type,
                )
            } else {
                // The port is completed by a net or variable declaration of
                // the same name, if there is one.
                match self.take_port_object(
                    &mut items,
                    (
                        &mut unit.net_declarations,
                        &mut unit.data_declarations,
                        &mut unit.assign_statements,
                    ),
                    &name,
                    declaration.data_type,
                )? {
                    Some(object) => object,
                    None => (None, false, declaration.data_type),
                }
            };
            port.direction = Some(declaration.direction);
            port.net_type = net_type;
            port.variable = variable;
            port.data_type = Some(data_type);
            port.dimensions = declaration.dimensions;
            port.default = declaration.default;
            port.tokens = declaration.tokens;
        }
        self.at = resume;
        if let Some((name, _)) = declarations.first() {
            return Err(self.error(format!("port declaration `{name}` names no port")));
        }
        unit.items = items;
        unit.port_style = PortStyle::Ansi;
        Ok(())
    }

    /// The names a port declaration starting at the cursor declares.
    fn port_declaration(&mut self, direction: Direction) -> Result<Vec<(String, PortDeclaration)>> {
        let start = self.at;
        self.attributes()?;
        self.at += 1;
        let net_type = if matches!(
            self.peek(),
            "wire"
                | "tri"
                | "tri0"
                | "tri1"
                | "supply0"
                | "supply1"
                | "wand"
                | "triand"
                | "wor"
                | "trior"
                | "trireg"
                | "uwire"
                | "interconnect"
        ) {
            let word = self.peek().to_owned();
            self.at += 1;
            Some(word)
        } else {
            None
        };
        let variable = self.eat("var");
        let data_type = if self.starts_type() {
            self.data_type()?
        } else {
            self.implicit_type()?
        };
        let mut declared = Vec::new();
        loop {
            let name = self.name()?;
            let dimensions = self.dimensions()?;
            let default = if self.eat("=") {
                Some(self.expression(0)?)
            } else {
                None
            };
            declared.push((
                name.value().to_string(),
                PortDeclaration {
                    direction,
                    net_type: net_type.clone(),
                    variable,
                    data_type,
                    dimensions,
                    default,
                    tokens: self.range(start),
                },
            ));
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")?;
        Ok(declared)
    }

    /// Remove the declarator of `name` from the module-scope net or variable
    /// declarations, returning the port's net type, `var` and type. The
    /// port declaration's packed range and signing carry over to a type
    /// that has neither (IEEE 1800-2023 23.2.2.1). A net declaration
    /// assignment stays as the continuous assignment it is (10.3.1).
    fn take_port_object(
        &mut self,
        items: &mut Vec<BodyItemId>,
        (unit_nets, unit_data, unit_assigns): (
            &mut Vec<NetDeclarationId>,
            &mut Vec<DataDeclarationId>,
            &mut Vec<AssignId>,
        ),
        name: &str,
        port_type: TypeId,
    ) -> Result<Option<(Option<String>, bool, TypeId)>> {
        let qualified = items
            .iter()
            .any(|&item| match self.body_items[item.0].kind {
                BodyItemKind::Net(id) => {
                    let net = &self.net_declarations[id.0];
                    (net.strength.is_some() || net.delay.is_some() || net.vector.is_some())
                        && net
                            .variables
                            .iter()
                            .any(|variable| variable.name.value() == name)
                }
                _ => false,
            });
        if qualified {
            return Err(self.error(format!(
                "net declaration of non-ANSI port `{name}` with a strength, delay or \
                 `vectored`/`scalared`"
            )));
        }
        let found = items.iter().enumerate().find_map(|(position, &item)| {
            let (declarators, object) = match self.body_items[item.0].kind {
                BodyItemKind::Net(id) => {
                    let net = &self.net_declarations[id.0];
                    if net.strength.is_some() || net.delay.is_some() || net.vector.is_some() {
                        return None;
                    }
                    (
                        &net.variables,
                        (Some(net.net_type.clone()), false, net.data_type),
                    )
                }
                BodyItemKind::Data(id) => match &self.data_declarations[id.0] {
                    DataDeclaration::Variables(variables) if !variables.constant => {
                        (&variables.variables, (None, true, variables.data_type))
                    }
                    _ => return None,
                },
                _ => return None,
            };
            let index = declarators
                .iter()
                .position(|declarator| declarator.name.value() == name)?;
            let declarator = declarators[index].clone();
            Some((
                position,
                item,
                index,
                object,
                declarator,
                declarators.len() == 1,
            ))
        });
        let Some((position, item, index, (net_type, variable, object_type), declarator, last)) =
            found
        else {
            return Ok(None);
        };
        if !declarator.dimensions.is_empty() || (variable && declarator.value.is_some()) {
            return Err(self.error(format!(
                "variable declaration of non-ANSI port `{name}` with an initializer, or net or \
                 variable declaration of it with unpacked dimensions"
            )));
        }
        let assign = match declarator.value {
            Some(right) => Some(self.declaration_assignment(&declarator, right)?),
            None => None,
        };
        if let Some((id, _)) = assign {
            unit_assigns.push(id);
        }
        match self.body_items[item.0].kind {
            BodyItemKind::Net(id) => {
                self.net_declarations[id.0].variables.remove(index);
                if last {
                    unit_nets.retain(|&net| net != id);
                }
            }
            BodyItemKind::Data(id) => {
                if let DataDeclaration::Variables(variables) = &mut self.data_declarations[id.0] {
                    variables.variables.remove(index);
                }
                if last {
                    unit_data.retain(|&data| data != id);
                }
            }
            _ => unreachable!("found above"),
        }
        match (assign, last) {
            (Some((_, assign)), true) => items[position] = assign,
            (Some((_, assign)), false) => items.insert(position + 1, assign),
            (None, true) => {
                items.remove(position);
            }
            (None, false) => {}
        }
        Ok(Some((
            net_type,
            variable,
            self.merge_port_type(port_type, object_type)?,
        )))
    }

    /// The continuous assignment `name = right` of a net declaration
    /// assignment, as an assignment and its body item.
    fn declaration_assignment(
        &mut self,
        declarator: &VariableDeclarator,
        right: ExprId,
    ) -> Result<(AssignId, BodyItemId)> {
        let start = self
            .indices
            .partition_point(|&index| index < declarator.tokens.start);
        self.at = start + 1;
        let left = self.node(start, ExprKind::Name(declarator.name.clone()))?;
        let id = AssignId(self.assign_statements.len());
        self.assign_statements.push(AssignSyntax {
            strength: None,
            delay: None,
            assignments: vec![AssignmentSyntax {
                left,
                right,
                tokens: declarator.tokens,
            }],
            tokens: declarator.tokens,
        });
        if self.body_items.len() >= 1_000_000 {
            return Err(self.unsupported("native body-item arena exceeds one million entries"));
        }
        let item = BodyItemId(self.body_items.len());
        self.body_items.push(BodyItem {
            kind: BodyItemKind::Assign(id),
            tokens: declarator.tokens,
        });
        Ok((id, item))
    }

    /// Whether a net or variable declaration among `items` declares `name`.
    fn declares_object(&self, items: &[BodyItemId], name: &str) -> bool {
        items.iter().any(|&item| {
            let declarators = match self.body_items[item.0].kind {
                BodyItemKind::Net(id) => &self.net_declarations[id.0].variables,
                BodyItemKind::Data(id) => match &self.data_declarations[id.0] {
                    DataDeclaration::Variables(variables)
                    | DataDeclaration::Constructed(variables) => &variables.variables,
                    DataDeclaration::Unconverted(_) => return false,
                },
                _ => return false,
            };
            declarators
                .iter()
                .any(|declarator| declarator.name.value() == name)
        })
    }

    /// The type of a port declared by a port declaration of type
    /// `port_type` and a net or variable declaration of type `object_type`.
    fn merge_port_type(&mut self, port_type: TypeId, object_type: TypeId) -> Result<TypeId> {
        let port = self.types[port_type.0].clone();
        let mut object = self.types[object_type.0].clone();
        let mut changed = false;
        if object.packed_dimensions.is_empty() {
            changed |= !port.packed_dimensions.is_empty();
            object.packed_dimensions = port.packed_dimensions.clone();
        } else {
            // A vector net or variable repeats the port's range exactly.
            let text = |dimensions: &[Dimension]| {
                dimensions
                    .iter()
                    .map(|dimension| {
                        self.tokens[dimension.tokens.range()]
                            .iter()
                            .filter(|token| !token.is_trivia())
                            .map(|token| token.text.as_str())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
            };
            if text(&port.packed_dimensions) != text(&object.packed_dimensions) {
                return Err(self.error(
                    "the ranges of a non-ANSI port declaration and its net or variable \
                     declaration differ",
                ));
            }
        }
        if port.signed == Some(true) && object.signed != Some(true) {
            object.signed = Some(true);
            changed = true;
        }
        if !changed {
            return Ok(object_type);
        }
        if self.types.len() >= 1_000_000 {
            return Err(self.unsupported("native type arena exceeds one million nodes"));
        }
        let id = TypeId(self.types.len());
        self.types.push(object);
        Ok(id)
    }
}
