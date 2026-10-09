//! State and helpers shared by the procedural lowerings.
//!
//! `always_comb` and continuous assignments are executed symbolically into
//! SLT (`comb`); `always_ff` bodies become imperative SIR (`ff`). Both lower
//! the statement IR of the analyzer, inline subroutine calls, and treat
//! procedural locals as hidden module variables.

use super::*;
use num_traits::{ToPrimitive, Zero};

/// Upper bound on the iterations unrolled for one process, matching the
/// generate-loop bound of the analyzer.
pub(super) const MAX_UNROLLED_ITERATIONS: usize = 10_000;
/// Upper bound on nested subroutine inlining.
pub(super) const MAX_CALL_DEPTH: usize = 64;

pub(super) fn unsupported(construct: impl Into<String>) -> sv::AnalyzerError {
    sv::AnalyzerError::Unsupported(construct.into())
}

/// The variables and subroutines of the module being lowered.
pub(super) struct ProcModule<'a> {
    pub variables: &'a mut HashMap<SourceVarId, SvVariable>,
    pub name_to_id: &'a mut HashMap<String, SourceVarId>,
    pub next_id: SourceVarId,
    pub constants: &'a HashMap<String, i128>,
    pub parameter_types: &'a HashMap<String, (usize, bool)>,
    pub subroutines: HashMap<String, sv::ir::Subroutine>,
    /// DPI-C imports of the module, by SystemVerilog name.
    pub dpi_imports: HashMap<String, sv::ir::DpiImport>,
    /// Extern functions the module calls, by local index.
    pub extern_functions: Vec<ExternFunction>,
    /// Number of extern calls lowered so far.
    pub extern_calls: usize,
    pub four_state: bool,
    /// Hidden variables created while lowering, for the caller to publish.
    pub created: Vec<SourceVarId>,
    temp_counter: usize,
    capture_counter: u64,
    /// Runtime event sites (`$display`, assertions, ...) of the module, by
    /// local site number.
    pub runtime_event_sites: Vec<RuntimeEventSite>,
    /// Runtime errors (fatal assertions) of the module, by local code.
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<SourceVarId>>,
    /// The declared bounds of each unpacked dimension, by variable.
    unpacked_bounds: HashMap<SourceVarId, Vec<(i128, i128)>>,
}

/// One word a memory file writes: its bits in the destination, value, and
/// unknown mask.
pub(super) struct MemoryWord {
    pub access: BitAccess,
    pub value: BigUint,
    pub mask: BigUint,
}

