//! Small, fail-closed JSON projection of the actual Veryl 0.21.0 analyzer IR.
//! This executable does not parse Veryl itself or implement hardware execution.
use serde_json::{Value as Json, json};
use std::path::PathBuf;
use veryl_analyzer::ir::{
    Component, Comptime, Declaration, Expression, Factor, Ir, Module, Op, Statement, Type,
    TypeKind, ValueVariant, VarIndex, VarSelect,
};
use veryl_analyzer::{Analyzer, Context, attribute_table, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

type ProbeResult<T> = Result<T, String>;

fn ty(t: &Type) -> ProbeResult<Json> {
    if !matches!(
        t.kind,
        TypeKind::Bit
            | TypeKind::Logic
            | TypeKind::Clock
            | TypeKind::ClockPosedge
            | TypeKind::ClockNegedge
            | TypeKind::Reset
            | TypeKind::ResetAsyncHigh
            | TypeKind::ResetAsyncLow
            | TypeKind::ResetSyncHigh
            | TypeKind::ResetSyncLow
    ) {
        return Err(format!("unsupported type: {t:?}"));
    }
    let width = t
        .total_width()
        .ok_or_else(|| format!("unresolved width: {t:?}"))?;
    if !t.array.is_empty() {
        return Err(format!("unpacked arrays outside probe subset: {t:?}"));
    }
    Ok(
        json!({"kind":format!("{:?}",t.kind),"width":width,"signed":t.signed,"packed_shape":t.width().as_slice(),"array_shape":t.array.as_slice(),"is_2state":t.is_2state()}),
    )
}

fn val(v: &veryl_analyzer::value::Value) -> Json {
    json!({"payload":v.payload().to_string(),"mask_xz":v.mask_xz().to_string(),"width":v.width(),"signed":v.signed()})
}

fn comptime(c: &Comptime) -> ProbeResult<Json> {
    Ok(
        json!({"type":ty(&c.r#type)?,"expr_context":{"width":c.expr_context.width,"signed":c.expr_context.signed,"is_const":c.expr_context.is_const,"is_global":c.expr_context.is_global},"is_const":c.is_const,"evaluated":c.evaluated}),
    )
}

fn scalar_access(index: &VarIndex, select: &VarSelect) -> ProbeResult<()> {
    if !index.0.is_empty() || !select.is_empty() {
        return Err("index/part-select outside probe subset".into());
    }
    Ok(())
}

fn expr(e: &Expression) -> ProbeResult<Json> {
    let mut ret = match e {
        Expression::Term(f) => match f.as_ref() {
            Factor::Variable(id, index, select, _) => {
                if !index.0.is_empty() {
                    return Err("unpacked index outside probe subset".into());
                }
                let selection = if select.is_empty() {
                    None
                } else {
                    Some(
                        json!({"indices": select.0.iter().map(expr).collect::<ProbeResult<Vec<_>>>()?,
                        "range": select.1.as_ref().map(|(op, bound)| Ok::<Json, String>(json!({"op":format!("{op:?}"),"bound":expr(bound)?}))).transpose()?}),
                    )
                };
                json!({"kind":"variable","id":id.to_string(),"select":selection})
            }
            Factor::Value(c) => match &c.value {
                ValueVariant::Numeric(v) => json!({"kind":"value","value":val(v)}),
                _ => return Err(format!("non-numeric value outside probe subset: {c:?}")),
            },
            _ => return Err(format!("factor outside probe subset: {f:?}")),
        },
        Expression::Unary(op, arg, _) => {
            json!({"kind":"unary","op":format!("{op:?}"),"arg":expr(arg)?})
        }
        Expression::Binary(lhs, Op::As, _rhs, _) => {
            json!({"kind":"cast","arg":expr(lhs)?})
        }
        Expression::Binary(lhs, op, rhs, _) => {
            json!({"kind":"binary","op":format!("{op:?}"),"lhs":expr(lhs)?,"rhs":expr(rhs)?})
        }
        Expression::Ternary(cond, t, f, _) => {
            json!({"kind":"ternary","cond":expr(cond)?,"true_side":expr(t)?,"false_side":expr(f)?})
        }
        _ => return Err(format!("expression outside probe subset: {e:?}")),
    };
    ret["comptime"] = comptime(e.comptime())?;
    Ok(ret)
}

fn statements(ss: &[Statement]) -> ProbeResult<Vec<Json>> {
    ss.iter().map(|s| Ok(match s {
        Statement::Assign(a) => {
            let dst = a.dst.iter().map(|d| {
                scalar_access(&d.index,&d.select)?;
                Ok(json!({"id":d.id.to_string(),"path":d.path.to_string(),"comptime":comptime(&d.comptime)?}))
            }).collect::<ProbeResult<Vec<_>>>()?;
            json!({"kind":"assign","width":a.width,"dst":dst,"expr":expr(&a.expr)?})
        }
        Statement::If(i) => json!({"kind":"if","cond":expr(&i.cond)?,"true_side":statements(&i.true_side)?,"false_side":statements(&i.false_side)?}),
        Statement::IfReset(i) => json!({"kind":"if_reset","true_side":statements(&i.true_side)?,"false_side":statements(&i.false_side)?}),
        Statement::Null => json!({"kind":"null"}),
        _ => return Err(format!("statement outside probe subset: {s}")),
    })).collect()
}

fn module(m: &Module) -> ProbeResult<Json> {
    if !m.functions.is_empty() || !m.interface_members.is_empty() {
        return Err("functions or interfaces outside probe subset".into());
    }
    let mut variables = m.variables.values().collect::<Vec<_>>();
    variables.sort_by_key(|v| v.path.to_string());
    let variables = variables.into_iter().map(|v| Ok(json!({"id":v.id.to_string(),"path":v.path.to_string(),"kind":v.kind.to_string(),"type":ty(&v.r#type)?,"analyzer_values":v.value.iter().map(val).collect::<Vec<_>>()}))).collect::<ProbeResult<Vec<_>>>()?;
    let declarations = m.declarations.iter().map(|d| Ok(match d {
        Declaration::Comb(c) => json!({"kind":"comb","statements":statements(&c.statements)?}),
        Declaration::Ff(f) => {
            scalar_access(&f.clock.index,&f.clock.select)?;
            let reset = if let Some(r) = &f.reset {
                scalar_access(&r.index,&r.select)?;
                Some(json!({"id":r.id.to_string(),"comptime":comptime(&r.comptime)?}))
            } else {None};
            json!({"kind":"ff","clock":{"id":f.clock.id.to_string(),"comptime":comptime(&f.clock.comptime)?},"reset":reset,"statements":statements(&f.statements)?})
        }
        Declaration::Null => json!({"kind":"null"}),
        _ => return Err(format!("declaration outside probe subset: {d}")),
    })).collect::<ProbeResult<Vec<_>>>()?;
    Ok(json!({"name":m.name.to_string(),"variables":variables,"declarations":declarations}))
}

fn analyze(paths: &[PathBuf]) -> ProbeResult<Json> {
    symbol_table::clear();
    attribute_table::clear();
    let metadata = Metadata::create_default("prj").map_err(|e| e.to_string())?;
    let analyzer = Analyzer::new(&metadata);
    let mut parsed = vec![];
    let mut warnings = vec![];
    let check = |stage: &str,
                 errors: Vec<veryl_analyzer::AnalyzerError>,
                 warnings: &mut Vec<String>|
     -> ProbeResult<()> {
        let mut failures = vec![];
        for e in errors {
            if e.is_error() {
                failures.push(format!("{e:?}"));
            } else {
                warnings.push(format!("{stage}: {e:?}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!("{stage}: {}", failures.join("\n")))
        }
    };
    for p in paths {
        let source = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let ast = Parser::parse(&source, p).map_err(|e| format!("parse {}: {e}", p.display()))?;
        check(
            "pass1",
            analyzer.analyze_pass1("prj", &ast.veryl),
            &mut warnings,
        )?;
        parsed.push(ast);
    }
    check("post_pass1", Analyzer::analyze_post_pass1(), &mut warnings)?;
    let mut context = Context::default();
    let mut ir = Ir::default();
    for ast in &parsed {
        check(
            "pass2",
            analyzer.analyze_pass2(&ast.veryl, &mut context, Some(&mut ir)),
            &mut warnings,
        )?;
    }
    check(
        "post_pass2",
        Analyzer::analyze_post_pass2(&ir),
        &mut warnings,
    )?;
    let modules = ir
        .components
        .iter()
        .map(|c| match c {
            Component::Module(m) => module(m),
            _ => Err("non-module component outside probe subset".into()),
        })
        .collect::<ProbeResult<Vec<_>>>()?;
    Ok(
        json!({"schema":"veryl-analyzer-probe-v1","analyzer_version":"0.21.0","source_files":paths,"metadata_defaults":{"clock_type":metadata.build.clock_type,"reset_type":metadata.build.reset_type},"warnings":warnings,"modules":modules,"analyzer_display":ir.to_string()}),
    )
}

fn main() {
    let paths = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        eprintln!("usage: veryl-analyzer-probe SOURCE.veryl [SOURCE2.veryl ...]");
        std::process::exit(2);
    }
    match analyze(&paths) {
        Ok(output) => println!("{}", serde_json::to_string_pretty(&output).unwrap()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
