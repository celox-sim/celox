//! Implicit nets of continuous assignment targets (IEEE 1800-2023 6.10).
//!
//! An identifier on the left-hand side of a continuous assignment that the
//! scope of the assignment does not declare names an implicit scalar net of
//! the default net type, declared in that scope. The net gets a net
//! declaration at the start of the scope, so semantic consumers see it like
//! an authored one. Under `default_nettype none` such an identifier is an
//! error (22.8).
use fxhash::FxHashSet as HashSet;

use super::*;

/// Marks a scope whose wildcard imports may supply any name.
const WILDCARD: &str = "*";

/// The implicit nets of one scope: the unit (`None`) or a generate block.
type ScopeNets = (Option<BodyItemId>, Vec<(Name, TokenRange)>);

/// What one scope declares and holds, borrowed from the parser.
#[derive(Default)]
struct ScopeItems<'p> {
    declared: HashSet<&'p str>,
    /// Items whose declared names are not known here: any identifier among
    /// their tokens counts as declared.
    opaque: Vec<TokenRange>,
    assigns: Vec<AssignId>,
    /// The generate blocks in the scope, each a scope of its own.
    blocks: Vec<BodyItemId>,
}

impl Parser<'_> {
    /// Declare the implicit nets of the continuous assignments of `unit`.
    pub(super) fn declare_implicit_nets(&mut self, unit: &mut Unit) -> Result<()> {
        // Implicit nets of interfaces stay unsupported.
        if unit.external
            || !matches!(unit.kind, UnitKind::Module | UnitKind::Program)
            || unit.assign_statements.is_empty()
        {
            return Ok(());
        }
        let implicit = self.implicit_nets(unit);
        let net_type = self.default_nettype.clone();
        if let (None, Some((_, nets))) = (&net_type, implicit.first()) {
            let (name, tokens) = &nets[0];
            self.at = self.indices.partition_point(|&index| index < tokens.start);
            return Err(self.error(format!(
                "continuous assignment to undeclared `{}` under `default_nettype none",
                name.value()
            )));
        }
        let net_type = net_type.as_deref().unwrap_or("wire");
        for (block, nets) in implicit {
            let mut items = Vec::with_capacity(nets.len());
            for (name, tokens) in nets {
                let item = self.implicit_net(name, tokens, net_type)?;
                if let BodyItemKind::Net(id) = self.body_items[item.0].kind {
                    unit.net_declarations.push(id);
                }
                items.push(item);
            }
            let scope = match block {
                None => &mut unit.items,
                Some(block) => match &mut self.body_items[block.0].kind {
                    BodyItemKind::Block { items, .. } => items,
                    _ => unreachable!("a scope is a block"),
                },
            };
            scope.splice(0..0, items);
        }
        Ok(())
    }

    /// The undeclared continuous assignment targets of `unit`, by the scope
    /// (`None` for the unit, or a generate block) that declares them.
    fn implicit_nets(&self, unit: &Unit) -> Vec<ScopeNets> {
        let mut header = ScopeItems::default();
        for &imports in &unit.imports {
            if self.tokens[imports.range()]
                .iter()
                .any(|token| token.text == "*")
            {
                header.declared.insert(WILDCARD);
            }
        }
        for parameter in unit.parameters.iter().flatten() {
            header.declared.insert(parameter.name.value());
        }
        for port in &unit.ports {
            match &port.kind {
                PortKind::Named(name) | PortKind::Explicit { name, .. } => {
                    header.declared.insert(name.value());
                }
                PortKind::NonAnsi(Some(expression)) => {
                    header.opaque.push(self.expressions[expression.0].tokens);
                }
                PortKind::NonAnsi(None) | PortKind::Wildcard => {}
            }
        }
        // The names each enclosing scope declares, outermost first, and
        // whether a wildcard import reaches it; a generate block at depth `d`
        // sees `chain[..d]`. Nested scopes are visited depth first from an
        // explicit stack.
        let header_wildcard = header.declared.contains(WILDCARD);
        let mut chain = vec![(header, header_wildcard)];
        let mut pending: Vec<(Option<BodyItemId>, usize)> = vec![(None, 1)];
        let mut implicit = Vec::new();
        while let Some((block, depth)) = pending.pop() {
            chain.truncate(depth);
            let items = match block {
                None => &unit.items,
                Some(block) => match &self.body_items[block.0].kind {
                    BodyItemKind::Block { items, .. } => items,
                    _ => continue,
                },
            };
            let mut scope = self.scope_items(items);
            let wildcard =
                scope.declared.contains(WILDCARD) || chain.last().is_some_and(|(_, wild)| *wild);
            let mut nets: Vec<(&Name, TokenRange)> = Vec::new();
            for &id in scope.assigns.iter().filter(|_| !wildcard) {
                for assignment in &self.assign_statements[id.0].assignments {
                    for (name, tokens) in self.target_names(assignment.left) {
                        let value = name.value();
                        if !self.declares(&scope, value)
                            && !chain.iter().any(|(outer, _)| self.declares(outer, value))
                            && !nets.iter().any(|(other, _)| other.value() == value)
                        {
                            nets.push((name, tokens));
                        }
                    }
                }
            }
            scope
                .declared
                .extend(nets.iter().map(|(name, _)| name.value()));
            if !nets.is_empty() {
                let nets = nets
                    .into_iter()
                    .map(|(name, tokens)| (name.clone(), tokens));
                implicit.push((block, nets.collect()));
            }
            pending.extend(scope.blocks.drain(..).map(|block| (Some(block), depth + 1)));
            chain.push((scope, wildcard));
        }
        implicit
    }

    /// The names the items of one scope declare, its continuous assignments
    /// and its generate blocks.
    fn scope_items(&self, items: &[BodyItemId]) -> ScopeItems<'_> {
        let mut scope = ScopeItems::default();
        // Items of the scope, and whether each is the body of a generate
        // construct without `begin`/`end`.
        let mut work: Vec<(BodyItemId, bool)> =
            items.iter().rev().map(|&item| (item, false)).collect();
        while let Some((item, generate_body)) = work.pop() {
            let declared = &mut scope.declared;
            let opaque = &mut scope.opaque;
            match &self.body_items[item.0].kind {
                BodyItemKind::Block {
                    leading_label,
                    label,
                    ..
                } => {
                    if let Some(label) = leading_label.as_ref().or(label.as_ref()) {
                        insert(declared, label);
                    }
                    scope.blocks.push(item);
                }
                BodyItemKind::Conditional {
                    then_body,
                    otherwise,
                    ..
                } => {
                    work.extend(otherwise.iter().map(|&body| (body, true)));
                    work.push((*then_body, true));
                }
                BodyItemKind::Case { arms, .. } => {
                    work.extend(arms.iter().rev().map(|arm| (arm.body, true)));
                }
                BodyItemKind::Loop { variable, body, .. } => {
                    insert(declared, variable);
                    work.push((*body, true));
                }
                BodyItemKind::Region(items) => {
                    work.extend(items.iter().rev().map(|&item| (item, generate_body)));
                }
                // A generate item without `begin`/`end` is a scope of its
                // own that this pass does not extend; its names count as
                // declared, so that it declares no nets elsewhere.
                _ if generate_body => opaque.push(self.body_items[item.0].tokens),
                BodyItemKind::Data(id) => match &self.data_declarations[id.0] {
                    DataDeclaration::Variables(variables)
                    | DataDeclaration::Constructed(variables) => {
                        variables
                            .variables
                            .iter()
                            .for_each(|variable| insert(declared, &variable.name));
                    }
                    DataDeclaration::Unconverted(tokens) => opaque.push(*tokens),
                },
                BodyItemKind::Net(id) => {
                    self.net_declarations[id.0]
                        .variables
                        .iter()
                        .for_each(|variable| insert(declared, &variable.name));
                }
                BodyItemKind::Instance(id) => {
                    self.instantiations[id.0]
                        .instances
                        .iter()
                        .for_each(|instance| insert(declared, &instance.name));
                }
                BodyItemKind::Dpi(id) => insert(declared, &self.dpi_declarations[id.0].name),
                BodyItemKind::Parameter(id) => {
                    self.parameter_declarations[id.0]
                        .parameters
                        .iter()
                        .for_each(|parameter| insert(declared, &parameter.name));
                }
                BodyItemKind::Assign(id) => scope.assigns.push(*id),
                BodyItemKind::Subroutine(id) => insert(declared, &self.subroutines[id.0].name),
                BodyItemKind::TypeAlias(id) => insert(declared, &self.type_aliases[id.0].name),
                BodyItemKind::PackageBinding(id) => {
                    for binding in &self.package_bindings[id.0].items {
                        match &binding.kind {
                            PackageBindingItemKind::Named { name, .. } => insert(declared, name),
                            // The names a wildcard import makes visible are
                            // not known here.
                            PackageBindingItemKind::Wildcard(_) | PackageBindingItemKind::All => {
                                add_wildcard(declared);
                            }
                        }
                    }
                }
                BodyItemKind::Modport(id) => {
                    self.modport_declarations[id.0]
                        .modports
                        .iter()
                        .for_each(|modport| insert(declared, &modport.name));
                }
                // Unconverted declarations (genvars, events, classes, ...)
                // declare names this pass does not know; any identifier in
                // them counts as declared.
                BodyItemKind::Unconverted { .. } => opaque.push(self.body_items[item.0].tokens),
                BodyItemKind::Procedure(_) | BodyItemKind::Time(_) | BodyItemKind::Unit(_) => {}
            }
        }
        scope
    }

    /// The identifiers of an assignment target that name a whole object:
    /// a name, the name a select applies to, or those of a concatenation.
    fn target_names(&self, target: ExprId) -> Vec<(&Name, TokenRange)> {
        let mut names = Vec::new();
        let mut work = vec![target];
        while let Some(expression) = work.pop() {
            let node = &self.expressions[expression.0];
            match &node.kind {
                ExprKind::Name(name) => names.push((name, node.tokens)),
                ExprKind::Select { expression, .. } => work.push(*expression),
                ExprKind::Concatenation(parts) => work.extend(parts.iter().rev()),
                ExprKind::Group(inner) => work.push(*inner),
                _ => {}
            }
        }
        names
    }

    /// Whether `scope` declares `name`, or may through an opaque item.
    fn declares(&self, scope: &ScopeItems<'_>, name: &str) -> bool {
        scope.declared.contains(name)
            || scope.opaque.iter().any(|tokens| {
                self.tokens[tokens.range()].iter().any(|token| {
                    matches!(
                        token.kind,
                        TokenKind::Identifier | TokenKind::EscapedIdentifier
                    ) && token.text.strip_prefix('\\').unwrap_or(&token.text) == name
                })
            })
    }

    /// A scalar net declaration of `name` of type `net_type`.
    fn implicit_net(
        &mut self,
        name: Name,
        tokens: TokenRange,
        net_type: &str,
    ) -> Result<BodyItemId> {
        if self.types.len() >= 1_000_000 {
            return Err(self.unsupported("native type arena exceeds one million nodes"));
        }
        let data_type = TypeId(self.types.len());
        self.types.push(DataType {
            kind: DataTypeKind::Implicit,
            signed: None,
            packed_dimensions: Vec::new(),
            tokens,
        });
        let id = NetDeclarationId(self.net_declarations.len());
        self.net_declarations.push(NetDeclaration {
            net_type: net_type.to_string(),
            strength: None,
            vector: None,
            data_type,
            delay: None,
            variables: vec![VariableDeclarator {
                name,
                dimensions: Vec::new(),
                value: None,
                tokens,
            }],
            tokens,
        });
        if self.body_items.len() >= 1_000_000 {
            return Err(self.unsupported("native body-item arena exceeds one million entries"));
        }
        let item = BodyItemId(self.body_items.len());
        self.body_items.push(BodyItem {
            kind: BodyItemKind::Net(id),
            tokens,
        });
        Ok(item)
    }
}

fn insert<'p>(declared: &mut HashSet<&'p str>, name: &'p Name) {
    declared.insert(name.value());
}

/// Mark a scope whose wildcard imports may supply any name.
fn add_wildcard(declared: &mut HashSet<&str>) {
    declared.insert(WILDCARD);
}