impl<'a> ProcModule<'a> {
    pub fn new(
        module: &sv::ir::Module,
        variables: &'a mut HashMap<SourceVarId, SvVariable>,
        name_to_id: &'a mut HashMap<String, SourceVarId>,
        constants: &'a HashMap<String, i128>,
        parameter_types: &'a HashMap<String, (usize, bool)>,
        four_state: bool,
    ) -> Self {
        let next_id = SourceVarId(
            variables
                .keys()
                .map(|id| id.0 + 1)
                .max()
                .unwrap_or_default(),
        );
        let subroutines = module
            .subroutines()
            .iter()
            .map(|subroutine| (subroutine.name.clone(), subroutine.clone()))
            .collect();
        let dpi_imports = module
            .dpi_imports()
            .iter()
            .map(|import| (import.name().to_string(), import.clone()))
            .collect();
        let declarations = module
            .ports()
            .iter()
            .map(|port| (port.name(), port.r#type()))
            .chain(
                module
                    .signals()
                    .iter()
                    .map(|signal| (signal.name(), signal.r#type())),
            )
            .chain(
                module
                    .locals()
                    .iter()
                    .map(|local| (local.name.as_str(), &local.r#type)),
            );
        let mut unpacked_bounds = HashMap::default();
        for (name, r#type) in declarations {
            let Some(&id) = name_to_id.get(name) else {
                continue;
            };
            let bounds: Option<Vec<_>> = r#type
                .unpacked_ranges()
                .iter()
                .map(|range| {
                    Some((
                        sv::typecheck::eval_const_expr_with_types(
                            range.left(),
                            constants,
                            parameter_types,
                        )?,
                        sv::typecheck::eval_const_expr_with_types(
                            range.right(),
                            constants,
                            parameter_types,
                        )?,
                    ))
                })
                .collect();
            if let Some(bounds) = bounds.filter(|bounds| !bounds.is_empty()) {
                unpacked_bounds.insert(id, bounds);
            }
        }
        Self {
            variables,
            name_to_id,
            next_id,
            constants,
            parameter_types,
            subroutines,
            dpi_imports,
            extern_functions: Vec::new(),
            extern_calls: 0,
            four_state,
            created: Vec::new(),
            temp_counter: 0,
            capture_counter: 0,
            runtime_event_sites: Vec::new(),
            runtime_errors: HashMap::default(),
            unpacked_bounds,
        }
    }

    /// The words `$readmemh`/`$readmemb` (IEEE 1800-2023 21.4) write, with
    /// the values of its optional start and finish addresses: the file is
    /// read when the design is compiled.
    pub fn readmem(
        &self,
        name: &str,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
        start: Option<i128>,
        finish: Option<i128>,
    ) -> Result<(SourceVarId, Vec<MemoryWord>), sv::AnalyzerError> {
        let radix = if name == "$readmemb" { 2 } else { 16 };
        let Some(sv::ir::SystemTaskArg::Str(filename)) = args.first() else {
            return Err(unsupported(format!(
                "`{name}` with a file name that is not a string literal"
            )));
        };
        // Veryl also emits a constant element select, which loads from that
        // element on.
        let (destination, select_lsb) = match args.get(1) {
            Some(sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Ident(destination))) => {
                (destination, None)
            }
            Some(sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Select { expr, lsb, .. }))
                if let sv::ir::Expr::Ident(destination) = &**expr =>
            {
                let lsb = sv::typecheck::eval_const_expr_with_types(
                    lsb,
                    self.constants,
                    self.parameter_types,
                )
                .and_then(|lsb| usize::try_from(lsb).ok())
                .ok_or_else(|| {
                    unsupported(format!("`{name}` destination with a run-time index"))
                })?;
                (destination, Some(lsb))
            }
            _ => {
                return Err(unsupported(format!(
                    "`{name}` destination that is not an array"
                )));
            }
        };
        let id = self
            .id(destination)
            .ok_or_else(|| unsupported(format!("`{name}` destination `{destination}`")))?;
        let variable = self.var(id);
        let (Some(bounds), Some(element_width)) = (
            self.unpacked_bounds.get(&id),
            unpacked_element_width(variable),
        ) else {
            return Err(unsupported(format!(
                "`{name}` destination that is not an unpacked array"
            )));
        };
        // A single dimension is addressed by its index values; several are
        // addressed in their storage order.
        let depth = variable.width / element_width;
        let (left, right) = match bounds.as_slice() {
            [bound] => *bound,
            _ => (0, depth as i128 - 1),
        };
        let (low, high) = (left.min(right), left.max(right));
        let selected = select_lsb.map(|lsb| {
            let position = (lsb / element_width) as i128;
            if left <= right {
                left + position
            } else {
                left - position
            }
        });
        let start = start.or(selected).unwrap_or(low);
        let finish = finish.unwrap_or(high);
        if start < low || start > high || finish < low || finish > high {
            return Err(sv::AnalyzerError::MemoryFile(format!(
                "address range [{start}:{finish}] is outside the destination range [{low}:{high}]"
            )));
        }
        let (Ok(first_index), Ok(end)) = (
            usize::try_from(start),
            usize::try_from(start.max(finish) + 1),
        ) else {
            return Err(unsupported(format!(
                "`{name}` destination with negative indices"
            )));
        };
        let content = std::fs::read_to_string(unescape(filename)).map_err(|error| {
            sv::AnalyzerError::MemoryFile(format!("failed to read {filename}: {error}"))
        })?;
        // Words load from the start address; address directives are offsets
        // from it.
        let writes = celox_frontend_core::memory_file::parse_memory_write_runs(
            &content,
            radix,
            element_width,
            first_index,
            end,
        )
        .map_err(|error| sv::AnalyzerError::MemoryFile(error.message))?;
        let word_mask = (BigUint::from(1u8) << element_width) - BigUint::from(1u8);
        let mut words = Vec::new();
        for run in writes.runs {
            let value = BigUint::from_bytes_le(&run.value_bytes);
            let mask = BigUint::from_bytes_le(&run.mask_bytes);
            for k in 0..run.bit_width / element_width {
                let index = (run.bit_offset / element_width + k) as i128;
                let position = usize::try_from(index.abs_diff(left)).unwrap_or(usize::MAX);
                let shift = k * element_width;
                let mut word_value = (&value >> shift) & &word_mask;
                let mut word_unknown = (&mask >> shift) & &word_mask;
                if !(self.four_state && variable.is_4state) {
                    word_value &= &word_mask ^ &word_unknown;
                    word_unknown = BigUint::zero();
                }
                words.push(MemoryWord {
                    access: BitAccess::new(
                        position * element_width,
                        (position + 1) * element_width - 1,
                    ),
                    value: word_value,
                    mask: word_unknown,
                });
            }
        }
        Ok((id, words))
    }

    /// Register a runtime event site and return its local number.
    pub fn event_site(&mut self, site: RuntimeEventSite) -> u32 {
        self.runtime_event_sites.push(site);
        (self.runtime_event_sites.len() - 1) as u32
    }

    /// Register a runtime error and return its local code.
    pub fn runtime_error(&mut self, message: String) -> i64 {
        let code = self.runtime_errors.len() as i64 + 1;
        self.runtime_errors.insert(
            code,
            RuntimeErrorInfo {
                message,
                signals: Vec::new(),
            },
        );
        code
    }

    /// A fresh key for an observer capture.
    pub fn capture_key(&mut self) -> u64 {
        self.capture_counter += 1;
        self.capture_counter
    }

    pub fn id(&self, name: &str) -> Option<SourceVarId> {
        self.name_to_id.get(name).copied()
    }

    pub fn var(&self, id: SourceVarId) -> &SvVariable {
        &self.variables[&id]
    }

    pub fn is_hidden(&self, id: SourceVarId) -> bool {
        self.variables.get(&id).is_some_and(|var| var.hidden)
    }

    /// A fresh hidden variable of the given shape. Its name cannot clash with
    /// a SystemVerilog identifier.
    pub fn temp(
        &mut self,
        purpose: &str,
        width: usize,
        signed: bool,
        is_4state: bool,
    ) -> (SourceVarId, String) {
        let name = format!("{purpose}@t{}", self.temp_counter);
        self.temp_counter += 1;
        let id = next_var_id(&mut self.next_id);
        self.variables.insert(
            id,
            SvVariable {
                path: vec![name.clone()],
                width: width.max(1),
                signed,
                is_4state,
                packed_ranges: vec![(i128::try_from(width.max(1)).unwrap_or(1) - 1, 0)],
                array_dims: Vec::new(),
                domain_kind: DomainKind::Other,
                kind: VariableKind::Variable,
                type_kind: if is_4state {
                    PortTypeKind::Logic
                } else {
                    PortTypeKind::Bit
                },
                source: None,
                hidden: true,
            },
        );
        self.name_to_id.insert(name.clone(), id);
        self.created.push(id);
        (id, name)
    }

    pub fn subroutine(&self, name: &str) -> Option<&sv::ir::Subroutine> {
        self.subroutines.get(name)
    }

    /// Whether an expression calls a user subroutine or a DPI-C import.
    pub fn lvalue_calls(&self, lvalue: &sv::ir::LValue) -> bool {
        lvalue_calls(lvalue, &|name| {
            self.subroutines.contains_key(name) || self.dpi_imports.contains_key(name)
        })
    }

    pub fn const_calls(&self, expr: &sv::ir::ConstExpr) -> bool {
        const_calls(expr, &|name| {
            self.subroutines.contains_key(name) || self.dpi_imports.contains_key(name)
        })
    }

    /// The largest subexpressions of `expr` that call a subroutine and also
    /// occur in `other`, left to right.
    pub fn shared_calls<'e>(
        &self,
        expr: &'e sv::ir::ConstExpr,
        other: &sv::ir::ConstExpr,
    ) -> Vec<&'e sv::ir::ConstExpr> {
        let mut shared = Vec::new();
        shared_calls(
            expr,
            other,
            &|name| self.subroutines.contains_key(name) || self.dpi_imports.contains_key(name),
            &mut shared,
        );
        shared
    }

    pub fn calls(&self, expr: &sv::ir::Expr) -> bool {
        expr_calls(expr, &|name| {
            self.subroutines.contains_key(name) || self.dpi_imports.contains_key(name)
        })
    }

    /// The local index of the extern function `import` links to. Imports
    /// of one C name with different prototypes get separate entries, which
    /// design assembly reports as a conflict.
    pub fn extern_function(&mut self, import: &sv::ir::DpiImport) -> u32 {
        let extern_type = |r#type: sv::ir::DpiType| match r#type {
            sv::ir::DpiType::Bit => ExternType::Bit,
            sv::ir::DpiType::Logic => ExternType::Logic,
            sv::ir::DpiType::Integer { width, signed } => ExternType::Integer { width, signed },
        };
        let function = ExternFunction {
            name: import.c_name().to_string(),
            signature: ExternSignature {
                pure: import.is_pure(),
                result: import.return_type().map(extern_type),
                arguments: import
                    .arguments()
                    .iter()
                    .map(|argument| extern_type(argument.r#type()))
                    .collect(),
            },
        };
        let index = self
            .extern_functions
            .iter()
            .position(|known| *known == function)
            .unwrap_or_else(|| {
                self.extern_functions.push(function);
                self.extern_functions.len() - 1
            });
        u32::try_from(index).expect("extern function count fits u32")
    }

    /// Whether an expression is signed, with each user function call typed
    /// by its declared return type.
    pub fn expr_signed(&self, expr: &sv::ir::Expr) -> bool {
        let probe = if self.calls(expr) {
            self.typed_calls(expr)
        } else {
            expr.clone()
        };
        sv_expr_is_signed_with_parameters(
            &probe,
            self.variables,
            self.name_to_id,
            self.parameter_types,
        )
    }

    /// The width at which `left` and `right` are compared: the larger of
    /// their self-determined widths (IEEE 1800-2023 11.6.1 and 11.8.2), with
    /// each user function call typed by its declared return type.
    pub fn comparison_width(&self, left: &sv::ir::Expr, right: &sv::ir::Expr) -> Option<usize> {
        let typed = |expr: &sv::ir::Expr| {
            if self.calls(expr) {
                self.typed_calls(expr)
            } else {
                expr.clone()
            }
        };
        sv_comparison_operand_width(
            &typed(left),
            &typed(right),
            self.variables,
            self.name_to_id,
            self.constants,
            self.parameter_types,
        )
    }

    /// `expr` with each user function call replaced by a literal of its
    /// return type.
    fn typed_calls(&self, expr: &sv::ir::Expr) -> sv::ir::Expr {
        use sv::ir::Expr;
        let go = |expr: &Expr| self.typed_calls(expr);
        match expr {
            Expr::Call { name, .. } if self.dpi_imports.contains_key(name) => {
                let (width, signed) = self.dpi_imports[name]
                    .return_type()
                    .map_or((1, false), |r#type| (r#type.width(), r#type.is_signed()));
                Expr::Literal(typed_literal(&BigUint::zero(), width, signed))
            }
            Expr::Call { name, .. } if self.subroutines.contains_key(name) => {
                let (width, signed) = self
                    .subroutines
                    .get(name)
                    .and_then(|subroutine| subroutine.return_type.as_ref())
                    .and_then(|r#type| self.type_shape(r#type).ok())
                    .map_or((1, false), |(width, signed, _)| (width, signed));
                Expr::Literal(typed_literal(&BigUint::zero(), width, signed))
            }
            Expr::Ident(_) | Expr::Literal(_) => expr.clone(),
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => Expr::Select {
                expr: Box::new(go(expr)),
                msb: msb.clone(),
                lsb: lsb.clone(),
                signed: *signed,
            },
            Expr::Concat(parts) => Expr::Concat(parts.iter().map(go).collect()),
            Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
                count: count.clone(),
                parts: parts.iter().map(go).collect(),
            },
            Expr::Resize {
                expr,
                width,
                signed,
            } => Expr::Resize {
                expr: Box::new(go(expr)),
                width: *width,
                signed: *signed,
            },
            Expr::Unary { op, expr } => Expr::Unary {
                op: *op,
                expr: Box::new(go(expr)),
            },
            Expr::Binary { left, op, right } => Expr::Binary {
                left: Box::new(go(left)),
                op: *op,
                right: Box::new(go(right)),
            },
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => Expr::Mux {
                condition: Box::new(go(condition)),
                then_expr: Box::new(go(then_expr)),
                else_expr: Box::new(go(else_expr)),
            },
            Expr::Call { name, args } => Expr::Call {
                name: name.clone(),
                args: args.iter().map(go).collect(),
            },
            Expr::Inside { expr, items } => Expr::Inside {
                expr: Box::new(go(expr)),
                items: items
                    .iter()
                    .map(|item| item.map(&mut |operand| go(operand)))
                    .collect(),
            },
        }
    }

    /// The width, signedness, and state count of a declared type.
    pub fn type_shape(
        &self,
        r#type: &sv::ir::Type,
    ) -> Result<(usize, bool, bool), sv::AnalyzerError> {
        let info = signal_type_from_sv(r#type, self.constants, self.parameter_types)?;
        Ok((info.width, info.signed, info.is_4state))
    }
}

/// How a system task statement reports.
/// The variable `$readmemh`/`$readmemb` writes, if it is a task of these.
pub(super) fn readmem_destination<'e>(
    name: &str,
    args: &'e [sv::ir::SystemTaskArg<sv::ir::Expr>],
) -> Option<&'e str> {
    if name != "$readmemh" && name != "$readmemb" {
        return None;
    }
    match args.get(1)? {
        sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Ident(destination)) => Some(destination),
        sv::ir::SystemTaskArg::Expr(sv::ir::Expr::Select { expr, .. }) => match &**expr {
            sv::ir::Expr::Ident(destination) => Some(destination),
            _ => None,
        },
        _ => None,
    }
}

pub(super) enum SystemTaskKind {
    /// `$display` and `$write`, with the radix of their `b`, `o` and `h`
    /// variants (`d` otherwise).
    Print(RuntimeEventKind, char),
    /// `$finish` and `$stop`.
    Finish,
    /// `$error`, `$warning`, `$info`: a message, then execution continues.
    Message,
    /// `$fatal`: a message, then the simulation ends with an error.
    Fatal,
}

pub(super) fn system_task_kind(name: &str) -> Option<SystemTaskKind> {
    Some(match name {
        "$display" | "$displayb" | "$displayh" | "$displayo" => {
            SystemTaskKind::Print(RuntimeEventKind::Display, print_radix(name))
        }
        "$write" | "$writeb" | "$writeh" | "$writeo" => {
            SystemTaskKind::Print(RuntimeEventKind::Write, print_radix(name))
        }
        "$finish" | "$stop" => SystemTaskKind::Finish,
        "$error" | "$warning" | "$info" => SystemTaskKind::Message,
        "$fatal" => SystemTaskKind::Fatal,
        _ => return None,
    })
}

fn print_radix(name: &str) -> char {
    match name.as_bytes().last() {
        Some(radix @ (b'b' | b'o' | b'h')) => char::from(*radix),
        _ => 'd',
    }
}

/// How many arguments the conversions of `template` consume, as the runtime
/// renders them.
fn template_arguments(template: &str) -> usize {
    let mut chars = template.chars().peekable();
    let mut count = 0;
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        while chars.next_if(char::is_ascii_digit).is_some() {}
        if let Some(spec) = chars.next()
            && "bBoOhHxXdDiIcCsS".contains(spec)
        {
            count += 1;
        }
    }
    count
}

/// The message template and value arguments of a system task: a leading
/// string literal is the template. `$fatal` first takes a finish number.
/// A `$display` or `$write` argument without a conversion of its own is
/// displayed in the task's radix, directly after the previous one (IEEE
/// 1800-2023 21.2.1.1).
pub(super) fn system_task_template(
    kind: &SystemTaskKind,
    args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
) -> (Option<String>, Vec<sv::ir::SystemTaskArg<sv::ir::Expr>>) {
    let args = match kind {
        SystemTaskKind::Fatal if matches!(args.first(), Some(sv::ir::SystemTaskArg::Expr(_))) => {
            &args[1..]
        }
        SystemTaskKind::Finish => &[][..],
        _ => args,
    };
    let (mut template, values) = match args.first() {
        Some(sv::ir::SystemTaskArg::Str(template)) => {
            (Some(unescape(template)), args[1..].to_vec())
        }
        _ => (None, args.to_vec()),
    };
    if let SystemTaskKind::Print(_, radix) = kind {
        let converted = template.as_deref().map_or(0, template_arguments);
        let rest = values.len().saturating_sub(converted);
        if rest > 0 {
            let mut text = template.unwrap_or_default();
            text.push_str(&format!("%{radix}").repeat(rest));
            template = Some(text);
        }
    }
    (template, values)
}

/// The value of a string literal's escape sequences (IEEE 1800-2023 5.9.1).
pub(super) fn unescape(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            result.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => result.push('\n'),
            Some('t') => result.push('\t'),
            Some('\\') => result.push('\\'),
            Some('"') => result.push('"'),
            Some(other) => {
                result.push('\\');
                result.push(other);
            }
            None => result.push('\\'),
        }
    }
    result
}

