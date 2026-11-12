use celox_design::{
    BitAccess, DisplaySizing, InstanceId, PortTypeKind, RuntimeEventKind, RuntimeEventSite,
    Severity, StateAddr,
};
use celox_testbench::{
    AssertMessage as GenericAssertMessage, ClockCount as GenericClockCount, ExprBytecode,
    ExprOpcode as TbOpcode, LoopBound as GenericLoopBound, SemanticArgument,
    SemanticComponentBinding, SemanticSignal, SemanticStatement, SourceLocation, StateLocation,
    TestbenchClock, TestbenchOperator as Op, TestbenchProgram, TestbenchSelection,
    TestbenchStatement as GenericTestbenchStatement, TestbenchTarget,
};
use fxhash::{FxHashMap as HashMap, FxHashSet};
use num_traits::{ToPrimitive as _, Zero as _};
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssertKind, CasePattern, Expression, Factor, ForBound, ForRange, Function,
    FunctionCall, HierAssignDestination, HierVarRef, Op as VerylOp, Statement, SystemFunctionInput,
    SystemFunctionKind, SystemFunctionOutput, TbMethod, TbMethodCall, VarId, VarIndex, VarSelect,
    VarSelectOp,
};
use veryl_analyzer::value::byte_value_to_string;
use veryl_parser::resource_table::{self, StrId};

use crate::{
    FrontendLookup, InstancePath, ParserError, SourceAddr, VariableInfo,
    VerylComponentEventBinding, VerylComponentInputBinding, VerylIdMap, VerylTestbenchSource,
    bitaccess::eval_constexpr,
    context_width::{
        ValueContext, binary_semantics, cast_semantics, expression_signed, get_expr_width,
    },
};

mod builder;
mod expr_compiler;
mod hierarchical;
mod observability;
mod validate;
use builder::*;
use expr_compiler::*;
pub(crate) use hierarchical::*;
pub use observability::*;
use validate::*;

type UnboundTbOpcode = TbOpcode<StateLocation<StateAddr>>;

const fn get_byte_size(width: usize) -> usize {
    width.div_ceil(8)
}

fn static_string_expr(expr: &Expression) -> Option<String> {
    if !expr.comptime().r#type.is_string() {
        return None;
    }
    let value = expr.comptime().get_value().ok()?;
    byte_value_to_string(value)
}

fn compile_assert_arg(
    input: &SystemFunctionInput,
    ec: &ExprCompiler<'_>,
) -> SemanticArgument<StateAddr> {
    let expr = &input.0;
    SemanticArgument {
        expr: ec.compile(expr),
        width: ec.natural_width(expr),
        signed: expression_signed(expr),
        is_string: expr.comptime().r#type.is_string(),
        float_precision: crate::float::precision(crate::float::type_of(expr)),
    }
}

fn compile_component_arg(
    input: &SystemFunctionInput,
    ec: &ExprCompiler<'_>,
) -> SemanticArgument<StateAddr> {
    let expr = &input.0;
    SemanticArgument {
        expr: ec.compile(expr),
        width: assert_arg_width(input),
        signed: expression_signed(expr),
        is_string: expr.comptime().r#type.is_string(),
        float_precision: crate::float::precision(crate::float::type_of(expr)),
    }
}

fn assert_arg_width(input: &SystemFunctionInput) -> usize {
    let expr = &input.0;
    let ctx_width = expr.comptime().expr_context.width;
    if ctx_width > 0 {
        ctx_width
    } else if let Some(type_width) = expr.comptime().r#type.total_width() {
        type_width
    } else if let Ok(value) = expr.comptime().get_value() {
        value.width()
    } else {
        0
    }
}

