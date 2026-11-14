//! Parameters, ports, signals and instances of the AST: construction and accessors.

use super::*;

impl Parameter {
    pub(super) fn new(
        name: String,
        value: Option<ConstExpr>,
        declared_width: Option<usize>,
        declared_signed: Option<bool>,
        declared_is_2state: bool,
        has_declared_type: bool,
        is_local: bool,
    ) -> Self {
        Self {
            name,
            value,
            declared_width,
            floating: None,
            semantic_type: None,
            packed_ranges: Vec::new(),
            members: Vec::new(),
            declared_signed,
            declared_is_2state,
            has_declared_type,
            is_local,
            signed_element_depth: None,
            enum_domain: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> Option<&ConstExpr> {
        self.value.as_ref()
    }

    pub(crate) fn resolved_value(
        &self,
        functions: &const_functions::ConstantFunctions,
        constants: &ConstEnv,
        parameter_types: &HashMap<String, ExprType>,
    ) -> Option<i128> {
        if self.floating.is_some() {
            return None;
        }
        let mut value =
            substitute_typed_parameter_literals(self.value()?.clone(), constants, parameter_types);
        if self.declared_is_2state {
            value = ConstExpr::Unary {
                op: UnaryOp::ToTwoState,
                expr: Box::new(value),
            };
        }
        let value: crate::ir::ConstExpr = value.into();
        let environment = const_functions::Environment {
            constants,
            functions,
        };
        let mut value = if self.has_declared_type && typecheck::const_expr_has_real_type(&value) {
            // An explicitly integral parameter supplies an integer assignment
            // context. Untyped real parameters retain their floating type.
            typecheck::round_real_to_i128(typecheck::eval_real_const_expr(&value, &environment)?)
        } else {
            typecheck::eval_const_expr_in_env(&value, &environment)?
        };
        if let Some(width) = self.declared_width {
            // A number holds a value wider than 128 bits by its low 128
            // bits, extended by their sign. An unsigned value with bit 127
            // set is extended by zeros instead (IEEE 1800-2023 10.7), which
            // only the literal of the declared width can hold; so is one
            // whose sign is not known here, such as a function result.
            if width > 128
                && value < 0
                && !infer_parameter_value_type(self.value()?, true, parameter_types)
                    .is_some_and(|r#type| r#type.signed)
            {
                return None;
            }
            value =
                coerce_const_parameter_value(value, width, self.declared_signed.unwrap_or(false));
        }
        Some(value)
    }

    /// Resolve once, retaining a nonnumeric literal for the caller to publish.
    /// Numeric results take precedence, including numbers recovered from a
    /// literal after two-state conversion or dependency substitution.
    pub(crate) fn resolved_value_and_literal(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        constants: &ConstEnv,
        parameter_types: &HashMap<String, ExprType>,
        literals: &HashMap<String, Expr>,
    ) -> (Option<i128>, Option<Expr>) {
        let functions = &tree.functions;
        if self.floating.is_some() {
            return (
                None,
                self.resolved_literal(tree, session, constants, parameter_types, literals),
            );
        }
        if let Some(value) = self.resolved_value(&tree.functions, constants, parameter_types) {
            return (Some(value), None);
        }
        let literal = self.resolved_literal(tree, session, constants, parameter_types, literals);
        let value = literal.as_ref().and_then(|literal| {
            eval_ast_const_expr(
                functions,
                &expr_to_const(session, literal.clone())?,
                constants,
            )
        });
        if value.is_some() {
            (value, None)
        } else {
            (None, literal)
        }
    }

    pub(crate) fn resolved_literal(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        constants: &ConstEnv,
        parameter_types: &HashMap<String, ExprType>,
        literals: &HashMap<String, Expr>,
    ) -> Option<Expr> {
        if let Some(kind) = self.floating {
            return parameters::real_literal(self.value()?, kind, constants, literals);
        }
        #[cfg(test)]
        parameters::LITERAL_RESOLUTIONS.with(|calls| calls.set(calls.get() + 1));
        if self
            .value()
            .is_some_and(|value| parameters::is_unbounded(value, constants))
        {
            return Some(Expr::Literal("$".into()));
        }
        // A previous elaboration pass may have left this declaration's numeric
        // value in the environment. Evaluate its initializer, not that value.
        let evaluation_constants = if constants.contains_key(self.name()) {
            #[cfg(test)]
            parameters::LITERAL_ENV_COPIES.with(|copies| copies.set(copies.get() + 1));
            let mut masked = constants.clone();
            masked.remove(self.name());
            Cow::Owned(masked)
        } else {
            Cow::Borrowed(constants)
        };
        let mut parameter = self.clone();
        parameter.value = self.value.clone().map(|value| {
            substitute_typed_parameter_literals(value, &evaluation_constants, parameter_types)
        });
        if let Some(ty) = self.resolved_type(parameter_types) {
            parameter.declared_width = Some(ty.width);
            parameter.declared_signed = Some(ty.signed);
        }
        let value = parameter_value_env(
            tree,
            session,
            std::slice::from_ref(&parameter),
            &evaluation_constants,
        )
        .remove(self.name())?;
        let value = substitute_expr_idents(value, literals);
        let value =
            fold_const_integral_expr_preserving_mask(tree, session, value, &evaluation_constants);
        matches!(value, Expr::Literal(_)).then_some(value)
    }

    pub(crate) fn resolved_type(
        &self,
        parameter_types: &HashMap<String, ExprType>,
    ) -> Option<ExprType> {
        let inferred = self.value().and_then(|value| {
            infer_parameter_value_type(value, self.has_declared_type, parameter_types)
        });
        let width = self
            .declared_width
            .or(inferred.map(|r#type| r#type.width))?;
        let signed = self
            .declared_signed
            .or(inferred.map(|r#type| r#type.signed))
            .unwrap_or(false);
        Some(ExprType { width, signed })
    }

    pub(crate) fn packed_ranges(&self) -> &[PackedRange] {
        &self.packed_ranges
    }

    pub(crate) fn declared_width(&self) -> Option<usize> {
        self.declared_width
    }

    pub(crate) fn declared_signed(&self) -> Option<bool> {
        self.declared_signed
    }
}

impl Signal {
    pub(super) fn new(name: String, r#type: Type) -> Self {
        Self {
            name,
            r#type,
            is_net: false,
            unresolved_net: false,
            net_resolution: NetResolution::Wire,
            net_drive: NetDrive::None,
            is_const: false,
            package_variable: None,
            hierarchical: None,
        }
    }

    pub(super) fn new_net(name: String, r#type: Type) -> Self {
        Self {
            name,
            r#type,
            is_net: true,
            unresolved_net: false,
            net_resolution: NetResolution::Wire,
            net_drive: NetDrive::None,
            is_const: false,
            package_variable: None,
            hierarchical: None,
        }
    }

    /// The hierarchical name of the signal of another instance the signal
    /// denotes, as written.
    pub fn hierarchical(&self) -> Option<&str> {
        self.hierarchical.as_deref()
    }

    /// The package variable (IEEE 1800-2023 26.2) the signal denotes, as
    /// `(package, variable)`: every module that uses it shares that object.
    pub fn package_variable(&self) -> Option<(&str, &str)> {
        self.package_variable
            .as_ref()
            .map(|(package, variable)| (package.as_str(), variable.as_str()))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// A variable of unpacked structure type, which only names its leaf
    /// variables.
    pub(crate) fn is_aggregate(&self) -> bool {
        self.r#type.unpacked_struct.is_some()
    }

    pub fn r#type(&self) -> &Type {
        &self.r#type
    }

    pub(crate) fn is_net(&self) -> bool {
        self.is_net
    }

    /// Whether this is an unresolved, single-driver `uwire` net (6.6.2).
    pub(crate) fn is_unresolved_net(&self) -> bool {
        self.unresolved_net
    }

    pub(crate) fn with_unresolved_net(mut self, unresolved: bool) -> Self {
        self.unresolved_net = unresolved;
        self
    }

    /// The implicit pull or supply driver of the net (6.6.5-6).
    pub(crate) fn net_drive(&self) -> NetDrive {
        self.net_drive
    }

    pub(crate) fn with_net_drive(mut self, drive: NetDrive) -> Self {
        self.net_drive = drive;
        self
    }

    /// How several drivers of the net combine (6.6).
    pub(crate) fn net_resolution(&self) -> NetResolution {
        self.net_resolution
    }

    pub(crate) fn with_net_resolution(mut self, resolution: NetResolution) -> Self {
        self.net_resolution = resolution;
        self
    }
}

impl Port {
    pub(super) fn new(name: String, direction: PortDirection, r#type: Type, is_net: bool) -> Self {
        Self {
            name,
            direction,
            r#type,
            randomizable: !is_net,
            is_net,
            unresolved_net: false,
            net_resolution: NetResolution::Wire,
            default: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn direction(&self) -> PortDirection {
        self.direction
    }

    pub fn r#type(&self) -> &Type {
        &self.r#type
    }

    pub fn is_net(&self) -> bool {
        self.is_net
    }

    /// Whether this is an unresolved, single-driver `uwire` net (6.6.2).
    pub(crate) fn is_unresolved_net(&self) -> bool {
        self.unresolved_net
    }

    pub(crate) fn with_unresolved_net(mut self, unresolved: bool) -> Self {
        self.unresolved_net = unresolved;
        self
    }

    /// How several drivers of the net combine (6.6).
    pub(crate) fn net_resolution(&self) -> NetResolution {
        self.net_resolution
    }

    pub(crate) fn with_net_resolution(mut self, resolution: NetResolution) -> Self {
        self.net_resolution = resolution;
        self
    }

    pub(crate) fn with_default(mut self, default: Option<ConstExpr>) -> Self {
        self.default = default;
        self
    }

    pub fn default(&self) -> Option<&ConstExpr> {
        self.default.as_ref()
    }
}

impl Instance {
    pub(super) fn new(
        module_name: String,
        name: String,
        parameter_names: Vec<String>,
        parameter_overrides: Vec<ParameterOverride>,
        condition: Option<ConstExpr>,
        port_names: Vec<String>,
        port_connections: Vec<PortConnection>,
        array_range: Option<(i128, i128)>,
    ) -> Self {
        Self {
            module_name,
            name,
            parameter_names,
            parameter_overrides,
            condition,
            port_names,
            port_connections,
            array_range,
        }
    }

    pub fn array_len(&self) -> Option<usize> {
        self.array_range
            .and_then(|(left, right)| usize::try_from(left.abs_diff(right)).ok()?.checked_add(1))
    }

    /// The declared `[left:right]` bounds of an instance array, if this is one.
    pub fn array_range(&self) -> Option<(i128, i128)> {
        self.array_range
    }

    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn parameter_names(&self) -> &[String] {
        &self.parameter_names
    }

    pub fn parameter_overrides(&self) -> &[ParameterOverride] {
        &self.parameter_overrides
    }

    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    pub fn port_names(&self) -> &[String] {
        &self.port_names
    }

    pub fn port_connections(&self) -> &[PortConnection] {
        &self.port_connections
    }
}

impl ParameterOverride {
    pub(super) fn new(name: String, value: Option<ConstExpr>) -> Self {
        Self {
            name,
            value,
            bound_type: None,
            bound_pattern: None,
        }
    }

    pub(super) fn type_override(name: String, bound_type: Type) -> Self {
        Self {
            name,
            value: None,
            bound_type: Some(bound_type),
            bound_pattern: None,
        }
    }

    /// A constant assignment pattern, evaluated in the caller scope.
    pub fn bound_pattern(&self) -> Option<&BoundPattern> {
        self.bound_pattern.as_ref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> Option<&ConstExpr> {
        self.value.as_ref()
    }

    pub fn bound_type(&self) -> Option<&Type> {
        self.bound_type.as_ref()
    }
}

impl PortConnection {
    pub(super) fn new(formal: String, actual: String, actual_expr: Option<Expr>) -> Self {
        Self {
            formal,
            actual,
            actual_expr,
            bound_pattern: None,
        }
    }

    /// An untyped assignment pattern with caller-owned values. The receiving
    /// port supplies its element types after parameter specialization.
    pub fn bound_pattern(&self) -> Option<&BoundPattern> {
        self.bound_pattern.as_ref()
    }

    pub(super) fn map_expressions(&mut self, mut map: impl FnMut(Expr) -> Expr) {
        self.actual_expr = self.actual_expr.take().map(&mut map);
        if let Some(pattern) = &mut self.bound_pattern {
            pattern.map_expressions(&mut map);
        }
    }

    pub fn formal(&self) -> &str {
        &self.formal
    }

    pub fn actual(&self) -> &str {
        &self.actual
    }

    pub fn actual_expr(&self) -> Option<&Expr> {
        self.actual_expr.as_ref()
    }
}