/// Register the procedural locals of `module` as hidden variables.
pub(super) fn register_locals(
    module: &sv::ir::Module,
    variables: &mut HashMap<SourceVarId, SvVariable>,
    name_to_id: &mut HashMap<String, SourceVarId>,
    next_id: &mut SourceVarId,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<(), sv::AnalyzerError> {
    for local in module.locals() {
        let type_info = signal_type_from_sv(&local.r#type, constants, parameter_types)?;
        let id = next_var_id(next_id);
        variables.insert(
            id,
            SvVariable {
                path: vec![local.source_name.clone()],
                width: type_info.width,
                signed: type_info.signed,
                is_4state: type_info.is_4state,
                packed_ranges: type_info.packed_ranges,
                array_dims: type_info.array_dims,
                domain_kind: DomainKind::Other,
                kind: VariableKind::Variable,
                type_kind: type_info.type_kind,
                source: None,
                hidden: true,
            },
        );
        name_to_id.insert(local.name.clone(), id);
    }
    Ok(())
}

/// The value of a constant SLT tree, when every bit is known.
pub(super) fn slt_const<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    consts: &mut ConstCache,
    node: NodeId,
) -> Option<(BigUint, usize)> {
    let (value, mask, width) = slt_const4(arena, consts, node)?;
    mask.is_zero().then_some((value, width))
}

/// A four-state constant: `(payload, mask, width)`. An unknown bit has its
/// mask bit set; its payload bit is 1 for X and 0 for Z.
pub(super) type Const4 = (BigUint, BigUint, usize);

fn mask_of(width: usize) -> BigUint {
    (BigUint::from(1u8) << width) - BigUint::from(1u8)
}

/// The [`slt_const4`] values of the nodes of one arena. Interned nodes never
/// change, so a value stays valid while the arena grows; a cache must not be
/// shared between arenas.
#[derive(Default)]
pub(super) struct ConstCache(HashMap<NodeId, Option<Const4>>);

/// The four-state value of a constant SLT tree (IEEE 1800-2023 11.4 for the
/// treatment of unknown bits).
///
/// Symbolic execution builds deep DAGs (one level per unrolled loop
/// iteration, for example), so the tree is evaluated with an explicit stack,
/// and each node is evaluated once per `consts`.
pub(super) fn slt_const4<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    consts: &mut ConstCache,
    node: NodeId,
) -> Option<Const4> {
    let values = &mut consts.0;
    let mut stack = vec![node];
    while let Some(&top) = stack.last() {
        if values.contains_key(&top) {
            stack.pop();
            continue;
        }
        // Evaluate `top` from the operand values known so far. The first
        // operand it needs that is not yet evaluated is pushed, and `top` is
        // evaluated again once that operand is known.
        let missing = std::cell::Cell::new(None);
        let child = |id: NodeId| match values.get(&id) {
            Some(value) => value.clone(),
            None => {
                if missing.get().is_none() {
                    missing.set(Some(id));
                }
                None
            }
        };
        let value = slt_const4_node(arena, top, &child);
        match missing.get() {
            Some(operand) => stack.push(operand),
            None => {
                values.insert(top, value);
                stack.pop();
            }
        }
    }
    values[&node].clone()
}

