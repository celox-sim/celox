//! Non-ANSI module ports (IEEE 1800-2023 23.2.2.1).
//!
//! A non-ANSI header lists the ports; port declarations in the body give
//! their direction and type, and a net or variable declaration may give the
//! type of a port whose port declaration has none. The declarations are
//! folded into the header ports and the unit takes the ANSI form, so that
//! semantic consumers see one port declaration per port: a header port that
//! is a simple identifier takes the declaration of its name, and any other
//! becomes the explicit port of its expression (23.2.2.2), whose referenced
//! objects the port declarations declare.
use super::*;

/// A port declaration of a non-ANSI module body.
struct PortDeclaration {
    name: Name,
    constant: bool,
    direction: Direction,
    net_type: Option<String>,
    variable: bool,
    data_type: TypeId,
    dimensions: Vec<Dimension>,
    default: Option<ExprId>,
    tokens: TokenRange,
}

/// The net type, `var`, type, port declaration type to check (see
/// [`Parser::take_port_object`]) and initializer of the net or variable
/// declaration of a non-ANSI port.
type PortObject = (Option<String>, bool, TypeId, Option<TypeId>, Option<ExprId>);

/// The net and data declarations and continuous assignments of a unit.
type UnitLists<'u> = (
    &'u mut Vec<NetDeclarationId>,
    &'u mut Vec<DataDeclarationId>,
    &'u mut Vec<AssignId>,
);

/// The lists of `unit` that a port declaration's net or variable declaration
/// changes.
fn unit_lists(unit: &mut Unit) -> UnitLists<'_> {
    (
        &mut unit.net_declarations,
        &mut unit.data_declarations,
        &mut unit.assign_statements,
    )
}