fn runtime_event_site_for_assert(
    kind: &AssertKind,
    args: &[SystemFunctionInput],
) -> RuntimeEventSite {
    let (template, value_args) = if args
        .first()
        .and_then(|arg| static_string_expr(&arg.0))
        .is_some()
    {
        (
            args.first().and_then(|arg| static_string_expr(&arg.0)),
            &args[1..],
        )
    } else {
        (None, args)
    };
    RuntimeEventSite {
        kind: match kind {
            AssertKind::Fatal => RuntimeEventKind::Report(Severity::Fatal),
            AssertKind::Continue => RuntimeEventKind::Report(Severity::Error),
        },
        template,
        sizing: DisplaySizing::Veryl,
        scope: None,
        location: None,
        assertion: None,
        deferred: None,
        timestamp_arg: false,
        time_format_arg: false,
        arg_widths: value_args.iter().map(assert_arg_width).collect(),
        arg_signed: value_args
            .iter()
            .map(|arg| expression_signed(&arg.0))
            .collect(),
        arg_float_precision: value_args
            .iter()
            .map(|arg| crate::float::precision(crate::float::type_of(&arg.0)))
            .collect(),
        arg_is_string: value_args
            .iter()
            .map(|arg| arg.0.comptime().r#type.is_string())
            .collect(),
        arg_is_handle: Vec::new(),
    }
}

fn function_body(
    funcs: &fxhash::FxHashMap<VarId, Function>,
    fc: &veryl_analyzer::ir::FunctionCall,
) -> Option<veryl_analyzer::ir::FunctionBody> {
    let func = funcs.get(&fc.id)?;
    if let Some(idx) = &fc.index {
        func.get_function(idx)
    } else {
        func.get_function(&[])
    }
}

fn collect_runtime_event_sites(
    stmts: &[Statement],
    funcs: &fxhash::FxHashMap<VarId, Function>,
    out: &mut Vec<RuntimeEventSite>,
) {
    for stmt in stmts {
        match stmt {
            Statement::SystemFunctionCall(sf) => {
                if let SystemFunctionKind::Assert { kind, args, .. } = &sf.kind {
                    out.push(runtime_event_site_for_assert(kind, args));
                }
            }
            Statement::If(s) => {
                collect_runtime_event_sites(&s.true_side, funcs, out);
                collect_runtime_event_sites(&s.false_side, funcs, out);
            }
            Statement::For(s) => collect_runtime_event_sites(&s.body, funcs, out),
            Statement::FunctionCall(fc) => {
                if let Some(body) = function_body(funcs, fc) {
                    collect_runtime_event_sites(&body.statements, funcs, out);
                }
            }
            _ => {}
        }
    }
}

fn count_assert_statements(
    stmts: &[Statement],
    funcs: &fxhash::FxHashMap<VarId, Function>,
) -> usize {
    let mut count = 0;
    for stmt in stmts {
        match stmt {
            Statement::SystemFunctionCall(sf) => {
                if matches!(&sf.kind, SystemFunctionKind::Assert { .. }) {
                    count += 1;
                }
            }
            Statement::If(s) => {
                count += count_assert_statements(&s.true_side, funcs);
                count += count_assert_statements(&s.false_side, funcs);
            }
            Statement::For(s) => count += count_assert_statements(&s.body, funcs),
            Statement::FunctionCall(fc) => {
                if let Some(body) = function_body(funcs, fc) {
                    count += count_assert_statements(&body.statements, funcs);
                }
            }
            _ => {}
        }
    }
    count
}

fn source_name(id: StrId) -> String {
    resource_table::get_str_value(id).unwrap_or_else(|| format!("{id}"))
}

fn source_path(path: &veryl_analyzer::ir::VarPath) -> Vec<String> {
    path.0.iter().copied().map(source_name).collect()
}

fn try_eval_const(expr: &Expression) -> Option<u64> {
    match expr {
        Expression::Term(f) => match f.as_ref() {
            Factor::Value(c) => c.get_value().ok().map(|v| v.payload_u64()),
            _ if expr.comptime().is_const => expr
                .comptime()
                .get_value()
                .ok()
                .map(|value| value.payload_u64()),
            _ => None,
        },
        _ if expr.comptime().is_const => expr
            .comptime()
            .get_value()
            .ok()
            .map(|value| value.payload_u64()),
        _ => None,
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn extract_source_location(
    token: &veryl_parser::token_range::TokenRange,
) -> Option<SourceLocation> {
    let t = &token.beg;
    let file = t
        .source
        .get_path()
        .and_then(resource_table::get_path_value)?;
    Some(SourceLocation {
        file: file.to_string_lossy().into_owned(),
        line: t.line,
        column: t.column,
    })
}

fn has_selected_testbench_destination(destination: &veryl_analyzer::ir::AssignDestination) -> bool {
    !destination.index.0.is_empty()
        || !destination.select.0.is_empty()
        || destination.select.1.is_some()
}

pub fn compile_semantic_testbench(
    lookup: &FrontendLookup,
    source: &VerylTestbenchSource,
    runtime_event_site_count: usize,
    random_seed: Option<u64>,
) -> Result<Option<TestbenchProgram<StateAddr>>, ParserError> {
    let sources: Vec<_> = source
        .sources()
        .filter(|source| source.initial_statements.is_some())
        .collect();
    if sources.is_empty() {
        return Ok(None);
    }
    let (_, reads) = collect_testbench_observability(lookup, source)?;
    let mut private_variables = Vec::new();
    for source in &sources {
        let instance = source.base_instance(lookup);
        for &id in &source.function_locals {
            if let Some(id) = source.id_map.instance_var(lookup, instance, id)
                && let Some((address, _)) = lookup.instance_variable(instance, id)
                && reads.contains(&address)
            {
                private_variables.push(address);
            }
        }
    }
    private_variables.sort_unstable();
    private_variables.dedup();
    let total_sites: usize = sources
        .iter()
        .map(|source| {
            count_assert_statements(
                source.initial_statements.as_ref().unwrap(),
                &source.functions,
            )
        })
        .sum();
    let mut site_end = runtime_event_site_count.saturating_sub(total_sites);
    let mut processes = Vec::new();
    let mut clocks = Vec::new();
    for source in sources {
        let initial_stmts = source.initial_statements.as_ref().unwrap();
        validate_testbench_statements(initial_stmts, lookup, source, &mut FxHashSet::default())?;
        site_end += count_assert_statements(initial_stmts, &source.functions);
        let mut builder = SemanticTestbenchBuilder::new(lookup, source, site_end);
        builder.prepared_readmem = super::readmem::prepare_testbench_memories(lookup, source)?;
        builder.build_event_map(initial_stmts);
        clocks.extend(builder.clock_generators());
        processes.extend(builder.convert(initial_stmts));
    }
    clocks.sort_unstable_by_key(|clock: &TestbenchClock<StateAddr>| clock.event);
    clocks.dedup_by_key(|clock| clock.event);
    let builder = SemanticTestbenchBuilder::new(lookup, source, runtime_event_site_count);
    let component_bindings = builder.convert_component_bindings()?;
    Ok(Some(
        TestbenchProgram::from_processes(processes)
            .with_clocks(clocks)
            .with_private_variables(private_variables)
            .with_random_seed_option(random_seed)
            .with_components(source.components.clone())
            .with_component_runtime(
                source.component_libraries.clone(),
                source.component_file_base.clone(),
                component_bindings,
            ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceVarId, VariableKind};
    use celox_design::{
        DomainKind, InstanceId, ModuleId, PortTypeKind, StateObjectId, VariableMetadata,
    };
    use veryl_analyzer::ir::{Comptime, VarIndex, VarPath, VarSelect};

    fn reference(inst_path: Vec<StrId>, var_path: VarPath) -> HierVarRef {
        HierVarRef {
            inst_path,
            var_path,
            index: VarIndex::default(),
            select: VarSelect::default(),
            comptime: Comptime::default(),
        }
    }

    fn lookup_with_child(child_name: StrId, variable_name: StrId) -> FrontendLookup {
        let root_instance = InstanceId(0);
        let child_instance = InstanceId(1);
        let root_module = ModuleId(0);
        let child_module = ModuleId(1);
        let var_id = SourceVarId(0);
        let source_address = SourceAddr {
            instance_id: child_instance,
            var_id,
        };
        let state_address = StateAddr {
            instance_id: child_instance,
            var_id: StateObjectId(0),
        };
        let path = vec![source_name(variable_name)];
        let info = VariableInfo {
            id: var_id,
            path: path.clone(),
            var_kind: VariableKind::Variable,
            signed: false,
            unpacked_ranges: Vec::new(),
            packed_dims: vec![8],
            metadata: VariableMetadata {
                width: 8,
                is_4state: true,
                kind: DomainKind::Other,
                type_kind: PortTypeKind::Logic,
                array_dims: Vec::new(),
                value_kind: Default::default(),
            },
        };

        let mut lookup = FrontendLookup::default();
        lookup
            .instance_ids
            .insert(InstancePath(Vec::new()), root_instance);
        lookup.instance_ids.insert(
            InstancePath(vec![(source_name(child_name), 0)]),
            child_instance,
        );
        lookup.instance_module.insert(root_instance, root_module);
        lookup.instance_module.insert(child_instance, child_module);
        lookup
            .module_variables
            .entry(child_module)
            .or_default()
            .insert(var_id, info);
        lookup
            .module_var_path_index
            .entry(child_module)
            .or_default()
            .insert(path, Some(var_id));
        lookup.source_to_state.insert(source_address, state_address);
        lookup.state_to_source.insert(state_address, source_address);
        lookup
    }

    #[test]
    fn hierarchical_reference_reports_missing_instance_segment() {
        let dut = resource_table::insert_str("dut");
        let missing = resource_table::insert_str("missing");
        let q = resource_table::insert_str("q");
        let lookup = lookup_with_child(dut, q);
        let reference = reference(vec![missing], VarPath(vec![q]));

        let error = resolve_hierarchical_reference(&lookup, &reference).unwrap_err();
        let ParserError::IllegalContext {
            feature, detail, ..
        } = error
        else {
            panic!("expected invalid hierarchical path diagnostic");
        };
        assert_eq!(feature, "hierarchical variable reference");
        assert!(detail.contains("instance `missing` was not found"));
    }

    #[test]
    fn hierarchical_reference_reports_missing_target_variable() {
        let dut = resource_table::insert_str("dut");
        let q = resource_table::insert_str("q");
        let missing = resource_table::insert_str("missing");
        let lookup = lookup_with_child(dut, q);
        let reference = reference(vec![dut], VarPath(vec![missing]));

        let error = resolve_hierarchical_reference(&lookup, &reference).unwrap_err();
        let ParserError::IllegalContext {
            feature, detail, ..
        } = error
        else {
            panic!("expected invalid hierarchical variable diagnostic");
        };
        assert_eq!(feature, "hierarchical variable reference");
        assert!(detail.contains("variable `missing` was not found"));
    }

    #[test]
    fn hierarchical_message_argument_width_uses_resolved_variable_metadata() {
        let dut = resource_table::insert_str("dut");
        let q = resource_table::insert_str("q");
        let lookup = lookup_with_child(dut, q);
        let source = VerylTestbenchSource::default();
        let compiler = ExprCompiler {
            lookup: &lookup,
            id_map: &source.id_map,
            constant_strings: &source.constant_strings,
            functions: &source.functions,
            base_instance: lookup.root_instance_and_module().unwrap().0,
        };
        let input = SystemFunctionInput(Expression::Term(Box::new(Factor::HierVariable(
            Box::new(reference(vec![dut], VarPath(vec![q]))),
        ))));

        assert_eq!(compile_assert_arg(&input, &compiler).width, 8);
    }

    #[test]
    fn component_argument_width_prefers_expression_context() {
        let dut = resource_table::insert_str("dut");
        let q = resource_table::insert_str("q");
        let lookup = lookup_with_child(dut, q);
        let source = VerylTestbenchSource::default();
        let compiler = ExprCompiler {
            lookup: &lookup,
            id_map: &source.id_map,
            constant_strings: &source.constant_strings,
            functions: &source.functions,
            base_instance: lookup.root_instance_and_module().unwrap().0,
        };
        let mut reference = reference(vec![dut], VarPath(vec![q]));
        reference.comptime.expr_context.width = 16;
        let input = SystemFunctionInput(Expression::Term(Box::new(Factor::HierVariable(
            Box::new(reference),
        ))));

        assert_eq!(compile_assert_arg(&input, &compiler).width, 8);
        assert_eq!(compile_component_arg(&input, &compiler).width, 16);
    }
}