/// The four-state value of `node`, given the values of its operands.
fn slt_const4_node<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    node: NodeId,
    child: &dyn Fn(NodeId) -> Option<Const4>,
) -> Option<Const4> {
    let width = celox_slt::get_width(node, arena);
    let all = mask_of(width);
    let unknown_all = || (all.clone(), all.clone(), width);
    let bit1 = |value: Option<bool>| -> Const4 {
        match value {
            Some(value) => (BigUint::from(u8::from(value)), BigUint::zero(), 1),
            None => (BigUint::from(1u8), BigUint::from(1u8), 1),
        }
    };
    let signed_value = |value: &BigUint, width: usize| -> num_bigint::BigInt {
        if width > 0 && value.bit(width as u64 - 1) {
            num_bigint::BigInt::from(value.clone()) - (num_bigint::BigInt::from(1u8) << width)
        } else {
            num_bigint::BigInt::from(value.clone())
        }
    };
    // The truth of a value: Some(true) for a known one bit, Some(false) when
    // every bit is a known zero, None otherwise.
    let truth = |(value, mask, width): &Const4| -> Option<bool> {
        let known = mask_of(*width) ^ (mask & mask_of(*width));
        if !(value & &known).is_zero() {
            Some(true)
        } else if (mask & mask_of(*width)).is_zero() {
            Some(false)
        } else {
            None
        }
    };
    let result: Const4 = match arena.get(node) {
        SLTNode::Constant(value, mask, width, _) => {
            (value & mask_of(*width), mask & mask_of(*width), *width)
        }
        SLTNode::Unary(op, inner) => {
            let (value, mask, inner_width) = child(*inner)?;
            let inner_all = mask_of(inner_width);
            match op {
                UnaryOp::Ident => (value, mask, width),
                UnaryOp::ToTwoState => (
                    (&value & (&inner_all ^ &mask)) & &all,
                    BigUint::zero(),
                    width,
                ),
                UnaryOp::BitNot => {
                    // Unknown bits stay unknown (as X).
                    ((&all ^ &value) | &mask, mask, width)
                }
                UnaryOp::Minus => {
                    if !mask.is_zero() {
                        return Some(unknown_all());
                    }
                    (
                        (&all + BigUint::from(1u8) - (&value & &all)) & &all,
                        BigUint::zero(),
                        width,
                    )
                }
                UnaryOp::LogicNot => bit1(truth(&(value, mask, inner_width)).map(|value| !value)),
                UnaryOp::And => {
                    let known_zero = &inner_all ^ (&value | &mask);
                    if !known_zero.is_zero() {
                        bit1(Some(false))
                    } else if !mask.is_zero() {
                        bit1(None)
                    } else {
                        bit1(Some(true))
                    }
                }
                UnaryOp::Or => bit1(truth(&(value, mask, inner_width))),
                UnaryOp::Xor => {
                    if !mask.is_zero() {
                        bit1(None)
                    } else {
                        bit1(Some(value.count_ones() % 2 == 1))
                    }
                }
                UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                    if !mask.is_zero() {
                        return Some(unknown_all());
                    }
                    let result = match op {
                        UnaryOp::PopCount => BigUint::from(value.count_ones()),
                        UnaryOp::CountLeadingZeros => {
                            BigUint::from(inner_width.saturating_sub(value.bits() as usize))
                        }
                        _ => BigUint::from(
                            value
                                .trailing_zeros()
                                .map_or(inner_width, |zeros| (zeros as usize).min(inner_width)),
                        ),
                    };
                    (result & &all, BigUint::zero(), width)
                }
            }
        }
        SLTNode::Binary(left, op, right) => {
            let (lv, lm, lw) = child(*left)?;
            let (rv, rm, rw) = child(*right)?;
            let both_signed = node_is_signed(arena, *left) && node_is_signed(arena, *right);
            let compare_width = lw.max(rw);
            let extend = |value: &BigUint,
                          mask: &BigUint,
                          from: usize,
                          signed: bool|
             -> (BigUint, BigUint) {
                if signed && from > 0 && (value.bit(from as u64 - 1) || mask.bit(from as u64 - 1)) {
                    let fill = mask_of(compare_width) ^ mask_of(from);
                    let value = if value.bit(from as u64 - 1) {
                        value | &fill
                    } else {
                        value.clone()
                    };
                    let mask = if mask.bit(from as u64 - 1) {
                        mask | &fill
                    } else {
                        mask.clone()
                    };
                    (value, mask)
                } else {
                    (value.clone(), mask.clone())
                }
            };
            let any_unknown = !lm.is_zero() || !rm.is_zero();
            match op {
                BinaryOp::And => {
                    let zero = (mask_of(lw) ^ (&lv | &lm)) | (mask_of(rw) ^ (&rv | &rm));
                    let one = (&lv & (mask_of(lw) ^ &lm)) & (&rv & (mask_of(rw) ^ &rm));
                    let unknown = &all ^ ((&zero | &one) & &all);
                    ((&one | &unknown) & &all, unknown & &all, width)
                }
                BinaryOp::Or => {
                    let one = (&lv & (mask_of(lw) ^ &lm)) | (&rv & (mask_of(rw) ^ &rm));
                    let zero = (mask_of(lw) ^ (&lv | &lm)) & (mask_of(rw) ^ (&rv | &rm));
                    let unknown = &all ^ ((&zero | &one) & &all);
                    ((&one | &unknown) & &all, unknown & &all, width)
                }
                BinaryOp::Xor => {
                    let unknown = (&lm | &rm) & &all;
                    (((&lv ^ &rv) | &unknown) & &all, unknown, width)
                }
                BinaryOp::EqCase | BinaryOp::NeCase => {
                    let (lv, lm) = extend(&lv, &lm, lw, both_signed);
                    let (rv, rm) = extend(&rv, &rm, rw, both_signed);
                    let equal = lv == rv && lm == rm;
                    bit1(Some(equal == (*op == BinaryOp::EqCase)))
                }
                BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
                    let (lv, lm) = extend(&lv, &lm, lw, both_signed);
                    let (rv, rm) = extend(&rv, &rm, rw, both_signed);
                    let wildcard = matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard);
                    let care = if wildcard {
                        mask_of(compare_width) ^ &rm
                    } else {
                        mask_of(compare_width)
                    };
                    let unknown = if wildcard {
                        &lm & &care
                    } else {
                        (&lm | &rm) & &care
                    };
                    let known = &care ^ &unknown;
                    let differs = !(((&lv ^ &rv) & &known).is_zero());
                    let equal = if differs {
                        Some(false)
                    } else if !unknown.is_zero() {
                        None
                    } else {
                        Some(true)
                    };
                    let positive = matches!(op, BinaryOp::Eq | BinaryOp::EqWildcard);
                    bit1(equal.map(|equal| equal == positive))
                }
                BinaryOp::LogicAnd => {
                    let left = truth(&(lv, lm, lw));
                    let right = truth(&(rv, rm, rw));
                    bit1(match (left, right) {
                        (Some(false), _) | (_, Some(false)) => Some(false),
                        (Some(true), Some(true)) => Some(true),
                        _ => None,
                    })
                }
                BinaryOp::LogicOr => {
                    let left = truth(&(lv, lm, lw));
                    let right = truth(&(rv, rm, rw));
                    bit1(match (left, right) {
                        (Some(true), _) | (_, Some(true)) => Some(true),
                        (Some(false), Some(false)) => Some(false),
                        _ => None,
                    })
                }
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar if rm.is_zero() => {
                    // Unknown bits of the shifted operand move with it; only an
                    // unknown amount makes the whole result unknown.
                    let shift = rv.to_usize().unwrap_or(usize::MAX);
                    match op {
                        BinaryOp::Shl => {
                            if shift >= width {
                                (BigUint::zero(), BigUint::zero(), width)
                            } else {
                                ((&lv << shift) & &all, (&lm << shift) & &all, width)
                            }
                        }
                        BinaryOp::Shr => {
                            if shift >= lw {
                                (BigUint::zero(), BigUint::zero(), width)
                            } else {
                                ((&lv >> shift) & &all, (&lm >> shift) & &all, width)
                            }
                        }
                        _ => {
                            let shift = shift.min(lw);
                            let fill = mask_of(lw) ^ mask_of(lw - shift);
                            let sign_value = lw > 0 && lv.bit(lw as u64 - 1);
                            let sign_mask = lw > 0 && lm.bit(lw as u64 - 1);
                            let mut value = if shift >= lw {
                                BigUint::zero()
                            } else {
                                &lv >> shift
                            };
                            let mut mask = if shift >= lw {
                                BigUint::zero()
                            } else {
                                &lm >> shift
                            };
                            if sign_value {
                                value |= &fill;
                            }
                            if sign_mask {
                                mask |= &fill;
                            }
                            (value & &all, mask & &all, width)
                        }
                    }
                }
                _ if any_unknown => {
                    if matches!(
                        op,
                        BinaryOp::LtU
                            | BinaryOp::LtS
                            | BinaryOp::LeU
                            | BinaryOp::LeS
                            | BinaryOp::GtU
                            | BinaryOp::GtS
                            | BinaryOp::GeU
                            | BinaryOp::GeS
                    ) {
                        bit1(None)
                    } else {
                        unknown_all()
                    }
                }
                _ => {
                    let extend2 = |value: &BigUint, from: usize, signed: bool| -> BigUint {
                        extend(value, &BigUint::zero(), from, signed).0
                    };
                    let compare = |signed: bool| -> std::cmp::Ordering {
                        let l = extend2(&lv, lw, signed);
                        let r = extend2(&rv, rw, signed);
                        if signed {
                            signed_value(&l, compare_width).cmp(&signed_value(&r, compare_width))
                        } else {
                            l.cmp(&r)
                        }
                    };
                    let value = match op {
                        BinaryOp::Add => (&lv + &rv) & &all,
                        BinaryOp::Sub => {
                            ((&all + BigUint::from(1u8) + (&lv & &all)) - (&rv & &all)) & &all
                        }
                        BinaryOp::Mul => (&lv * &rv) & &all,
                        BinaryOp::DivU | BinaryOp::RemU => {
                            if rv.is_zero() {
                                return Some(unknown_all());
                            }
                            if *op == BinaryOp::DivU {
                                &lv / &rv
                            } else {
                                &lv % &rv
                            }
                        }
                        BinaryOp::DivS | BinaryOp::RemS => {
                            let l = signed_value(&extend2(&lv, lw, true), compare_width);
                            let r = signed_value(&extend2(&rv, rw, true), compare_width);
                            if r.is_zero() {
                                return Some(unknown_all());
                            }
                            let value = if *op == BinaryOp::DivS { l / r } else { l % r };
                            let modulus = num_bigint::BigInt::from(1u8) << width;
                            (((value % &modulus) + &modulus) % &modulus).to_biguint()?
                        }
                        BinaryOp::Shl => {
                            let shift = rv.to_usize().unwrap_or(usize::MAX);
                            if shift >= width {
                                BigUint::zero()
                            } else {
                                (&lv << shift) & &all
                            }
                        }
                        BinaryOp::Shr => {
                            let shift = rv.to_usize().unwrap_or(usize::MAX);
                            if shift >= lw {
                                BigUint::zero()
                            } else {
                                &lv >> shift
                            }
                        }
                        BinaryOp::Sar => {
                            let shift = rv.to_usize().unwrap_or(usize::MAX).min(lw);
                            let negative = lw > 0 && lv.bit(lw as u64 - 1);
                            let shifted = if shift >= lw {
                                BigUint::zero()
                            } else {
                                &lv >> shift
                            };
                            if negative {
                                (shifted | (mask_of(lw) ^ mask_of(lw - shift))) & &all
                            } else {
                                shifted
                            }
                        }
                        BinaryOp::LtU => BigUint::from(u8::from(compare(false).is_lt())),
                        BinaryOp::LtS => BigUint::from(u8::from(compare(true).is_lt())),
                        BinaryOp::LeU => BigUint::from(u8::from(compare(false).is_le())),
                        BinaryOp::LeS => BigUint::from(u8::from(compare(true).is_le())),
                        BinaryOp::GtU => BigUint::from(u8::from(compare(false).is_gt())),
                        BinaryOp::GtS => BigUint::from(u8::from(compare(true).is_gt())),
                        BinaryOp::GeU => BigUint::from(u8::from(compare(false).is_ge())),
                        BinaryOp::GeS => BigUint::from(u8::from(compare(true).is_ge())),
                        _ => return None,
                    };
                    (value & &all, BigUint::zero(), width)
                }
            }
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            let merge_arms = || -> Option<Const4> {
                let (tv, tm, _) = child(*then_expr)?;
                let (ev, em, _) = child(*else_expr)?;
                // Bits equal in both arms (an unknown bit too) keep their
                // value; the others become X.
                let differ = ((&tv ^ &ev) | (&tm ^ &em)) & &all;
                let keep = &all ^ &differ;
                Some((
                    ((&tv & &keep) | &differ) & &all,
                    ((&tm & &keep) | &differ) & &all,
                    width,
                ))
            };
            match child(*cond) {
                Some(condition) => match truth(&condition) {
                    Some(true) => child(*then_expr)?,
                    Some(false) => child(*else_expr)?,
                    None => merge_arms()?,
                },
                // A run-time condition selecting between equal constants.
                None => {
                    let (tv, tm, _) = child(*then_expr)?;
                    let (ev, em, _) = child(*else_expr)?;
                    if tv != ev || tm != em {
                        return None;
                    }
                    (tv, tm, width)
                }
            }
        }
        SLTNode::Concat(parts) => {
            let mut value = BigUint::zero();
            let mut mask = BigUint::zero();
            for (part, part_width) in parts {
                let (part_value, part_mask, _) = child(*part)?;
                value = (value << *part_width) | (part_value & mask_of(*part_width));
                mask = (mask << *part_width) | (part_mask & mask_of(*part_width));
            }
            (value, mask, width)
        }
        SLTNode::Slice { expr, access } => {
            let (value, mask, _) = child(*expr)?;
            let slice = mask_of(access.msb - access.lsb + 1);
            (
                (value >> access.lsb) & &slice,
                (mask >> access.lsb) & &slice,
                width,
            )
        }
        SLTNode::Capture { expr, .. } => child(*expr)?,
        _ => return None,
    };
    Some((result.0 & mask_of(width), result.1 & mask_of(width), width))
}

