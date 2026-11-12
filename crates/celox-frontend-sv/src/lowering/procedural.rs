//! State and helpers shared by the procedural lowerings.
//!
//! `always_comb` and continuous assignments are executed symbolically into
//! SLT (`comb`); `always_ff` bodies become imperative SIR (`ff`). Both lower
//! the statement IR of the analyzer, inline subroutine calls, and treat
//! procedural locals as hidden module variables.

mod calls;
mod checkpoint;
mod effects;
mod expr_types;
mod hidden;
mod loops;
mod memory_files;
mod sensitivity;
mod slt;
mod system_tasks;
mod writes;

use super::*;
use num_traits::{ToPrimitive, Zero};
use std::sync::Arc;

pub(super) use calls::*;
pub(super) use expr_types::*;
pub(super) use hidden::*;
pub(super) use loops::*;
pub(super) use memory_files::*;
pub(super) use slt::*;
pub(super) use system_tasks::*;
pub(super) use writes::*;

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
    /// The variable each name denotes. Rebind a name through
    /// [`Self::bind_name`], which keeps the storage index up to date.
    pub name_to_id: &'a mut HashMap<String, SourceVarId>,
    /// The storage of the subroutines by variable (see
    /// [`Self::is_static_storage`]), once a query asks for it.
    pub(super) storage: Option<out_of_line::StorageIndex>,
    pub next_id: SourceVarId,
    pub constants: &'a HashMap<String, i128>,
    pub parameter_types: &'a HashMap<String, (usize, bool)>,
    pub subroutines: Arc<HashMap<String, sv::ir::Subroutine>>,
    pub constant_functions: sv::RuntimeConstantFunctions<'a>,
    /// DPI-C imports of the module, by SystemVerilog name.
    pub dpi_imports: HashMap<String, sv::ir::DpiImport>,
    /// The extern functions the module calls.
    pub externs: Externs,
    pub four_state: bool,
    /// The module is a program (IEEE 1800-2023 24.3): its kernels honour
    /// `$exit` (24.7).
    pub program: bool,
    pub forceable: HashMap<SourceVarId, (SourceVarId, SourceVarId)>,
    /// Hidden variables created while lowering, for the caller to publish.
    pub created: Vec<SourceVarId>,
    temp_counter: usize,
    capture_counter: u64,
    /// What the module reports at run time.
    pub runtime: RuntimeReports,
    /// The sites of the module's function units.
    pub function_sites: FunctionSites,
    /// The change counts of the boxes of the module's units, in the order
    /// they were made (see `Ff::box_changes`): module variables that any
    /// unit may bump. A box's field names its count by the count's id
    /// (`ChangeKey::Box`), apart from the counts of class properties.
    pub box_changes: Vec<SourceVarId>,
    /// The continuous assignments `$event[k] = expr` of the event
    /// expressions the module's processes wait on, by target.
    pub event_assignments: Vec<(SourceVarId, Vec<sv::ir::Stmt>)>,
    /// The declared bounds of each unpacked dimension, by variable.
    unpacked_bounds: HashMap<SourceVarId, Vec<(i128, i128)>>,
    /// Where the warnings found while lowering go. A process lowered more
    /// than once repeats its warnings; the frontend reports each once.
    warnings: &'a mut Vec<crate::warning::PendingWarning>,
    /// Where the calls that the lowering inlines are recorded, for the
    /// recursion report (see [`super::recursion`]).
    pub calls: Option<&'a mut super::recursion::CallGraph>,
    /// Whether a call of each subroutine asked about can make a host
    /// request, by name.
    requesting_subroutines: HashMap<String, bool>,
    /// The automatic storage that inlined subroutine activations copied,
    /// innermost last: each name, the variable it named before the copy,
    /// and the copy it names while the activation is lowered.
    pub(super) activation_copies: Vec<(String, SourceVarId, SourceVarId)>,
}

/// The extern functions the module calls.
#[derive(Default)]
pub(super) struct Externs {
    /// Extern functions the module calls, by local index.
    pub functions: Vec<ExternFunction>,
    /// Number of extern calls lowered so far.
    pub calls: usize,
}