impl Parser<'_> {
    /// Fold the body port declarations of a non-ANSI `unit` into its header.
    pub(super) fn adopt_non_ansi_ports(&mut self, unit: &mut Unit) -> Result<()> {
        // Only a header with a list of ports leaves port declarations to
        // the body (IEEE 1800-2023 A.1.2: an ANSI header, or one without
        // ports, takes non-port items only).
        if unit.port_style == PortStyle::Ansi
            && let Some(tokens) = unit.items.iter().find_map(|&item| {
                let BodyItem { kind, tokens } = &self.body_items[item.0];
                matches!(
                    kind,
                    BodyItemKind::Unconverted { category, .. }
                        if matches!(
                            category.as_str(),
                            "input" | "output" | "inout" | "ref" | "const ref"
                        )
                )
                .then_some(*tokens)
            })
        {
            return Err(self.error_at(
                tokens,
                "port declaration in the body of a design element without a list of ports",
            ));
        }
        if unit.port_style != PortStyle::NonAnsi {
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
            let (direction, constant) = match category.as_str() {
                "input" => (Direction::Input, false),
                "output" => (Direction::Output, false),
                "inout" => (Direction::Inout, false),
                "ref" => (Direction::Ref, false),
                "const ref" => (Direction::Ref, true),
                _ => {
                    items.push(item);
                    continue;
                }
            };
            let tokens = *tokens;
            self.at = self.indices.partition_point(|&index| index < tokens.start);
            for (name, declaration) in self.port_declaration(direction, constant)? {
                if declarations.iter().any(|(declared, _)| *declared == name) {
                    self.at = resume;
                    return Err(self.error(format!("port `{name}` is declared more than once")));
                }
                declarations.push((name, declaration));
            }
        }
        // `.a(a)` is the port `a` (23.2.2.1).
        for port in &mut unit.ports {
            if let PortKind::Explicit {
                name,
                expression: Some(expression),
            } = &port.kind
                && matches!(&self.expressions[expression.0].kind,
                    ExprKind::Name(other) if other.value() == name.value())
            {
                port.kind = PortKind::Named(name.clone());
            }
        }
        // The names each header port references. A port that is a simple
        // identifier no other port references takes the port declaration
        // of its name; any other port is the explicit port of its
        // expression (23.2.2.1).
        let mut references = Vec::with_capacity(unit.ports.len());
        for port in &unit.ports {
            references.push(match &port.kind {
                PortKind::Named(name) => vec![name.value().to_string()],
                PortKind::NonAnsi(Some(expression))
                | PortKind::Explicit {
                    expression: Some(expression),
                    ..
                } => match self.port_references(*expression) {
                    Some(names) => names,
                    None => {
                        let tokens = self.expressions[expression.0].tokens;
                        self.at = resume;
                        return Err(self.error_at(
                            tokens,
                            "a port expression is a port reference or a concatenation of them",
                        ));
                    }
                },
                PortKind::NonAnsi(None) | PortKind::Explicit { .. } | PortKind::Wildcard => {
                    Vec::new()
                }
            });
        }
        let directions: Vec<(String, Direction)> = declarations
            .iter()
            .map(|(name, declaration)| (name.clone(), declaration.direction))
            .collect();
        let direction_of = |name: &str| {
            directions
                .iter()
                .find(|(declared, _)| declared == name)
                .map(|(_, direction)| *direction)
        };
        // A port that is a simple identifier takes the port declaration of
        // its name, by which it connects (23.2.2.1): the only port that
        // references the object, or the first one of an output that other
        // port expressions read too. Any other port is the explicit port of
        // its expression, also the ports of an input that several drive.
        let simple: Vec<bool> = unit
            .ports
            .iter()
            .enumerate()
            .map(|(position, port)| match &port.kind {
                PortKind::Named(name) => {
                    let name = name.value();
                    let shared = references
                        .iter()
                        .filter(|names| names.iter().any(|other| other == name))
                        .count()
                        > 1;
                    let first = !unit.ports[..position].iter().any(
                        |other| matches!(&other.kind, PortKind::Named(other) if other.value() == name),
                    );
                    !shared || first && direction_of(name) == Some(Direction::Output)
                }
                _ => false,
            })
            .collect();
        // A port expression of an inout or ref port, or of input and output
        // ports together, joins nets in both directions; the unit keeps
        // the non-ANSI form, which analysis rejects.
        for (position, names) in references.iter().enumerate() {
            if simple[position] {
                continue;
            }
            let mut found = names.iter().filter_map(|name| direction_of(name));
            let first = found.next();
            if first.is_some_and(|first| {
                !matches!(first, Direction::Input | Direction::Output)
                    || found.any(|direction| direction != first)
            }) {
                self.at = resume;
                return Ok(());
            }
        }
        // The names a generated port name may not take.
        let mut taken: Vec<String> = unit
            .ports
            .iter()
            .filter_map(|port| match &port.kind {
                PortKind::Named(name) | PortKind::Explicit { name, .. } => {
                    Some(name.value().to_string())
                }
                _ => None,
            })
            .chain(directions.iter().map(|(name, _)| name.clone()))
            .chain(self.declared_names(&items))
            .chain(
                unit.parameters
                    .iter()
                    .flatten()
                    .map(|parameter| parameter.name.value().to_string()),
            )
            .collect();
        let mut referenced: Vec<String> = Vec::new();
        let mut ports = std::mem::take(&mut unit.ports);
        for (position, port) in ports.iter_mut().enumerate() {
            if simple[position]
                && let PortKind::Named(name) = &port.kind
            {
                let name = name.value().to_string();
                self.at = resume;
                self.adopt_named_port(port, &name, &mut declarations, &mut items, unit_lists(unit))
                    .inspect_err(|_| self.at = resume)?;
                continue;
            }
            let (name, expression) = match port.kind.clone() {
                PortKind::Named(name) => {
                    let expression = self.name_expression(&name, port.tokens)?;
                    (
                        self.port_name(position, port.tokens, &mut taken),
                        Some(expression),
                    )
                }
                PortKind::NonAnsi(expression) => (
                    self.port_name(position, port.tokens, &mut taken),
                    expression,
                ),
                PortKind::Explicit { name, expression } => (name, expression),
                PortKind::Wildcard => unreachable!("a non-ANSI header has no wildcard port"),
            };
            // The port carries the bits of the objects it references in
            // their direction; a port of no object carries nothing.
            let mut direction = None;
            for reference in &references[position] {
                let Some(declared) = direction_of(reference) else {
                    self.at = resume;
                    return Err(self.error(format!("port `{reference}` has no port declaration")));
                };
                direction = Some(declared);
                if !referenced.contains(reference) {
                    referenced.push(reference.clone());
                }
            }
            port.kind = PortKind::Explicit { name, expression };
            port.direction = Some(direction.unwrap_or(Direction::Input));
            port.net_type = None;
            port.variable = false;
            port.data_type = None;
            port.dimensions = Vec::new();
            port.default = None;
        }
        unit.ports = ports;
        // The port declarations of the objects port expressions reference
        // declare those objects, completed by a net or variable declaration
        // of the same name as those of simple ports are. The object of a
        // simple port is the port.
        let mut objects = Vec::new();
        for (name, mut declaration) in std::mem::take(&mut declarations) {
            if !referenced.contains(&name) {
                self.at = resume;
                return Err(self.error(format!("port declaration `{name}` names no port")));
            }
            let complete = declaration.net_type.is_some()
                || declaration.variable
                || !matches!(
                    self.types[declaration.data_type.0].kind,
                    DataTypeKind::Implicit
                );
            if complete && self.declares_object(&items, &name) {
                self.at = resume;
                return Err(self.error(format!(
                    "port `{name}` with a net or variable type is declared again"
                )));
            }
            if !complete
                && let Some((net_type, variable, data_type, declared_type, initializer)) = self
                    .take_port_object(&mut items, unit_lists(unit), &name, declaration.data_type)
                    .inspect_err(|_| self.at = resume)?
            {
                declaration.default = initializer;
                if declared_type.is_some() {
                    self.at = resume;
                    return Err(self.unsupported(format!(
                        "ranges of port `{name}`, which a port expression references, written \
                         differently in its port and its net or variable declaration"
                    )));
                }
                declaration.net_type = net_type;
                if declaration.direction == Direction::Ref && declaration.net_type.is_some() {
                    self.at = resume;
                    return Err(self.error_at(
                        declaration.tokens,
                        "a `ref` port cannot have a net type (IEEE 1800-2023 23.2.2.3)",
                    ));
                }
                declaration.variable = variable;
                declaration.data_type = data_type;
            }
            let item = self
                .port_object(declaration, unit)
                .inspect_err(|_| self.at = resume)?;
            objects.push(item);
        }
        self.at = resume;
        items.splice(0..0, objects);
        unit.items = items;
        unit.port_style = PortStyle::Ansi;
        Ok(())
    }

    /// Give header port `port`, the identifier `name`, the direction and
    /// type of the port declaration of `name`, completed by the net or
    /// variable declaration of `name` among `items` if there is one.
    fn adopt_named_port(
        &mut self,
        port: &mut Port,
        name: &str,
        declarations: &mut Vec<(String, PortDeclaration)>,
        items: &mut Vec<BodyItemId>,
        lists: UnitLists<'_>,
    ) -> Result<()> {
        let Some(index) = declarations
            .iter()
            .position(|(declared, _)| *declared == name)
        else {
            return Err(self.error(format!("port `{name}` has no port declaration")));
        };
        let (_, mut declaration) = declarations.swap_remove(index);
        let (net_type, variable, data_type, declared_type) = if declaration.net_type.is_some()
            || declaration.variable
            || !matches!(
                self.types[declaration.data_type.0].kind,
                DataTypeKind::Implicit
            ) {
            // The port is completely declared (IEEE 1800-2023 23.2.2.1).
            if self.declares_object(items, name) {
                return Err(self.error(format!(
                    "port `{name}` with a net or variable type is declared again"
                )));
            }
            (
                declaration.net_type,
                declaration.variable,
                declaration.data_type,
                None,
            )
        } else {
            // The port is completed by a net or variable declaration of the
            // same name, if there is one.
            match self.take_port_object(items, lists, name, declaration.data_type)? {
                Some((net_type, variable, data_type, declared_type, initializer)) => {
                    // Only a variable output port is initialized (IEEE
                    // 1800-2023 23.2.2.2).
                    if initializer.is_some() {
                        if declaration.direction != Direction::Output {
                            return Err(self.error(format!(
                                "variable declaration with an initializer of non-ANSI port \
                                 `{name}` that is not an output"
                            )));
                        }
                        declaration.default = initializer;
                    }
                    if declaration.direction == Direction::Ref && net_type.is_some() {
                        return Err(self.error_at(
                            declaration.tokens,
                            "a `ref` port cannot have a net type (IEEE 1800-2023 23.2.2.3)",
                        ));
                    }
                    (net_type, variable, data_type, declared_type)
                }
                None => (None, false, declaration.data_type, None),
            }
        };
        port.constant = declaration.constant;
        port.direction = Some(declaration.direction);
        port.net_type = net_type;
        port.variable = variable;
        port.data_type = Some(data_type);
        port.declared_type = declared_type;
        port.dimensions = declaration.dimensions;
        port.default = declaration.default;
        port.tokens = declaration.tokens;
        Ok(())
    }

    /// The names port expression `id` references, if it is a port reference
    /// (an identifier with constant selects) or a concatenation of them
    /// (Syntax 23-2).
    fn port_references(&self, id: ExprId) -> Option<Vec<String>> {
        let mut names = Vec::new();
        let mut pending = vec![(id, true)];
        while let Some((id, concatenation)) = pending.pop() {
            match &self.expressions[id.0].kind {
                ExprKind::Concatenation(parts) if concatenation => {
                    pending.extend(parts.iter().rev().map(|&part| (part, false)));
                }
                ExprKind::Name(name) => {
                    if !names.iter().any(|other| other == name.value()) {
                        names.push(name.value().to_string());
                    }
                }
                ExprKind::Select { expression, .. } => {
                    let mut base = *expression;
                    while let ExprKind::Select { expression, .. } = &self.expressions[base.0].kind {
                        base = *expression;
                    }
                    let ExprKind::Name(name) = &self.expressions[base.0].kind else {
                        return None;
                    };
                    if !names.iter().any(|other| other == name.value()) {
                        names.push(name.value().to_string());
                    }
                }
                _ => return None,
            }
        }
        Some(names)
    }

    /// The expression of the identifier `name` that header port `tokens`
    /// spells.
    fn name_expression(&mut self, name: &Name, tokens: TokenRange) -> Result<ExprId> {
        let start = self.indices.partition_point(|&index| index < tokens.start);
        let start = (start..self.indices.len())
            .find(|&at| self.tokens[self.indices[at]].text == name.spelling)
            .unwrap_or(start);
        let resume = self.at;
        self.at = start + 1;
        let id = self.node(start, ExprKind::Name(name.clone()));
        self.at = resume;
        id
    }

    /// The name of header port `position` (zero-based) of an implicit port
    /// with an expression, which named connections may not use (23.2.2.1):
    /// `%` starts no identifier of the source, so neither a connection nor
    /// a declaration of the module or of a package it imports can name it.
    /// It also differs from the other names of the header, recorded in
    /// `taken`.
    fn port_name(&self, position: usize, tokens: TokenRange, taken: &mut Vec<String>) -> Name {
        let token = self
            .tokens
            .get(tokens.start)
            .or_else(|| self.tokens.last())
            .expect("a port has a token");
        let mut spelling = format!("%port{}", position + 1);
        while taken.contains(&spelling) {
            spelling.push('$');
        }
        taken.push(spelling.clone());
        Name {
            spelling,
            origin: token.origin,
        }
    }

    /// The net or variable declaration that port declaration `declaration`
    /// gives an object a port expression references (23.2.2.1): a variable
    /// for `var` or an output of a data type (23.2.2.3), else a net of the
    /// default net type.
    fn port_object(&mut self, declaration: PortDeclaration, unit: &mut Unit) -> Result<BodyItemId> {
        let variable = declaration.variable
            || declaration.direction == Direction::Ref
            || (declaration.direction == Direction::Output
                && declaration.net_type.is_none()
                && !matches!(
                    self.types[declaration.data_type.0].kind,
                    DataTypeKind::Implicit
                ));
        // Only a variable output port is initialized (IEEE 1800-2023
        // 23.2.2.2); the initializer initializes the variable.
        if declaration.default.is_some()
            && !(variable && declaration.direction == Direction::Output)
        {
            return Err(self.error_at(
                declaration.tokens,
                format!(
                    "initializer of non-ANSI port `{}` that is not a variable output",
                    declaration.name.value()
                ),
            ));
        }
        let declarator = VariableDeclarator {
            name: declaration.name,
            dimensions: declaration.dimensions,
            value: declaration.default,
            tokens: declaration.tokens,
        };
        let kind = if variable {
            let id = DataDeclarationId(self.data_declarations.len());
            self.data_declarations
                .push(DataDeclaration::Variables(VariableDeclaration {
                    constant: declaration.constant,
                    var: declaration.variable,
                    lifetime: None,
                    data_type: declaration.data_type,
                    variables: vec![declarator],
                    tokens: declaration.tokens,
                }));
            unit.data_declarations.push(id);
            BodyItemKind::Data(id)
        } else {
            let net_type = match (declaration.net_type, &self.default_nettype) {
                (Some(net_type), _) => net_type,
                (None, Some(net_type)) => net_type.clone(),
                (None, None) => {
                    return Err(self.error_at(
                        declaration.tokens,
                        format!(
                            "port `{}` without a net type under `default_nettype none",
                            declarator.name.value()
                        ),
                    ));
                }
            };
            let id = NetDeclarationId(self.net_declarations.len());
            self.net_declarations.push(NetDeclaration {
                net_type,
                strength: None,
                vector: None,
                data_type: declaration.data_type,
                delay: None,
                variables: vec![declarator],
                tokens: declaration.tokens,
            });
            unit.net_declarations.push(id);
            BodyItemKind::Net(id)
        };
        let item = BodyItemId(self.body_items.len());
        self.body_items.push(BodyItem {
            kind,
            tokens: declaration.tokens,
        });
        Ok(item)
    }

    /// The names a port declaration starting at the cursor declares.
    fn port_declaration(
        &mut self,
        direction: Direction,
        constant: bool,
    ) -> Result<Vec<(String, PortDeclaration)>> {
        let start = self.at;
        self.attributes()?;
        if constant {
            self.expect("const")?;
        }
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
        if direction == Direction::Ref && net_type.is_some() {
            return Err(self.error_at(
                self.range(start),
                "a `ref` port cannot have a net type (IEEE 1800-2023 23.2.2.3)",
            ));
        }
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
                    name,
                    constant,
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
    /// declarations, returning the port's net type, `var` and type, the
    /// port declaration's type when its range must be checked against the
    /// object's (see [`Self::merge_port_type`]), and the initializer of a
    /// variable declaration. The port declaration's packed range and signing
    /// carry over to a type that has neither (IEEE 1800-2023 23.2.2.1). A
    /// net declaration assignment stays as the continuous assignment it is
    /// (10.3.1).
    fn take_port_object(
        &mut self,
        items: &mut Vec<BodyItemId>,
        (unit_nets, unit_data, unit_assigns): UnitLists<'_>,
        name: &str,
        port_type: TypeId,
    ) -> Result<Option<PortObject>> {
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
        if !declarator.dimensions.is_empty() {
            return Err(self.error(format!(
                "net or variable declaration of non-ANSI port `{name}` with unpacked dimensions"
            )));
        }
        let (assign, initializer) = match declarator.value {
            Some(right) if variable => (None, Some(right)),
            Some(right) => (Some(self.declaration_assignment(&declarator, right)?), None),
            None => (None, None),
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
        let (data_type, declared_type) = self.merge_port_type(port_type, object_type)?;
        Ok(Some((
            net_type,
            variable,
            data_type,
            declared_type,
            initializer,
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
        let item = BodyItemId(self.body_items.len());
        self.body_items.push(BodyItem {
            kind: BodyItemKind::Assign(id),
            tokens: declarator.tokens,
        });
        Ok((id, item))
    }

    /// The names the parameter, net and variable declarations among `items`
    /// declare.
    fn declared_names(&self, items: &[BodyItemId]) -> Vec<String> {
        items
            .iter()
            .flat_map(|&item| {
                if let BodyItemKind::Parameter(id) = self.body_items[item.0].kind {
                    return self.parameter_declarations[id.0]
                        .parameters
                        .iter()
                        .map(|parameter| parameter.name.value().to_string())
                        .collect::<Vec<_>>();
                }
                let declarators: &[VariableDeclarator] = match self.body_items[item.0].kind {
                    BodyItemKind::Net(id) => &self.net_declarations[id.0].variables,
                    BodyItemKind::Data(id) => match &self.data_declarations[id.0] {
                        DataDeclaration::Variables(variables)
                        | DataDeclaration::Constructed(variables) => &variables.variables,
                        DataDeclaration::Unconverted(_) => &[],
                    },
                    _ => &[],
                };
                declarators
                    .iter()
                    .map(|declarator| declarator.name.value().to_string())
                    .collect()
            })
            .collect()
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
    /// `port_type` and a net or variable declaration of type `object_type`,
    /// and `port_type` when both give a range in other words: the ranges
    /// must be identical (IEEE 1800-2023 23.2.2.1), so their bounds are
    /// compared once parameters have values. It is also `port_type` when the
    /// object's type is a keyword or name without a range, whose width the
    /// port declaration's range must give.
    pub(super) fn merge_port_type(
        &mut self,
        port_type: TypeId,
        object_type: TypeId,
    ) -> Result<(TypeId, Option<TypeId>)> {
        let port = self.types[port_type.0].clone();
        let mut object = self.types[object_type.0].clone();
        let mut changed = false;
        let mut declared = None;
        if object.packed_dimensions.is_empty() {
            if matches!(object.kind, DataTypeKind::Implicit) {
                changed |= !port.packed_dimensions.is_empty();
                object.packed_dimensions = port.packed_dimensions.clone();
            } else if !port.packed_dimensions.is_empty() {
                // A type keyword or name without a range: the port
                // declaration's range must give the type's width.
                declared = Some(port_type);
            }
        } else {
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
            if port.packed_dimensions.is_empty() {
                return Err(self.error(
                    "the ranges of a non-ANSI port declaration and its net or variable \
                     declaration differ",
                ));
            }
            if text(&port.packed_dimensions) != text(&object.packed_dimensions) {
                declared = Some(port_type);
            }
        }
        if port.signed == Some(true) && object.signed != Some(true) {
            object.signed = Some(true);
            changed = true;
        }
        if !changed {
            return Ok((object_type, declared));
        }
        let id = TypeId(self.types.len());
        self.types.push(object);
        Ok((id, declared))
    }
}