/// Whether the lowering treats the value of `node` as signed when it extends it.
///
/// A node is signed when every operand that determines its signedness is;
/// the operands are visited with an explicit stack because symbolic
/// execution builds deep DAGs.
pub(super) fn node_is_signed<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    node: NodeId,
) -> bool {
    let mut visited = HashSet::default();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if !visited.insert(node) {
            continue;
        }
        let signed = match arena.get(node) {
            SLTNode::Input { signed, .. } => *signed,
            SLTNode::Constant(_, _, _, signed) => *signed,
            SLTNode::Binary(left, op, right) => match op {
                BinaryOp::DivS | BinaryOp::RemS => true,
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
                    stack.push(*left);
                    true
                }
                BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor => {
                    stack.extend([*right, *left]);
                    true
                }
                _ => false,
            },
            SLTNode::Unary(
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot,
                inner,
            )
            | SLTNode::Capture { expr: inner, .. } => {
                stack.push(*inner);
                true
            }
            SLTNode::Mux {
                then_expr,
                else_expr,
                ..
            } => {
                stack.extend([*else_expr, *then_expr]);
                true
            }
            SLTNode::ForFold {
                loop_signed,
                result: celox_slt::SLTForFoldResult::State(_),
                ..
            } => *loop_signed,
            _ => false,
        };
        if !signed {
            return false;
        }
    }
    true
}