/// What the module reports at run time.
#[derive(Default)]
pub(super) struct RuntimeReports {
    /// Runtime event sites (`$display`, assertions, ...) of the module, by
    /// local site number.
    pub event_sites: Vec<RuntimeEventSite>,
    /// Runtime errors (fatal assertions) of the module, by local code.
    pub errors: HashMap<i64, RuntimeErrorInfo<SourceVarId>>,
}

/// The sites of the module's function units.
#[derive(Default)]
pub(super) struct FunctionSites {
    /// The host request sites of the module's function units, which every
    /// process of the module lists after its own (see
    /// [`celox_design::SiteProcess`]), numbered per family.
    pub host_request: Vec<celox_design::HostRequestSite<SourceVarId>>,
    /// The wait sites of the module's function units, which every process
    /// that can suspend inside a call lists after its own; a unit suspends
    /// with the number of the site among these, which the kernel's call
    /// turns into one among its process's sites.
    pub wait: Vec<celox_design::WaitSite<SourceVarId>>,
    /// The nonblocking assignment sites of the module's function units,
    /// listed and numbered as [`Self::wait`] are.
    pub nba: Vec<celox_design::NbaSite<SourceVarId>>,
    /// The runtime event site of the flush points of the units' waits
    /// (IEEE 1800-2023 16.4.2), made at the first.
    pub flush: Option<u32>,
}

/// What every lowering of a module reads of its subroutines, made once for
/// the module: its subroutines by name, and the functions a lowering may
/// evaluate.
pub(super) struct ModuleTables {
    subroutines: Arc<HashMap<String, sv::ir::Subroutine>>,
    constant_functions: sv::RuntimeConstantTable,
}

impl ModuleTables {
    pub fn new(module: &sv::ir::Module) -> Self {
        Self {
            subroutines: Arc::new(
                module
                    .subroutines()
                    .iter()
                    .map(|subroutine| (subroutine.name.clone(), subroutine.clone()))
                    .collect(),
            ),
            constant_functions: sv::RuntimeConstantFunctions::table(module),
        }
    }
}

