//! Reports of UVM's C library functions that Celox provides (UVM 1.2's
//! `src/dpi`). Where the C code reports an error or a message, it calls
//! `m_uvm_report_dpi`, which calls the function the UVM package exports for
//! it, `m__uvm_report_dpi` (UVM `base/uvm_globals.svh`): the report counts
//! as any other UVM report, its errors in the test's verdict. A function
//! that Celox provides records its report on the heap instead (see
//! `celox_heap::uvm_dpi`), and the scope that declares both the import and
//! `m__uvm_report_dpi` calls the import through a subroutine of the
//! import's name that passes the recorded report on:
//!
//! ```text
//! function T f(args); f = f%celox(args);
//!     if (report_int(0)) m__uvm_report_dpi(report_int(1), report_string(2),
//!         report_string(3), report_int(4), report_string(5), report_int(6));
//! endfunction
//! ```

use super::*;
use crate::ir::{DpiArgument, DpiBuiltin, DpiImport, DpiType};

/// The function the UVM package exports for the reports of its C library.
const REPORT_FUNCTION: &str = "m__uvm_report_dpi";
/// The hidden imports that read the fields of the recorded report.
const REPORT_INT: &str = "celox%uvm_report_int";
const REPORT_STRING: &str = "celox%uvm_report_string";

/// Whether the C function of `builtin` may report.
fn reports(builtin: DpiBuiltin) -> bool {
    matches!(
        builtin,
        DpiBuiltin::ReMatch | DpiBuiltin::GlobToRe | DpiBuiltin::DumpReCache | DpiBuiltin::RegComp
    )
}

use super::super::dpi::argument_type;

/// Call the imports of `imports` whose functions report through the UVM
/// package's `m__uvm_report_dpi` when `subroutines` has it (see the module
/// documentation): each is renamed, and `subroutines` gains one of its
/// name that calls it and passes its report on.
pub(in crate::ast) fn report_through_uvm(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    state: &mut BodyState<'_>,
    dimensions: &PackedDimensions,
    imports: &mut Vec<DpiImport>,
    subroutines: &mut Vec<Subroutine>,
) -> Converted<()> {
    let Some(report) = subroutines
        .iter()
        .find(|subroutine| subroutine.name == REPORT_FUNCTION && subroutine.params.len() == 6)
        .map(|subroutine| subroutine.name.clone())
    else {
        return Ok(());
    };
    let reporting = imports
        .iter()
        .filter(|import| import.builtin().is_some_and(reports))
        .cloned()
        .collect::<Vec<_>>();
    if reporting.is_empty() {
        return Ok(());
    }
    let int = DpiType::Integer {
        width: 32,
        signed: true,
    };
    for (name, builtin, result) in [
        (REPORT_INT, DpiBuiltin::ReportInt, int),
        (REPORT_STRING, DpiBuiltin::ReportString, DpiType::String),
    ] {
        if !imports.iter().any(|import| import.name() == name) {
            imports.push(
                DpiImport::new(
                    name.to_owned(),
                    name.replace('%', "_"),
                    false,
                    Some(result),
                    vec![DpiArgument::new("field".into(), int)],
                )
                .with_builtin(builtin),
            );
        }
    }
    for import in reporting {
        let hidden = format!("{}%celox", import.name());
        for known in imports.iter_mut() {
            if known.name() == import.name() {
                *known = known.renamed(hidden.clone());
            }
        }
        subroutines.push(wrapper(
            tree, session, state, dimensions, &import, &hidden, &report,
        )?);
    }
    Ok(())
}

/// The subroutine named as `import` that calls it as `hidden` and passes
/// its report on to `report`.
fn wrapper(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    state: &mut BodyState<'_>,
    dimensions: &PackedDimensions,
    import: &DpiImport,
    hidden: &str,
    report: &str,
) -> Converted<Subroutine> {
    let mut types = Vec::new();
    for argument in import.arguments() {
        let r#type = argument_type(session, argument.r#type())
            .ok_or_else(|| unsupported("argument type of a UVM DPI-C function"))?;
        types.push(r#type);
    }
    let result = match import.return_type() {
        Some(r#type) => Some(
            argument_type(session, r#type)
                .ok_or_else(|| unsupported("result type of a UVM DPI-C function"))?,
        ),
        None => None,
    };
    let ir = |session: &mut Session, r#type: Type| {
        super::super::ir_type(session, &tree.functions, r#type, &dimensions.const_env)
    };
    let mut builder = BodyBuilder::new(
        tree,
        session,
        dimensions,
        state,
        system_functions::Body::Subroutine,
    );
    builder.automatic = true;
    builder.set_owner(import.name().to_owned());
    let first_local = builder.state.locals.len();
    builder.push_scope();
    let formals = import
        .arguments()
        .iter()
        .zip(&types)
        .map(|(argument, r#type)| builder.declare(argument.name(), r#type.clone()))
        .collect::<Vec<_>>();
    let return_var = result
        .as_ref()
        .map(|r#type| builder.declare(import.name(), r#type.clone()));
    builder.pop_scope();
    let automatic_locals = builder.state.locals[first_local..]
        .iter()
        .map(|local| local.name.clone())
        .collect();
    drop(builder);
    let params = import
        .arguments()
        .iter()
        .zip(types)
        .zip(&formals)
        .map(|((argument, r#type), name)| SubroutineParam {
            name: name.clone(),
            source_name: argument.name().to_owned(),
            direction: ParamDirection::Input,
            r#type: ir(session, r#type),
            default: None,
            leaves: Vec::new(),
        })
        .collect();
    let field = |name: &str, field: u32| Expr::Call {
        callee: Callee::Subroutine(name.to_owned()),
        args: vec![Some(Expr::Literal(format!("32'sd{field}")))],
        arg_order: Vec::new(),
    };
    let actuals = formals
        .iter()
        .map(|name| Some(Expr::Ident(name.clone())))
        .collect::<Vec<_>>();
    let call = match &return_var {
        Some(name) => Stmt::Assign {
            lhs: LValue::Ident(name.clone()),
            rhs: Expr::Call {
                callee: Callee::Subroutine(hidden.to_owned()),
                args: actuals,
                arg_order: Vec::new(),
            },
            nonblocking: false,
        },
        None => Stmt::Call {
            name: hidden.to_owned(),
            args: actuals,
            arg_order: Vec::new(),
        },
    };
    let pass_on = Stmt::If {
        condition: Expr::Binary {
            left: Box::new(field(REPORT_INT, 0)),
            op: BinaryOp::Ne,
            right: Box::new(Expr::Literal("32'sd0".into())),
        },
        then_body: vec![Stmt::Call {
            name: report.to_owned(),
            args: vec![
                Some(field(REPORT_INT, 1)),
                Some(field(REPORT_STRING, 2)),
                Some(field(REPORT_STRING, 3)),
                Some(field(REPORT_INT, 4)),
                Some(field(REPORT_STRING, 5)),
                Some(field(REPORT_INT, 6)),
            ],
            arg_order: Vec::new(),
        }],
        else_body: Vec::new(),
    };
    Ok(Subroutine {
        name: import.name().to_owned(),
        is_task: false,
        automatic: true,
        automatic_locals,
        persistent_locals: Vec::new(),
        return_type: result.map(|r#type| ir(session, r#type)),
        params,
        return_var,
        body: vec![call, pass_on],
        sensitivity_body: Vec::new(),
    })
}