/// The boolean constant `node` holds, if it is one.
pub(super) fn slt_bool<A: std::hash::Hash + Eq + Clone>(
    arena: &SLTNodeArena<A>,
    consts: &mut ConstCache,
    node: NodeId,
) -> Option<bool> {
    slt_const(arena, consts, node).map(|(value, _)| !value.is_zero())
}

pub(super) fn slt_constant<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    value: BigUint,
    width: usize,
    signed: bool,
) -> Result<NodeId, sv::AnalyzerError> {
    arena
        .alloc(SLTNode::Constant(value, BigUint::default(), width, signed))
        .map_err(slt_error)
}

pub(super) fn slt_unknown<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    width: usize,
) -> Result<NodeId, sv::AnalyzerError> {
    let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    arena
        .alloc(SLTNode::Constant(BigUint::default(), mask, width, false))
        .map_err(slt_error)
}

pub(super) fn slt_error(error: impl std::fmt::Display) -> sv::AnalyzerError {
    unsupported(format!("procedural lowering: {error}"))
}

/// The truth of a procedural condition: a known nonzero bit (IEEE 1800-2023
/// 12.4). Unknown bits count as false.
pub(super) fn slt_truth<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    condition: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    if let SLTNode::Constant(value, mask, width, _) = arena.get(condition) {
        let width_mask = (BigUint::from(1u8) << *width) - BigUint::from(1u8);
        let known = &width_mask ^ (mask & &width_mask);
        let truth = !(value & known).is_zero();
        return slt_constant(arena, BigUint::from(u8::from(truth)), 1, false);
    }
    if celox_slt::get_width(condition, arena) == 1
        && let SLTNode::Unary(UnaryOp::ToTwoState, _) = arena.get(condition)
    {
        return Ok(condition);
    }
    let any = arena
        .alloc(SLTNode::Unary(UnaryOp::Or, condition))
        .map_err(slt_error)?;
    arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, any))
        .map_err(slt_error)
}

/// Whether the truth of a condition is ambiguous: no bit is one and some
/// bit is unknown (IEEE 1800-2023 11.4.11). A condition with a known one is
/// true even when other bits are unknown.
pub(super) fn slt_truth_unknown<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    consts: &mut ConstCache,
    condition: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    let any = arena
        .alloc(SLTNode::Unary(UnaryOp::Or, condition))
        .map_err(slt_error)?;
    let known = arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, any))
        .map_err(slt_error)?;
    let is_known = arena
        .alloc(SLTNode::Binary(any, BinaryOp::EqCase, known))
        .map_err(slt_error)?;
    slt_not(arena, consts, is_known)
}

/// Whether a value is not logically false: some bit is one or unknown. The
/// second operand of `&&` is skipped only when the first is false
/// (IEEE 1800-2023 11.4.7); an ambiguous first operand still evaluates it.
pub(super) fn slt_not_false<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    value: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    let width = celox_slt::get_width(value, arena);
    let zero = slt_constant(arena, BigUint::zero(), width, false)?;
    arena
        .alloc(SLTNode::Binary(value, BinaryOp::NeCase, zero))
        .map_err(slt_error)
}

pub(super) fn slt_not<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    consts: &mut ConstCache,
    condition: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    if let Some(value) = slt_bool(arena, consts, condition) {
        return slt_constant(arena, BigUint::from(u8::from(!value)), 1, false);
    }
    arena
        .alloc(SLTNode::Unary(UnaryOp::LogicNot, condition))
        .map_err(slt_error)
}

pub(super) fn slt_and<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    consts: &mut ConstCache,
    left: NodeId,
    right: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    match (
        slt_bool(arena, consts, left),
        slt_bool(arena, consts, right),
    ) {
        (Some(false), _) | (_, Some(false)) => slt_constant(arena, BigUint::zero(), 1, false),
        (Some(true), _) => Ok(right),
        (_, Some(true)) => Ok(left),
        _ => arena
            .alloc(SLTNode::Binary(left, BinaryOp::LogicAnd, right))
            .map_err(slt_error),
    }
}

pub(super) fn slt_or<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    consts: &mut ConstCache,
    left: NodeId,
    right: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    match (
        slt_bool(arena, consts, left),
        slt_bool(arena, consts, right),
    ) {
        (Some(true), _) | (_, Some(true)) => slt_constant(arena, BigUint::from(1u8), 1, false),
        (Some(false), _) => Ok(right),
        (_, Some(false)) => Ok(left),
        _ => arena
            .alloc(SLTNode::Binary(left, BinaryOp::LogicOr, right))
            .map_err(slt_error),
    }
}

/// A sized literal with the given width, signedness, and bits.
pub(super) fn typed_literal(value: &BigUint, width: usize, signed: bool) -> String {
    format!(
        "{width}'{}h{}",
        if signed { "s" } else { "" },
        value.to_str_radix(16)
    )
}

/// The `for` loop shapes that lower to a counted SLT fold:
/// `for (v = start; v < end; v += step)` and its relatives.
#[derive(Debug, Clone)]
pub(super) struct CanonicalLoop {
    pub var: String,
    pub start: sv::ir::Expr,
    pub end: sv::ir::Expr,
    pub compare: sv::ir::BinaryOp,
    pub step: usize,
    pub step_op: celox_slt::SLTStepOp,
    pub decreasing: bool,
}

fn literal_usize(expr: &sv::ir::Expr) -> Option<usize> {
    let sv::ir::Expr::Literal(text) = expr else {
        return None;
    };
    let literal = sv::typecheck::parse_integral_literal(text)?;
    if !literal.mask.is_zero() {
        return None;
    }
    literal.value.to_usize()
}

pub(super) fn canonical_for_loop(
    init: &[sv::ir::Stmt],
    condition: Option<&sv::ir::Expr>,
    step: &[sv::ir::Stmt],
) -> Option<CanonicalLoop> {
    let (var, start) = match init {
        [
            sv::ir::Stmt::Local {
                name,
                init: Some(start),
            },
        ] => (name.clone(), start.clone()),
        [
            sv::ir::Stmt::Assign {
                lhs: sv::ir::LValue::Ident(name),
                rhs,
                nonblocking: false,
            },
        ] => (name.clone(), rhs.clone()),
        _ => return None,
    };
    let [
        sv::ir::Stmt::Assign {
            lhs: sv::ir::LValue::Ident(step_target),
            rhs:
                sv::ir::Expr::Binary {
                    left: step_left,
                    op: step_binary,
                    right: step_right,
                },
            nonblocking: false,
        },
    ] = step
    else {
        return None;
    };
    if *step_target != var || !matches!(&**step_left, sv::ir::Expr::Ident(name) if *name == var) {
        return None;
    }
    let step = literal_usize(step_right)?;
    let (step_op, decreasing) = match step_binary {
        sv::ir::BinaryOp::Add => (celox_slt::SLTStepOp::Add, false),
        sv::ir::BinaryOp::Sub => (celox_slt::SLTStepOp::Add, true),
        sv::ir::BinaryOp::Mul => (celox_slt::SLTStepOp::Mul, false),
        sv::ir::BinaryOp::Shl => (celox_slt::SLTStepOp::Shl, false),
        sv::ir::BinaryOp::BitOr => (celox_slt::SLTStepOp::BitOr, false),
        sv::ir::BinaryOp::BitXor => (celox_slt::SLTStepOp::BitXor, false),
        _ => return None,
    };
    if step == 0 && step_op == celox_slt::SLTStepOp::Add {
        return None;
    }
    let sv::ir::Expr::Binary { left, op, right } = condition? else {
        return None;
    };
    let is_var = |expr: &sv::ir::Expr| matches!(expr, sv::ir::Expr::Ident(name) if *name == var);
    let (compare, end) = if is_var(left) {
        (*op, (**right).clone())
    } else if is_var(right) {
        let flipped = match op {
            sv::ir::BinaryOp::Lt => sv::ir::BinaryOp::Gt,
            sv::ir::BinaryOp::Le => sv::ir::BinaryOp::Ge,
            sv::ir::BinaryOp::Gt => sv::ir::BinaryOp::Lt,
            sv::ir::BinaryOp::Ge => sv::ir::BinaryOp::Le,
            _ => return None,
        };
        (flipped, (**left).clone())
    } else {
        return None;
    };
    let valid = match compare {
        sv::ir::BinaryOp::Lt | sv::ir::BinaryOp::Le => !decreasing,
        sv::ir::BinaryOp::Gt | sv::ir::BinaryOp::Ge => {
            decreasing && step_op == celox_slt::SLTStepOp::Add
        }
        _ => false,
    };
    valid.then_some(CanonicalLoop {
        var,
        start,
        end,
        compare,
        step,
        step_op,
        decreasing,
    })
}