impl<'a> ProcModule<'a> {
    /// The lowering state of `module`, whose tables `tables` are.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        module: &sv::ir::Module,
        tables: &ModuleTables,
        variables: &'a mut HashMap<SourceVarId, SvVariable>,
        name_to_id: &'a mut HashMap<String, SourceVarId>,
        constants: &'a HashMap<String, i128>,
        parameter_types: &'a HashMap<String, (usize, bool)>,
        four_state: bool,
        warnings: &'a mut Vec<crate::warning::PendingWarning>,
    ) -> Self {
        let next_id = SourceVarId(
            variables
                .keys()
                .map(|id| id.0 + 1)
                .max()
                .unwrap_or_default(),
        );
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
                .map(|range| super::unpacked_range_bounds(range, constants, parameter_types))
                .collect();
            if let Some(bounds) = bounds.filter(|bounds| !bounds.is_empty()) {
                unpacked_bounds.insert(id, bounds);
            }
        }
        let forceable = variables
            .iter()
            .filter_map(|(&enabled, var)| {
                let (target, true) = var.force_control? else {
                    return None;
                };
                let value = variables.iter().find_map(|(&id, var)| {
                    (var.force_control == Some((target, false))).then_some(id)
                })?;
                Some((target, (enabled, value)))
            })
            .collect();
        Self {
            forceable,
            variables,
            name_to_id,
            storage: None,
            next_id,
            constants,
            parameter_types,
            subroutines: Arc::clone(&tables.subroutines),
            constant_functions: sv::RuntimeConstantFunctions::with_table(
                &tables.constant_functions,
                constants,
                four_state,
            ),
            dpi_imports,
            externs: Externs::default(),
            four_state,
            program: module.is_program(),
            created: Vec::new(),
            temp_counter: 0,
            capture_counter: 0,
            runtime: RuntimeReports::default(),
            function_sites: FunctionSites::default(),
            box_changes: Vec::new(),
            event_assignments: Vec::new(),
            unpacked_bounds,
            warnings,
            calls: None,
            requesting_subroutines: Default::default(),
            activation_copies: Vec::new(),
        }
    }

    /// Report a warning of the statement at `site`.
    pub fn warn(&mut self, kind: crate::FrontendWarningKind, site: Option<&sv::ir::SourceSpan>) {
        self.warnings.push((kind, site.cloned()));
    }

    /// Register a runtime event site and return its local number.
    pub fn event_site(&mut self, site: RuntimeEventSite) -> u32 {
        self.runtime.event_sites.push(site);
        (self.runtime.event_sites.len() - 1) as u32
    }

    /// Register a runtime error and return its local code.
    pub fn runtime_error(&mut self, message: String) -> i64 {
        let code = self.runtime.errors.len() as i64 + 1;
        self.runtime.errors.insert(
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

    pub fn force_controls(&self, id: SourceVarId) -> Option<(SourceVarId, SourceVarId)> {
        self.forceable.get(&id).copied()
    }

    pub fn var(&self, id: SourceVarId) -> &SvVariable {
        &self.variables[&id]
    }

    pub fn is_hidden(&self, id: SourceVarId) -> bool {
        self.variables.get(&id).is_some_and(|var| var.hidden)
    }

    pub fn subroutine(&self, name: &str) -> Option<&sv::ir::Subroutine> {
        self.subroutines.get(name)
    }

    /// The local index of the extern function `import` links to. Imports
    /// of one C name with different prototypes get separate entries, which
    /// design assembly reports as a conflict.
    pub fn extern_function(&mut self, import: &sv::ir::DpiImport) -> u32 {
        let extern_type = |r#type: sv::ir::DpiType| match r#type {
            sv::ir::DpiType::Bit => ExternType::Bit,
            sv::ir::DpiType::Logic => ExternType::Logic,
            sv::ir::DpiType::Integer { width, signed } => ExternType::Integer { width, signed },
            sv::ir::DpiType::String => ExternType::String,
            sv::ir::DpiType::BitVector { width, .. } => ExternType::BitVector { width },
            sv::ir::DpiType::LogicVector { width, .. } => ExternType::LogicVector { width },
            // A pointer, 64 bits on the supported hosts.
            sv::ir::DpiType::Chandle => ExternType::Integer {
                width: 64,
                signed: false,
            },
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
                directions: import
                    .arguments()
                    .iter()
                    .map(|argument| match argument.direction() {
                        sv::ir::DpiDirection::Input => ExternDirection::Input,
                        sv::ir::DpiDirection::Output => ExternDirection::Output,
                        sv::ir::DpiDirection::Inout => ExternDirection::Inout,
                    })
                    .collect(),
            },
        };
        let index = self
            .externs
            .functions
            .iter()
            .position(|known| *known == function)
            .unwrap_or_else(|| {
                self.externs.functions.push(function);
                self.externs.functions.len() - 1
            });
        u32::try_from(index).expect("extern function count fits u32")
    }
}

/// The identifiers an expression reads.
pub(super) fn expr_idents(expr: &sv::ir::Expr, names: &mut HashSet<String>) {
    fn const_idents(expr: &sv::ir::ConstExpr, names: &mut HashSet<String>) {
        match expr {
            sv::ir::ConstExpr::Ident(name) => {
                names.insert(name.clone());
            }
            sv::ir::ConstExpr::Literal(_) => {}
            sv::ir::ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                const_idents(expr, names);
                const_idents(msb, names);
                const_idents(lsb, names);
            }
            sv::ir::ConstExpr::Select { expr, bit } => {
                const_idents(expr, names);
                const_idents(bit, names);
            }
            sv::ir::ConstExpr::Function { args, .. } => {
                args.iter().for_each(|arg| const_idents(arg, names))
            }
            sv::ir::ConstExpr::Resize { expr, .. } | sv::ir::ConstExpr::Unary { expr, .. } => {
                const_idents(expr, names)
            }
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
        sv::ir::Expr::Call { args, .. } => args
            .iter()
            .flatten()
            .for_each(|arg| expr_idents(arg, names)),
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
