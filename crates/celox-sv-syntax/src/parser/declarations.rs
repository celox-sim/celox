use super::*;

struct UnitFrame {
    unit: Unit,
    start: usize,
    body_start: usize,
    end: &'static str,
    scopes: Vec<&'static str>,
    delimiters: Vec<&'static str>,
    conditional: bool,
    prototype: bool,
    item_start: bool,
    label_pending: bool,
    header_pending: bool,
    builder: super::generate::Builder,
    typed_category: Option<String>,
    checkers: Vec<CheckerFrame>,
    classes: Vec<ClassFrame>,
}

struct ClassFrame {
    start: usize,
    items: Vec<BodyItemId>,
}

struct CheckerFrame {
    start: usize,
    depth: usize,
    builder: super::generate::Builder,
    raw: Option<(usize, String)>,
}

impl UnitFrame {
    fn checker_ready(&self) -> bool {
        self.checkers.last().is_some_and(|checker| {
            !self.scopes[checker.depth..].iter().any(|scope| {
                matches!(
                    *scope,
                    "endclass"
                        | "endgroup"
                        | "endclocking"
                        | "endproperty"
                        | "endsequence"
                        | "endspecify"
                )
            })
        })
    }
    fn parses_items(&self) -> bool {
        !self.typed_scope() || self.checker_ready()
    }
    fn pending(&mut self, start: usize) {
        if self.checker_ready() {
            self.checkers
                .last_mut()
                .unwrap()
                .builder
                .pending
                .get_or_insert(start);
        } else {
            self.builder.pending.get_or_insert(start);
        }
    }
    fn boundary(&mut self, parser: &mut Parser<'_>) -> Result<bool> {
        if self.checker_ready() {
            self.checkers
                .last_mut()
                .unwrap()
                .builder
                .boundary(parser, &mut self.scopes)
        } else {
            self.builder.boundary(parser, &mut self.scopes)
        }
    }
    fn typed_scope(&self) -> bool {
        self.scopes.iter().any(|scope| {
            matches!(
                *scope,
                "endclass"
                    | "endchecker"
                    | "endclocking"
                    | "endgroup"
                    | "endproperty"
                    | "endsequence"
                    | "endspecify"
            )
        })
    }
    fn record(&mut self, parser: &mut Parser<'_>, start: usize, kind: BodyItemKind) -> Result<()> {
        if self.checker_ready() {
            let checker = self.checkers.last_mut().unwrap();
            checker.raw = None;
            checker.builder.complete(parser, start, kind)?;
        } else if let Some(class) = self.classes.last_mut() {
            if self.scopes.last() == Some(&"endclass") {
                parser.class_item(&kind);
            }
            class
                .items
                .push(super::generate::Builder::node(parser, start, kind)?);
        } else if !self.typed_scope() {
            self.builder.complete(parser, start, kind)?;
            self.typed_category = None;
            self.conditional = self.builder.active();
        }
        self.item_start = true;
        Ok(())
    }
}