/// The identifiers an expression reads.
pub(super) fn expr_idents(expr: &sv::ir::Expr, names: &mut HashSet<String>) {
    fn const_idents(expr: &sv::ir::ConstExpr, names: &mut HashSet<String>) {
        match expr {
            sv::ir::ConstExpr::Ident(name) => {
                names.insert(name.clone());
            }
            sv::ir::ConstExpr::Literal(_) => {}
            sv::ir::ConstExpr::Select { expr, bit } => {
                const_idents(expr, names);
                const_idents(bit, names);
            }
            sv::ir::ConstExpr::Function { args, .. } => {
                args.iter().for_each(|arg| const_idents(arg, names))
            }
            sv::ir::ConstExpr::Unary { expr, .. } => const_idents(expr, names),
            sv::ir::ConstExpr::Binary { left, right, .. } => {
                const_idents(left, names);
                const_idents(right, names);
            }
            sv::ir::ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                const_idents(condition, names);
                const_idents(then_expr, names);
                const_idents(else_expr, names);
            }
        }
    }
    match expr {
        sv::ir::Expr::Ident(name) => {
            names.insert(name.clone());
        }
        sv::ir::Expr::Literal(_) => {}
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            expr_idents(expr, names);
            const_idents(msb, names);
            const_idents(lsb, names);
        }
        sv::ir::Expr::Concat(parts) => parts.iter().for_each(|part| expr_idents(part, names)),
        sv::ir::Expr::RepeatConcat { count, parts } => {
            const_idents(count, names);
            parts.iter().for_each(|part| expr_idents(part, names));
        }
        sv::ir::Expr::Resize { expr, .. } | sv::ir::Expr::Unary { expr, .. } => {
            expr_idents(expr, names)
        }
        sv::ir::Expr::Binary { left, right, .. } => {
            expr_idents(left, names);
            expr_idents(right, names);
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_idents(condition, names);
            expr_idents(then_expr, names);
            expr_idents(else_expr, names);
        }
        sv::ir::Expr::Call { args, .. } => args.iter().for_each(|arg| expr_idents(arg, names)),
        sv::ir::Expr::Inside { expr, items } => {
            expr_idents(expr, names);
            for item in items {
                item.exprs()
                    .into_iter()
                    .for_each(|operand| expr_idents(operand, names));
            }
        }
    }
}

/// Whether a constant-expression operand, such as a run-time select index,
/// calls a subroutine.
pub(super) fn const_calls(expr: &sv::ir::ConstExpr, is_callee: &dyn Fn(&str) -> bool) -> bool {
    use sv::ir::ConstExpr;
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => false,
        ConstExpr::Select { expr, bit } => {
            const_calls(expr, is_callee) || const_calls(bit, is_callee)
        }
        ConstExpr::Function { name, args, .. } => {
            is_callee(name) || args.iter().any(|arg| const_calls(arg, is_callee))
        }
        ConstExpr::Unary { expr, .. } => const_calls(expr, is_callee),
        ConstExpr::Binary { left, right, .. } => {
            const_calls(left, is_callee) || const_calls(right, is_callee)
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            const_calls(condition, is_callee)
                || const_calls(then_expr, is_callee)
                || const_calls(else_expr, is_callee)
        }
    }
}

/// Whether `expr` has `part` as a subexpression.
fn const_contains(expr: &sv::ir::ConstExpr, part: &sv::ir::ConstExpr) -> bool {
    use sv::ir::ConstExpr;
    expr == part
        || match expr {
            ConstExpr::Literal(_) | ConstExpr::Ident(_) => false,
            ConstExpr::Select { expr, bit } => {
                const_contains(expr, part) || const_contains(bit, part)
            }
            ConstExpr::Function { args, .. } => args.iter().any(|arg| const_contains(arg, part)),
            ConstExpr::Unary { expr, .. } => const_contains(expr, part),
            ConstExpr::Binary { left, right, .. } => {
                const_contains(left, part) || const_contains(right, part)
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                const_contains(condition, part)
                    || const_contains(then_expr, part)
                    || const_contains(else_expr, part)
            }
        }
}

/// Collect the largest subexpressions of `expr` that call a subroutine and
/// also occur in `other`, left to right.
fn shared_calls<'e>(
    expr: &'e sv::ir::ConstExpr,
    other: &sv::ir::ConstExpr,
    is_callee: &dyn Fn(&str) -> bool,
    shared: &mut Vec<&'e sv::ir::ConstExpr>,
) {
    use sv::ir::ConstExpr;
    if !const_calls(expr, is_callee) {
        return;
    }
    if const_contains(other, expr) {
        shared.push(expr);
        return;
    }
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => {}
        ConstExpr::Select { expr, bit } => {
            shared_calls(expr, other, is_callee, shared);
            shared_calls(bit, other, is_callee, shared);
        }
        ConstExpr::Function { args, .. } => args
            .iter()
            .for_each(|arg| shared_calls(arg, other, is_callee, shared)),
        ConstExpr::Unary { expr, .. } => shared_calls(expr, other, is_callee, shared),
        ConstExpr::Binary { left, right, .. } => {
            shared_calls(left, other, is_callee, shared);
            shared_calls(right, other, is_callee, shared);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            shared_calls(condition, other, is_callee, shared);
            shared_calls(then_expr, other, is_callee, shared);
            shared_calls(else_expr, other, is_callee, shared);
        }
    }
}

/// Whether the select positions of an assignment target call a subroutine.
pub(super) fn lvalue_calls(lvalue: &sv::ir::LValue, is_callee: &dyn Fn(&str) -> bool) -> bool {
    match lvalue {
        sv::ir::LValue::Ident(_) => false,
        sv::ir::LValue::Select { msb, lsb, .. } => {
            const_calls(msb, is_callee) || const_calls(lsb, is_callee)
        }
    }
}

/// Whether an expression calls a user subroutine.
pub(super) fn expr_calls(expr: &sv::ir::Expr, is_callee: &dyn Fn(&str) -> bool) -> bool {
    match expr {
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => false,
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            expr_calls(expr, is_callee)
                || const_calls(msb, is_callee)
                || const_calls(lsb, is_callee)
        }
        sv::ir::Expr::Resize { expr, .. } | sv::ir::Expr::Unary { expr, .. } => {
            expr_calls(expr, is_callee)
        }
        sv::ir::Expr::Concat(parts) | sv::ir::Expr::RepeatConcat { parts, .. } => {
            parts.iter().any(|part| expr_calls(part, is_callee))
        }
        sv::ir::Expr::Binary { left, right, .. } => {
            expr_calls(left, is_callee) || expr_calls(right, is_callee)
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_calls(condition, is_callee)
                || expr_calls(then_expr, is_callee)
                || expr_calls(else_expr, is_callee)
        }
        sv::ir::Expr::Call { name, args } => {
            is_callee(name) || args.iter().any(|arg| expr_calls(arg, is_callee))
        }
        sv::ir::Expr::Inside { expr, items } => {
            expr_calls(expr, is_callee)
                || items
                    .iter()
                    .flat_map(sv::ir::InsideItem::exprs)
                    .any(|operand| expr_calls(operand, is_callee))
        }
    }
}

/// The names written by the assignments (and through call arguments) of a
/// statement list.
pub(super) fn written_names(stmts: &[sv::ir::Stmt], names: &mut HashSet<String>) {
    for stmt in stmts {
        stmt.walk(&mut |stmt| match stmt {
            sv::ir::Stmt::Assign { lhs, .. } => {
                names.insert(lhs.name().to_string());
            }
            sv::ir::Stmt::AssignConcat { parts, .. } => {
                names.extend(parts.iter().map(|part| part.name().to_string()));
            }
            sv::ir::Stmt::Local { name, .. } => {
                names.insert(name.clone());
            }
            sv::ir::Stmt::Call { args, .. } => {
                for arg in args.iter().flatten() {
                    expr_idents(arg, names);
                }
            }
            _ => {}
        });
    }
}

/// The lvalue an output argument writes back to.
pub(super) fn lvalue_from_expr(expr: &sv::ir::Expr) -> Option<Vec<sv::ir::LValue>> {
    match expr {
        sv::ir::Expr::Ident(name) => Some(vec![sv::ir::LValue::Ident(name.clone())]),
        sv::ir::Expr::Select { expr, msb, lsb, .. } => match &**expr {
            sv::ir::Expr::Ident(name) => Some(vec![sv::ir::LValue::Select {
                name: name.clone(),
                msb: msb.clone(),
                lsb: lsb.clone(),
                array_slice_width: None,
                array_slice_reversed: false,
            }]),
            _ => None,
        },
        sv::ir::Expr::Concat(parts) => {
            let mut lvalues = Vec::new();
            for part in parts {
                lvalues.extend(lvalue_from_expr(part)?);
            }
            Some(lvalues)
        }
        sv::ir::Expr::Resize { expr, .. } => lvalue_from_expr(expr),
        _ => None,
    }
}

/// Whether a statement list may leave its enclosing block early: a `break`
/// or `continue` of the innermost loop, or a `return`.
pub(super) fn stmts_may_jump(stmts: &[sv::ir::Stmt], in_loop: bool) -> bool {
    stmts.iter().any(|stmt| stmt_may_jump(stmt, in_loop))
}

pub(super) fn stmt_may_jump(stmt: &sv::ir::Stmt, in_loop: bool) -> bool {
    match stmt {
        sv::ir::Stmt::Break | sv::ir::Stmt::Continue => !in_loop,
        sv::ir::Stmt::Return(_) => true,
        sv::ir::Stmt::If {
            then_body,
            else_body,
            ..
        } => stmts_may_jump(then_body, in_loop) || stmts_may_jump(else_body, in_loop),
        sv::ir::Stmt::Case { items, default, .. } => {
            items.iter().any(|item| stmts_may_jump(&item.body, in_loop))
                || default
                    .as_ref()
                    .is_some_and(|body| stmts_may_jump(body, in_loop))
        }
        // A `break` or `continue` inside a nested loop belongs to it.
        sv::ir::Stmt::Loop { body, .. } => stmts_may_jump(body, true),
        _ => false,
    }
}

/// The user subroutine calls an expression makes, outermost first, with their
/// arguments.
pub(super) fn collect_calls(
    expr: &sv::ir::Expr,
    calls: &mut Vec<(String, Vec<Option<sv::ir::Expr>>)>,
) {
    match expr {
        sv::ir::Expr::Call { name, args } => {
            calls.push((name.clone(), args.iter().cloned().map(Some).collect()));
            for arg in args {
                collect_calls(arg, calls);
            }
        }
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => {}
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            collect_calls(expr, calls);
            collect_const_calls(msb, calls);
            collect_const_calls(lsb, calls);
        }
        sv::ir::Expr::Resize { expr, .. } | sv::ir::Expr::Unary { expr, .. } => {
            collect_calls(expr, calls)
        }
        sv::ir::Expr::Concat(parts) => parts.iter().for_each(|part| collect_calls(part, calls)),
        sv::ir::Expr::RepeatConcat { count, parts } => {
            collect_const_calls(count, calls);
            parts.iter().for_each(|part| collect_calls(part, calls))
        }
        sv::ir::Expr::Binary { left, right, .. } => {
            collect_calls(left, calls);
            collect_calls(right, calls);
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_calls(condition, calls);
            collect_calls(then_expr, calls);
            collect_calls(else_expr, calls);
        }
        sv::ir::Expr::Inside { expr, items } => {
            collect_calls(expr, calls);
            for item in items {
                item.exprs()
                    .into_iter()
                    .for_each(|operand| collect_calls(operand, calls));
            }
        }
    }
}

/// The calls of a constant-expression operand, such as a run-time select
/// position.
fn collect_const_calls(
    expr: &sv::ir::ConstExpr,
    calls: &mut Vec<(String, Vec<Option<sv::ir::Expr>>)>,
) {
    use sv::ir::ConstExpr;
    match expr {
        ConstExpr::Function { name, args, .. } => {
            calls.push((
                name.clone(),
                args.iter().map(expr_from_const_expr).collect(),
            ));
            args.iter().for_each(|arg| collect_const_calls(arg, calls));
        }
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => {}
        ConstExpr::Select { expr, bit } => {
            collect_const_calls(expr, calls);
            collect_const_calls(bit, calls);
        }
        ConstExpr::Unary { expr, .. } => collect_const_calls(expr, calls),
        ConstExpr::Binary { left, right, .. } => {
            collect_const_calls(left, calls);
            collect_const_calls(right, calls);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_const_calls(condition, calls);
            collect_const_calls(then_expr, calls);
            collect_const_calls(else_expr, calls);
        }
    }
}

/// The calls a statement makes in its own expressions (not in the statements
/// it contains), including a call statement itself.
/// The calls in the select positions of an assignment target.
fn lvalue_position_calls(
    lvalue: &sv::ir::LValue,
    calls: &mut Vec<(String, Vec<Option<sv::ir::Expr>>)>,
) {
    if let sv::ir::LValue::Select { msb, lsb, .. } = lvalue {
        collect_const_calls(msb, calls);
        collect_const_calls(lsb, calls);
    }
}

pub(super) fn stmt_calls(
    stmt: &sv::ir::Stmt,
    calls: &mut Vec<(String, Vec<Option<sv::ir::Expr>>)>,
) {
    let mut exprs: Vec<&sv::ir::Expr> = Vec::new();
    match stmt {
        sv::ir::Stmt::Call { name, args } => {
            calls.push((name.clone(), args.clone()));
            exprs.extend(args.iter().flatten());
        }
        sv::ir::Stmt::Assign { lhs, rhs, .. } => {
            lvalue_position_calls(lhs, calls);
            exprs.push(rhs);
        }
        sv::ir::Stmt::AssignConcat { parts, rhs, .. } => {
            parts
                .iter()
                .for_each(|part| lvalue_position_calls(part, calls));
            exprs.push(rhs);
        }
        sv::ir::Stmt::Eval(rhs) => exprs.push(rhs),
        sv::ir::Stmt::If { condition, .. } => exprs.push(condition),
        sv::ir::Stmt::Case {
            selector, items, ..
        } => {
            exprs.push(selector);
            for item in items {
                for label in &item.labels {
                    match label {
                        sv::ir::CaseLabel::Value(value) => exprs.push(value),
                        sv::ir::CaseLabel::Range { low, high } => {
                            exprs.push(low);
                            exprs.push(high);
                        }
                    }
                }
            }
        }
        sv::ir::Stmt::Loop {
            kind, condition, ..
        } => {
            if let sv::ir::LoopKind::Repeat(count) = kind {
                exprs.push(count);
            }
            exprs.extend(condition);
        }
        sv::ir::Stmt::Return(Some(value)) => exprs.push(value),
        sv::ir::Stmt::Local {
            init: Some(init), ..
        } => exprs.push(init),
        sv::ir::Stmt::SystemTask { args, .. } => {
            for arg in args {
                if let sv::ir::SystemTaskArg::Expr(expr) = arg {
                    exprs.push(expr);
                }
            }
        }
        _ => {}
    }
    for expr in exprs {
        collect_calls(expr, calls);
    }
}

/// The calls in the default values a call uses for its omitted arguments.
pub(super) fn default_calls(
    subroutine: &sv::ir::Subroutine,
    args: &[Option<sv::ir::Expr>],
    calls: &mut Vec<(String, Vec<Option<sv::ir::Expr>>)>,
) {
    for (position, param) in subroutine.params.iter().enumerate() {
        if matches!(args.get(position), None | Some(None))
            && let Some(default) = &param.default
        {
            collect_calls(default, calls);
        }
    }
}