impl Parser<'_> {
    /// Retain the unsupported checker's header as a token range while its
    /// captured body uses the same native control/procedure grammar as units.
    fn checker_header(&mut self) -> Result<usize> {
        let start = self.at;
        self.expect("checker")?;
        while self.directive()? {}
        self.name()?;
        while self.directive()? {}
        if self.eat("(") && !self.eat(")") {
            loop {
                while self.directive()? {}
                self.attributes()?;
                if matches!(self.peek(), "input" | "output") {
                    self.at += 1;
                }
                if matches!(self.peek(), "property" | "sequence" | "untyped") {
                    self.at += 1;
                } else if self.starts_type() {
                    self.data_type()?;
                }
                self.name()?;
                self.dimensions()?;
                while self.directive()? {}
                if self.eat("=") {
                    self.checker_default()?;
                }
                while self.directive()? {}
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
            }
        }
        while self.directive()? {}
        self.expect(";")?;
        Ok(start)
    }
    // SVA actuals remain an explicit opaque header class. Validate their
    // delimiter/termination boundary without narrowing them to data expressions.
    fn checker_default(&mut self) -> Result<()> {
        let start = self.at;
        let mut delimiters = Vec::new();
        loop {
            match self.peek() {
                "," | ")" if delimiters.is_empty() => {
                    if self.at == start {
                        return Err(self.error("expected a checker port default"));
                    }
                    return Ok(());
                }
                "" | ";" if delimiters.is_empty() => {
                    return Err(self.error("unterminated checker port default"));
                }
                "(" => delimiters.push(")"),
                "[" => delimiters.push("]"),
                "{" => delimiters.push("}"),
                word if delimiters.last().copied() == Some(word) => {
                    delimiters.pop();
                }
                ")" | "]" | "}" | "" => return Err(self.error("unbalanced checker port default")),
                _ => {}
            }
            self.at += 1;
        }
    }

    fn source_declaration(&mut self, start: usize, kind: BodyItemKind) -> Result<()> {
        let id = super::generate::Builder::node(self, start, kind)?;
        self.source_items.push(SourceItem {
            kind: SourceItemKind::Declaration(id),
            tokens: self.range(start),
        });
        Ok(())
    }
    pub(super) fn units(&mut self) -> Result<Vec<Unit>> {
        let mut units = Vec::new();
        let mut delimiters = Vec::new();
        let mut scopes = Vec::new();
        let mut blocked_subroutines = 0usize;
        let mut prototype = false;
        let mut class_boundary = false;
        let mut captured: Vec<(usize, String, Vec<BodyItemId>)> = Vec::new();
        // Start, first grammar token, declared opaque category. Metadata inside
        // unconverted scopes remains available without becoming global items.
        let mut pending: Option<(usize, usize, String)> = None;
        while !self.peek().is_empty() {
            if self.directive()? {
                continue;
            }
            if pending.is_none() && scopes.is_empty() && delimiters.is_empty() {
                let start = self.at;
                self.attributes()?;
                if self.peek().is_empty() {
                    return Err(self.error("attribute without a source declaration"));
                }
                pending = Some((start, self.at, self.peek().to_owned()));
            }
            if class_boundary
                && scopes.last() == Some(&"endclass")
                && delimiters.is_empty()
                && self.peek() == "(*"
            {
                self.attributes()?;
                continue;
            }
            if scopes.last() == Some(&"endgroup")
                && delimiters.is_empty()
                && self.peek() != "endgroup"
            {
                if self.coverage_item_prefix()? {
                    delimiters.push("}");
                }
                continue;
            }
            let root = scopes.is_empty() && delimiters.is_empty();
            let start = pending.as_ref().map_or(self.at, |value| value.0);
            if self.unit_start() {
                let id = SourceUnitId(units.len());
                units.push(self.unit()?);
                if root {
                    self.source_items.push(SourceItem {
                        kind: SourceItemKind::Unit(id),
                        tokens: self.range(start),
                    });
                    pending = None;
                }
                continue;
            }
            let item_start = self.at;
            let kind = if self.dpi_start() {
                Some(BodyItemKind::Dpi(self.dpi_declaration()?))
            } else if self.package_binding_start() {
                Some(BodyItemKind::PackageBinding(self.package_binding()?))
            } else if self.peek() == "typedef" {
                Some(BodyItemKind::TypeAlias(self.type_alias()?))
            } else if matches!(self.peek(), "timeunit" | "timeprecision") {
                Some(BodyItemKind::Time(self.time_declaration()?))
            } else if delimiters.is_empty() && matches!(self.peek(), "parameter" | "localparam") {
                Some(BodyItemKind::Parameter(self.parameter_declaration()?))
            } else if delimiters.is_empty() && self.peek() == "assign" {
                Some(BodyItemKind::Assign(self.assign_statement()?))
            } else if delimiters.is_empty()
                && blocked_subroutines == 0
                && matches!(self.peek(), "function" | "task")
            {
                let keyword = self.at;
                let id = self.subroutine(prototype)?;
                self.class_method(keyword, id);
                prototype = false;
                Some(BodyItemKind::Subroutine(id))
            } else if root
                && pending.as_ref().is_some_and(|value| value.1 == self.at)
                && self.net_start()
            {
                Some(BodyItemKind::Net(self.net_declaration()?))
            } else if root
                && pending.as_ref().is_some_and(|value| value.1 == self.at)
                && self.body_data_start()
            {
                Some(BodyItemKind::Data(self.body_data_declaration()?))
            } else {
                None
            };
            if let Some(kind) = kind {
                if scopes.last() == Some(&"endclass") && delimiters.is_empty() {
                    self.class_item(&kind);
                }
                if root {
                    self.source_declaration(start, kind)?;
                    pending = None;
                } else if let Some((_, _, items)) = captured.last_mut() {
                    items.push(super::generate::Builder::node(self, item_start, kind)?);
                }
                if scopes.last() == Some(&"endclass") && delimiters.is_empty() {
                    class_boundary = true;
                }
                continue;
            }
            if class_boundary && scopes.last() == Some(&"endclass") && delimiters.is_empty() {
                if self.class_constraint_start() {
                    self.class_constraint()?;
                    continue;
                }
                if self.class_property_start() {
                    self.class_property()?;
                    continue;
                }
                class_boundary = false;
            }
            if root
                && pending.as_ref().is_some_and(|value| value.1 == self.at)
                && self.token().is_some_and(|token| {
                    matches!(
                        token.kind,
                        TokenKind::Identifier | TokenKind::EscapedIdentifier
                    ) && !self.reserved_identifier(token)
                })
            {
                return Err(self.error("unrecognized source declaration"));
            }
            let word = self.peek();
            let mut complete = false;
            let mut closed = false;
            let mut completed_children = Vec::new();
            let opening = delimiters.is_empty()
                && matches!(
                    word,
                    "class" | "checker" | "clocking" | "covergroup" | "sequence"
                )
                || delimiters.is_empty()
                    && word == "property"
                    && (self.at == 0
                        || !matches!(
                            self.tokens[self.indices[self.at - 1]].text.as_str(),
                            "assert" | "assume" | "cover" | "restrict"
                        ));
            if opening {
                captured.push((self.at, word.to_owned(), Vec::new()));
                if word == "class" {
                    let items = self.class_header()?;
                    captured.last_mut().unwrap().2.extend(items);
                    scopes.push("endclass");
                    class_boundary = true;
                    continue;
                }
                if word == "checker" {
                    self.checker_header()?;
                    scopes.push("endchecker");
                    continue;
                }
                if word == "covergroup" {
                    self.coverage_header()?;
                    scopes.push("endgroup");
                    blocked_subroutines += 1;
                    continue;
                }
            }
            let closing = delimiters.is_empty() && scopes.last().copied() == Some(word);
            match word {
                "extern" | "pure" if delimiters.is_empty() => prototype = true,
                ";" if delimiters.is_empty() => {
                    prototype = false;
                    complete = scopes.is_empty();
                }
                "class" if delimiters.is_empty() => scopes.push("endclass"),
                "checker" if delimiters.is_empty() => scopes.push("endchecker"),
                "clocking" if delimiters.is_empty() => scopes.push("endclocking"),
                "covergroup" if delimiters.is_empty() => {
                    scopes.push("endgroup");
                    blocked_subroutines += 1;
                }
                "property"
                    if delimiters.is_empty()
                        && (self.at == 0
                            || !matches!(
                                self.tokens[self.indices[self.at - 1]].text.as_str(),
                                "assert" | "assume" | "cover" | "restrict"
                            )) =>
                {
                    scopes.push("endproperty");
                    blocked_subroutines += 1;
                }
                "sequence" if delimiters.is_empty() => {
                    scopes.push("endsequence");
                    blocked_subroutines += 1;
                }
                word if delimiters.is_empty() && scopes.last().copied() == Some(word) => {
                    let ended = scopes.pop().unwrap();
                    if matches!(ended, "endgroup" | "endproperty" | "endsequence") {
                        blocked_subroutines -= 1;
                    }
                    closed = scopes.is_empty();
                }
                "(" => delimiters.push(")"),
                "[" => delimiters.push("]"),
                "{" => delimiters.push("}"),
                word if delimiters.last().copied() == Some(word) => {
                    delimiters.pop();
                }
                ")" | "]" | "}" => {
                    return Err(self.error("mismatched source declaration delimiter"));
                }
                _ => {}
            }
            if delimiters.is_empty()
                && scopes.last() == Some(&"endclass")
                && (word == ";" || word == "}" || closing)
            {
                class_boundary = true;
            }
            self.at += 1;
            if closing && let Some((start, category, items)) = captured.pop() {
                if category == "class" {
                    self.finish_class();
                }
                if let Some((_, _, parent)) = captured.last_mut() {
                    parent.push(super::generate::Builder::node(
                        self,
                        start,
                        BodyItemKind::Unconverted { category, items },
                    )?);
                } else {
                    completed_children = items;
                }
            }
            if closed {
                if self.eat(":") {
                    self.name()?;
                }
                complete = true;
            }
            if complete {
                let (start, _, category) = pending
                    .take()
                    .ok_or_else(|| self.error("source declaration has no owner"))?;
                self.source_declaration(
                    start,
                    BodyItemKind::Unconverted {
                        category,
                        items: completed_children,
                    },
                )?;
            }
        }
        if pending.is_some() || !scopes.is_empty() || !delimiters.is_empty() {
            return Err(self.error("unterminated source declaration"));
        }
        Ok(units)
    }

    fn unit_start(&self) -> bool {
        let word = if self.peek() == "extern" {
            self.nth(1)
        } else {
            self.peek()
        };
        matches!(word, "module" | "macromodule" | "package" | "program")
            || (word == "interface"
                && self.nth(1) != "class"
                && (self.at == 0 || self.tokens[self.indices[self.at - 1]].text != "virtual"))
    }

    fn unit_header(&mut self) -> Result<UnitFrame> {
        let start = self.at;
        let external = self.eat("extern");
        let (kind, end) = match self.peek() {
            "module" | "macromodule" => (UnitKind::Module, "endmodule"),
            "interface" => (UnitKind::Interface, "endinterface"),
            "package" => (UnitKind::Package, "endpackage"),
            "program" => (UnitKind::Program, "endprogram"),
            _ => return Err(self.error("expected a design unit")),
        };
        self.at += 1;
        let lifetime = if matches!(self.peek(), "automatic" | "static") {
            let word = self.peek().to_owned();
            self.at += 1;
            Some(word)
        } else {
            None
        };
        let name = self.name()?;
        let mut imports = Vec::new();
        let mut package_bindings = Vec::new();
        while self.peek() == "import" {
            let id = self.package_binding()?;
            imports.push(self.package_bindings[id.0].tokens);
            package_bindings.push(id);
        }
        let parameters = if self.eat("#") {
            self.expect("(")?;
            Some(self.parameters(")")?)
        } else {
            None
        };
        let (ports, port_style) = if self.eat("(") {
            self.ports()?
        } else {
            (Vec::new(), PortStyle::Ansi)
        };
        self.expect(";")?;
        let header = self.range(start);
        Ok(UnitFrame {
            start,
            body_start: self.at,
            end,
            unit: Unit {
                kind,
                name,
                lifetime,
                external,
                parameters,
                ports,
                port_style,
                imports,
                header,
                body: TokenRange::default(),
                body_parameters: Vec::new(),
                parameter_declarations: Vec::new(),
                assign_statements: Vec::new(),
                data_declarations: Vec::new(),
                net_declarations: Vec::new(),
                instantiations: Vec::new(),
                items: Vec::new(),
                procedures: Vec::new(),
                subroutines: Vec::new(),
                children: Vec::new(),
                dpi_declarations: Vec::new(),
                package_bindings,
                type_aliases: Vec::new(),
                time_declarations: Vec::new(),
                end_label: None,
                tokens: header,
            },
            scopes: Vec::new(),
            delimiters: Vec::new(),
            conditional: false,
            prototype: false,
            item_start: true,
            label_pending: false,
            header_pending: false,
            builder: super::generate::Builder::default(),
            typed_category: None,
            checkers: Vec::new(),
            classes: Vec::new(),
        })
    }

    fn unit(&mut self) -> Result<Unit> {
        let mut frames = vec![self.unit_header()?];
        while let Some(mut frame) = frames.pop() {
            if frame.unit.external || self.peek() == frame.end {
                if !frame.checkers.is_empty() || !frame.builder.finished() {
                    return Err(self.error("unfinished generate body item"));
                }
                frame.unit.items = std::mem::take(&mut frame.builder.roots);
                frame.unit.body = self.range(frame.body_start);
                if !frame.unit.external {
                    self.expect(frame.end)?;
                    if self.eat(":") {
                        frame.unit.end_label = Some(self.name()?);
                    }
                }
                frame.unit.tokens = self.range(frame.start);
                if let Some(parent) = frames.last_mut() {
                    if self.nested_units.len() >= 1_000_000 {
                        return Err(self.unsupported("native unit arena exceeds one million nodes"));
                    }
                    let id = UnitId(self.nested_units.len());
                    self.nested_units.push(frame.unit);
                    let start = frame.start;
                    parent.unit.children.push(id);
                    parent.record(self, start, BodyItemKind::Unit(id))?;
                    continue;
                }
                return Ok(frame.unit);
            }
            if self.peek().is_empty() {
                return Err(self.error(format!("expected `{}`", frame.end)));
            }
            // IEEE 1800-2023 22.3 permits resetall only outside design elements.
            if self.peek() == "`" && self.nth(1) == "resetall" {
                return Err(self.error("resetall directive inside a design element"));
            }
            if self.directive()? {
                frames.push(frame);
                continue;
            }
            if frame.scopes.last() == Some(&"endgroup")
                && frame.delimiters.is_empty()
                && self.peek() != "endgroup"
            {
                if self.coverage_item_prefix()? {
                    frame.delimiters.push("}");
                }
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.parses_items() && frame.delimiters.is_empty() && self.peek() == "checker" {
                if frame.checkers.len() >= 1_000_000 {
                    return Err(self.unsupported(
                        "native checker continuation stack exceeds one million scopes",
                    ));
                }
                let start = self.checker_header()?;
                frame.scopes.push("endchecker");
                frame.checkers.push(CheckerFrame {
                    start,
                    depth: frame.scopes.len(),
                    builder: super::generate::Builder::default(),
                    raw: None,
                });
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if self.peek() == "endchecker"
                && frame.scopes.last() == Some(&"endchecker")
                && !frame.checkers.is_empty()
            {
                let checker = frame.checkers.pop().unwrap();
                if !checker.builder.finished() || checker.raw.is_some() {
                    return Err(self.error("unfinished checker body item"));
                }
                frame.scopes.pop();
                self.at += 1;
                if self.eat(":") {
                    self.name()?;
                }
                frame.record(
                    self,
                    checker.start,
                    BodyItemKind::Unconverted {
                        category: "checker".into(),
                        items: checker.builder.roots,
                    },
                )?;
                frames.push(frame);
                continue;
            }
            if frame.item_start && frame.delimiters.is_empty() && frame.parses_items() {
                if self.peek() == "(*" {
                    frame.pending(self.at);
                    self.attributes()?;
                    frames.push(frame);
                    continue;
                }
                if frame.boundary(self)? {
                    frame.item_start = true;
                    frame.conditional = frame.builder.active();
                    frames.push(frame);
                    continue;
                }
            }
            if frame.item_start
                && frame.delimiters.is_empty()
                && frame.parses_items()
                && self.peek() == "modport"
            {
                let start = self.at;
                let id = self.modport_declaration()?;
                frame.record(self, start, BodyItemKind::Modport(id))?;
                frame.conditional = frame.builder.active();
                frame.prototype = false;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if self.dpi_start() {
                let start = self.at;
                let id = self.dpi_declaration()?;
                frame.unit.dpi_declarations.push(id);
                frame.record(self, start, BodyItemKind::Dpi(id))?;
                frame.conditional = frame.builder.active();
                frame.prototype = false;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.delimiters.is_empty() && matches!(self.peek(), "timeunit" | "timeprecision") {
                let start = self.at;
                let id = self.time_declaration()?;
                frame.unit.time_declarations.push(id);
                frame.record(self, start, BodyItemKind::Time(id))?;
                frame.conditional = frame.builder.active();
                frame.prototype = false;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.delimiters.is_empty() && self.peek() == "typedef" {
                let start = self.at;
                let id = self.type_alias()?;
                frame.unit.type_aliases.push(id);
                frame.record(self, start, BodyItemKind::TypeAlias(id))?;
                frame.conditional = frame.builder.active();
                frame.prototype = false;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.delimiters.is_empty() && self.package_binding_start() {
                let start = self.at;
                let id = self.package_binding()?;
                frame.unit.package_bindings.push(id);
                frame.record(self, start, BodyItemKind::PackageBinding(id))?;
                frame.conditional = frame.builder.active();
                frame.prototype = false;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if self.unit_start() {
                if frames.len() >= 1_000_000 {
                    return Err(self.unsupported(
                        "native unit continuation stack exceeds one million entries",
                    ));
                }
                frame.conditional = frame.builder.active();
                frames.push(frame);
                frames.push(self.unit_header()?);
                continue;
            }
            if frame.delimiters.is_empty() && matches!(self.peek(), "parameter" | "localparam") {
                let direct = frame.scopes.is_empty() && !frame.conditional;
                let start = self.at;
                let id = self.parameter_declaration()?;
                frame.unit.parameter_declarations.push(id);
                if direct {
                    let mut parameters = self.parameter_declarations[id.0].parameters.clone();
                    if frame.unit.parameters.is_some() {
                        for parameter in &mut parameters {
                            parameter.local = true;
                        }
                    }
                    frame.unit.body_parameters.extend(parameters);
                }
                frame.record(self, start, BodyItemKind::Parameter(id))?;
                frame.conditional = frame.builder.active();
                frame.prototype = false;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.delimiters.is_empty() && self.peek() == "assign" {
                let start = self.at;
                let id = self.assign_statement()?;
                frame.unit.assign_statements.push(id);
                frame.record(self, start, BodyItemKind::Assign(id))?;
                frame.conditional = frame.builder.active();
                frame.prototype = false;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.delimiters.is_empty()
                && !frame
                    .scopes
                    .iter()
                    .any(|scope| matches!(*scope, "endgroup" | "endproperty" | "endsequence"))
                && matches!(self.peek(), "function" | "task")
            {
                let parameters = self.parameter_declarations.len();
                let aliases = self.type_aliases.len();
                let packages = self.package_bindings.len();
                let times = self.time_declarations.len();
                let assigns = self.assign_statements.len();
                let start = self.at;
                let id = self.subroutine(frame.prototype)?;
                self.class_method(start, id);
                frame.unit.subroutines.push(id);
                frame.record(self, start, BodyItemKind::Subroutine(id))?;
                frame.unit.parameter_declarations.extend(
                    (parameters..self.parameter_declarations.len()).map(ParameterDeclarationId),
                );
                frame
                    .unit
                    .type_aliases
                    .extend((aliases..self.type_aliases.len()).map(TypeAliasId));
                frame
                    .unit
                    .package_bindings
                    .extend((packages..self.package_bindings.len()).map(PackageBindingId));
                frame
                    .unit
                    .time_declarations
                    .extend((times..self.time_declarations.len()).map(TimeDeclarationId));
                frame
                    .unit
                    .assign_statements
                    .extend((assigns..self.assign_statements.len()).map(AssignId));
                frame.conditional = frame.builder.active();
                frame.prototype = false;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.item_start
                && frame.delimiters.is_empty()
                && !frame
                    .scopes
                    .iter()
                    .any(|scope| matches!(*scope, "endproperty" | "endsequence"))
                && matches!(
                    self.peek(),
                    "initial" | "always" | "always_comb" | "always_latch" | "always_ff" | "final"
                )
            {
                let parameters = self.parameter_declarations.len();
                let aliases = self.type_aliases.len();
                let packages = self.package_bindings.len();
                let times = self.time_declarations.len();
                let assigns = self.assign_statements.len();
                let start = self.at;
                let id = self.procedure()?;
                frame.unit.procedures.push(id);
                frame.record(self, start, BodyItemKind::Procedure(id))?;
                frame.unit.parameter_declarations.extend(
                    (parameters..self.parameter_declarations.len()).map(ParameterDeclarationId),
                );
                frame
                    .unit
                    .type_aliases
                    .extend((aliases..self.type_aliases.len()).map(TypeAliasId));
                frame
                    .unit
                    .package_bindings
                    .extend((packages..self.package_bindings.len()).map(PackageBindingId));
                frame
                    .unit
                    .time_declarations
                    .extend((times..self.time_declarations.len()).map(TimeDeclarationId));
                frame
                    .unit
                    .assign_statements
                    .extend((assigns..self.assign_statements.len()).map(AssignId));
                frame.conditional = frame.builder.active();
                frame.prototype = false;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.item_start
                && frame.delimiters.is_empty()
                && frame.scopes.last() == Some(&"endclass")
                && self.class_constraint_start()
            {
                self.class_constraint()?;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.item_start
                && frame.delimiters.is_empty()
                && frame.scopes.last() == Some(&"endclass")
                && self.class_property_start()
            {
                self.class_property()?;
                frame.item_start = true;
                frames.push(frame);
                continue;
            }
            if frame.label_pending {
                frame.label_pending = false;
                if self.eat(":") {
                    self.name()?;
                    frame.item_start = true;
                    frames.push(frame);
                    continue;
                }
            }
            if frame.item_start && frame.delimiters.is_empty() {
                if self.peek() == "(*" {
                    self.attributes()?;
                    frames.push(frame);
                    continue;
                }
                if !frame.typed_scope() && self.instantiation_start() {
                    let start = self.at;
                    let id = self.instantiation()?;
                    frame.unit.instantiations.push(id);
                    frame.record(self, start, BodyItemKind::Instance(id))?;
                    frames.push(frame);
                    continue;
                }
                if !frame.typed_scope() && self.net_start() {
                    let start = self.at;
                    let id = self.net_declaration()?;
                    frame.unit.net_declarations.push(id);
                    frame.record(self, start, BodyItemKind::Net(id))?;
                    frames.push(frame);
                    continue;
                }
                if !frame.scopes.iter().any(|scope| {
                    matches!(
                        *scope,
                        "endclass"
                            | "endchecker"
                            | "endclocking"
                            | "endgroup"
                            | "endproperty"
                            | "endsequence"
                            | "endspecify"
                    )
                }) && self.body_data_start()
                {
                    let start = self.at;
                    let id = self.body_data_declaration()?;
                    frame.unit.data_declarations.push(id);
                    frame.record(self, start, BodyItemKind::Data(id))?;
                    frame.item_start = true;
                    frame.conditional = frame.builder.active();
                    frames.push(frame);
                    continue;
                }
            }
            if frame.item_start
                && frame.delimiters.is_empty()
                && !frame.typed_scope()
                && self.token().is_some_and(|token| {
                    matches!(
                        token.kind,
                        TokenKind::Identifier | TokenKind::EscapedIdentifier
                    ) && !self.reserved_identifier(token)
                })
            {
                return Err(self.error("unrecognized body declaration"));
            }
            if frame.checker_ready() && frame.checkers.last().unwrap().raw.is_none() {
                frame.checkers.last_mut().unwrap().raw = Some((self.at, self.peek().to_owned()));
            }
            let checker_was_ready = frame.checker_ready();
            if !frame.typed_scope() && frame.typed_category.is_none() {
                frame.typed_category = Some(self.peek().to_owned());
            }
            let was_typed = frame.typed_scope();
            let item_word = self.peek();
            let closing_class = item_word == "endclass" && frame.scopes.last() == Some(&"endclass");
            if frame.delimiters.is_empty() {
                frame.item_start = matches!(
                    item_word,
                    "begin" | "end" | "generate" | "endgenerate" | "else" | ":" | ";"
                );
                frame.label_pending = matches!(item_word, "begin" | "end");
                if matches!(item_word, "if" | "for" | "case" | "casex" | "casez") {
                    frame.header_pending = true;
                }
            } else if item_word == ")" && frame.delimiters.len() == 1 && frame.header_pending {
                frame.item_start = true;
                frame.header_pending = false;
            }
            let scopes = &mut frame.scopes;
            let delimiters = &mut frame.delimiters;
            let word = self.peek();
            let forward_class = self.at > 0
                && (self.tokens[self.indices[self.at - 1]].text == "typedef"
                    || (self.tokens[self.indices[self.at - 1]].text == "interface"
                        && self.at > 1
                        && self.tokens[self.indices[self.at - 2]].text == "typedef"));
            match word {
                "(" => delimiters.push(")"),
                "[" => delimiters.push("]"),
                "{" => delimiters.push("}"),
                ")" | "]" | "}" if delimiters.last().copied() == Some(word) => {
                    delimiters.pop();
                }
                "extern" | "pure" | "import" if delimiters.is_empty() => frame.prototype = true,
                "function" | "task"
                    if delimiters.is_empty()
                        && !frame.prototype
                        && !scopes.iter().any(|scope| {
                            matches!(*scope, "endgroup" | "endproperty" | "endsequence")
                        }) =>
                {
                    scopes.push(if word == "function" {
                        "endfunction"
                    } else {
                        "endtask"
                    })
                }
                "class" if delimiters.is_empty() && self.nth(1) != ";" && !forward_class => {
                    frame.classes.push(ClassFrame {
                        start: self.at,
                        items: Vec::new(),
                    });
                    scopes.push("endclass");
                    let items = self.class_header()?;
                    frame.classes.last_mut().unwrap().items.extend(items);
                    frame.item_start = true;
                    frames.push(frame);
                    continue;
                }
                "generate" => scopes.push("endgenerate"),
                "begin" => scopes.push("end"),
                "case" | "casex" | "casez" => scopes.push("endcase"),
                "fork" => scopes.push("join"),
                "clocking" if self.nth(2) != ";" => scopes.push("endclocking"),
                "checker" => scopes.push("endchecker"),
                "property"
                    if delimiters.is_empty()
                        && self.at > 0
                        && !matches!(
                            self.tokens[self.indices[self.at - 1]].text.as_str(),
                            "assert" | "assume" | "cover" | "restrict"
                        ) =>
                {
                    scopes.push("endproperty")
                }
                "sequence" => scopes.push("endsequence"),
                "covergroup" if delimiters.is_empty() => {
                    scopes.push("endgroup");
                    self.coverage_header()?;
                    frame.item_start = true;
                    frames.push(frame);
                    continue;
                }
                "specify" => scopes.push("endspecify"),
                "if" | "for" | "else" if scopes.is_empty() && delimiters.is_empty() => {
                    frame.conditional = true
                }
                ";" if delimiters.is_empty() => {
                    frame.conditional = frame.builder.active();
                    frame.prototype = false;
                    frame.item_start = true;
                }
                word if scopes.last().copied() == Some(word)
                    || (matches!(word, "join_any" | "join_none")
                        && scopes.last() == Some(&"join")) =>
                {
                    scopes.pop();
                    frame.conditional = frame.builder.active();
                    frame.item_start = true;
                    frame.label_pending = matches!(
                        word,
                        "end"
                            | "endclass"
                            | "endchecker"
                            | "endgroup"
                            | "endclocking"
                            | "endproperty"
                            | "endsequence"
                    );
                }
                _ => {}
            }
            if word == "}"
                && frame.delimiters.is_empty()
                && frame.scopes.last() == Some(&"endclass")
            {
                frame.item_start = true;
            }
            let semicolon = word == ";";
            self.at += 1;
            if closing_class {
                self.finish_class();
                let class = frame
                    .classes
                    .pop()
                    .ok_or_else(|| self.error("class scope has no captured owner"))?;
                if self.eat(":") {
                    self.name()?;
                }
                frame.record(
                    self,
                    class.start,
                    BodyItemKind::Unconverted {
                        category: "class".into(),
                        items: class.items,
                    },
                )?;
                if !frame.typed_scope() {
                    frame.typed_category = None;
                }
                frames.push(frame);
                continue;
            }
            if frame.checker_ready()
                && frame.delimiters.is_empty()
                && (semicolon || !checker_was_ready)
            {
                if !checker_was_ready && self.eat(":") {
                    self.name()?;
                    frame.label_pending = false;
                }
                if let Some((start, category)) = frame.checkers.last_mut().unwrap().raw.take() {
                    frame.record(
                        self,
                        start,
                        BodyItemKind::Unconverted {
                            category,
                            items: Vec::new(),
                        },
                    )?;
                }
                frames.push(frame);
                continue;
            }
            if (was_typed && !frame.typed_scope())
                || (semicolon && frame.delimiters.is_empty() && !frame.typed_scope())
            {
                if was_typed && self.eat(":") {
                    self.name()?;
                    frame.label_pending = false;
                }
                if let Some(category) = frame.typed_category.take() {
                    let start = frame.builder.pending.unwrap_or(self.at - 1);
                    frame.builder.complete(
                        self,
                        start,
                        BodyItemKind::Unconverted {
                            category,
                            items: Vec::new(),
                        },
                    )?;
                    frame.item_start = true;
                    frame.conditional = frame.builder.active();
                }
            }
            frames.push(frame);
        }
        unreachable!("unit parser begins with a header")
    }

    pub(super) fn parameter_declaration(&mut self) -> Result<ParameterDeclarationId> {
        let start = self.at;
        let parameters = self.parameters(";")?;
        let id = ParameterDeclarationId(self.parameter_declarations.len());
        self.parameter_declarations.push(ParameterDeclaration {
            parameters,
            tokens: self.range(start),
        });
        Ok(id)
    }

    pub(super) fn parameters(&mut self, end: &str) -> Result<Vec<Parameter>> {
        let mut parameters = Vec::new();
        let mut local = false;
        let mut type_parameter = false;
        let mut inherited = self.implicit_type()?;
        if self.eat(end) {
            return Ok(parameters);
        }
        loop {
            let start = self.at;
            self.attributes()?;
            let explicit = if self.eat("parameter") {
                local = false;
                true
            } else if self.eat("localparam") {
                local = true;
                true
            } else {
                false
            };
            if self.peek() == "type" && self.nth(1) != "(" {
                self.at += 1;
                type_parameter = true;
            } else if explicit || self.starts_type() {
                type_parameter = false;
                inherited = if self.starts_type() {
                    self.data_type()?
                } else {
                    self.implicit_type()?
                };
            }
            let name = self.name()?;
            let kind = if type_parameter {
                let default = if self.eat("=") {
                    Some(self.data_type()?)
                } else {
                    None
                };
                ParameterKind::Type { default }
            } else {
                let dimensions = self.dimensions()?;
                self.validate_unpacked_dimensions(&dimensions)?;
                let default = if self.eat("=") {
                    Some(self.constant_parameter_expression()?)
                } else {
                    None
                };
                ParameterKind::Value {
                    data_type: inherited,
                    dimensions,
                    default,
                }
            };
            let id = ParameterId(self.next_parameter_id);
            self.next_parameter_id += 1;
            parameters.push(Parameter {
                id,
                name,
                local,
                kind,
                tokens: self.range(start),
            });
            if self.eat(end) {
                break;
            }
            self.expect(",")?;
        }
        Ok(parameters)
    }

    fn ports(&mut self) -> Result<(Vec<Port>, PortStyle)> {
        let mut ports = Vec::new();
        let mut ansi = false;
        let mut inherited: Option<Port> = None;
        if self.eat(")") {
            return Ok((ports, PortStyle::Ansi));
        }
        loop {
            let start = self.at;
            self.attributes()?;
            let constant = self.eat("const");
            let direction = match self.peek() {
                "input" => Some(Direction::Input),
                "output" => Some(Direction::Output),
                "inout" => Some(Direction::Inout),
                "ref" => Some(Direction::Ref),
                _ => None,
            };
            if direction.is_some() {
                self.at += 1;
            }
            if constant && direction != Some(Direction::Ref) {
                return Err(self.error("expected `ref` after `const`"));
            }
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
            let typed = self.starts_type();
            let fresh = direction.is_some() || net_type.is_some() || variable || typed;
            let data_type = if typed {
                Some(self.data_type()?)
            } else if fresh {
                Some(self.implicit_type()?)
            } else {
                inherited.as_ref().and_then(|port| port.data_type)
            };
            ansi |= fresh;
            let kind = if self.eat(".*") {
                PortKind::Wildcard
            } else if self.eat(".") {
                let name = self.name()?;
                self.expect("(")?;
                let expression = if self.peek() == ")" {
                    None
                } else {
                    Some(self.expression(0)?)
                };
                self.expect(")")?;
                PortKind::Explicit { name, expression }
            } else if matches!(self.peek(), "," | ")") {
                PortKind::NonAnsi(None)
            } else if !fresh && matches!(self.peek(), "{" | "(") {
                PortKind::NonAnsi(Some(self.expression(0)?))
            } else {
                PortKind::Named(self.name()?)
            };
            let dimensions = self.dimensions()?;
            let default = if self.eat("=") {
                Some(self.expression(0)?)
            } else {
                None
            };
            let inherited_direction = if fresh {
                direction
            } else {
                inherited.as_ref().and_then(|port| port.direction)
            };
            let inherited_net = if fresh {
                net_type
            } else {
                inherited.as_ref().and_then(|port| port.net_type.clone())
            };
            let inherited_variable =
                variable || (!fresh && inherited.as_ref().is_some_and(|port| port.variable));
            let constant =
                constant || (!fresh && inherited.as_ref().is_some_and(|port| port.constant));
            let port = Port {
                kind,
                constant,
                direction: inherited_direction,
                net_type: inherited_net,
                variable: inherited_variable,
                data_type,
                dimensions,
                default,
                tokens: self.range(start),
            };
            inherited = Some(port.clone());
            ports.push(port);
            if self.eat(")") {
                break;
            }
            self.expect(",")?;
        }
        let style = if ports
            .iter()
            .any(|port| matches!(port.kind, PortKind::Wildcard))
        {
            PortStyle::Wildcard
        } else if ansi {
            PortStyle::Ansi
        } else {
            PortStyle::NonAnsi
        };
        Ok((ports, style))
    }

    // Preprocessing intentionally retains active compiler directives. Fixed
    // arity directives may be followed by source on the same physical line.
    pub(super) fn directive(&mut self) -> Result<bool> {
        if self.peek() != "`" {
            return Ok(false);
        }
        let start = self.at;
        self.at += 1;
        let name = self.peek().to_owned();
        self.at += 1;
        if self.keyword_directive(&name)? {
            self.directives.push(self.range(start));
            return Ok(true);
        }
        if name == "timescale" {
            let unit = self.time_value()?;
            self.expect("/")?;
            let precision = self.time_value()?;
            self.time_directives.push(TimeDirective {
                kind: TimeDirectiveKind::Scale { unit, precision },
                tokens: self.range(start),
            });
            self.directives.push(self.range(start));
            return Ok(true);
        }
        if name == "resetall" {
            self.time_directives.push(TimeDirective {
                kind: TimeDirectiveKind::Reset,
                tokens: self.range(start),
            });
        }
        match name.as_str() {
            "default_nettype"
                if !matches!(
                    self.peek(),
                    "wire"
                        | "tri"
                        | "tri0"
                        | "tri1"
                        | "wand"
                        | "triand"
                        | "wor"
                        | "trior"
                        | "trireg"
                        | "uwire"
                        | "none"
                ) =>
            {
                return Err(self.error("invalid default_nettype value"));
            }
            "unconnected_drive" if !matches!(self.peek(), "pull0" | "pull1") => {
                return Err(self.error("unconnected_drive requires pull0 or pull1"));
            }
            _ => {}
        }
        let arity = match name.as_str() {
            "default_nettype"
            | "unconnected_drive"
            | "begin_keywords"
            | "default_decay_time"
            | "default_trireg_strength" => Some(1),
            "line" => Some(3),
            "resetall"
            | "celldefine"
            | "endcelldefine"
            | "nounconnected_drive"
            | "end_keywords"
            | "delay_mode_distributed"
            | "delay_mode_path"
            | "delay_mode_unit"
            | "delay_mode_zero"
            | "accelerate"
            | "noaccelerate"
            | "expand_vectornets"
            | "noexpand_vectornets"
            | "autoexpand_vectornets"
            | "remove_gatenames"
            | "noremove_gatenames"
            | "remove_netnames"
            | "noremove_netnames"
            | "protect"
            | "endprotect"
            | "protected"
            | "endprotected" => Some(0),
            _ => None,
        };
        if let Some(arity) = arity {
            for _ in 0..arity {
                if self.peek().is_empty() {
                    return Err(self.error("incomplete compiler directive"));
                }
                self.at += 1;
            }
        } else {
            let previous = self
                .indices
                .get(self.at - 1)
                .copied()
                .unwrap_or(self.tokens.len());
            while self.at < self.indices.len()
                && !self.tokens[previous + 1..self.indices[self.at]]
                    .iter()
                    .any(|token| token.kind == TokenKind::Newline)
            {
                self.at += 1;
            }
        }
        self.directives.push(self.range(start));
        Ok(true)
    }
}
