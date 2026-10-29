//! DPI-C imports and exports (IEEE 1800-2023 clause 35).
//!
//! An imported function is called through the C ABI. Arguments and results
//! are passed by value as the C types of IEEE 1800-2023 35.5.6; other
//! argument kinds, tasks and `context` imports are rejected.
//!
//! An export only lets foreign code in a DPI call chain call the subroutine
//! and does not change it (35.7). Only a `context` import may call an
//! exported subroutine (35.5.3), so an accepted export is checked and has no
//! further effect.

use super::*;
use crate::ir::{DpiArgument, DpiImport, DpiType};

/// The largest number of arguments an imported function may take; the
/// limit of `celox_sir::MAX_EXTERN_CALL_ARGUMENTS`.
const MAX_DPI_ARGUMENTS: usize = 16;

/// Every DPI-C import of the already elaborated native body items. Scope
/// selection is shared with the other collectors, not rediscovered from a CST.
pub(super) fn imports(
    items: &[generate::Item],
    tree: &AnalysisContext<'_>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<DpiImport>, AnalyzerError> {
    let syntax = tree.declarations();
    let mut imports = Vec::new();
    let mut exports = HashMap::<(String, String), String>::default();
    for item in items {
        let _interface_scope = item.interface_scope();
        if let celox_sv_syntax::BodyItemKind::Dpi(id) = syntax.body_item(item.id).kind {
            let declaration = syntax.dpi_declaration(id);
            if declaration.import {
                imports.push(import(id, syntax)?);
            } else {
                export(declaration, item, items, tree, type_aliases, &mut exports)?;
            }
        }
    }
    Ok(imports)
}

/// Check the export declaration of `item` against the subroutine it names in
/// the same elaborated scope (IEEE 1800-2023 35.7). `exports` maps the scopes
/// and C names exported so far to their subroutines.
fn export(
    declaration: &celox_sv_syntax::DpiDeclaration,
    item: &generate::Item,
    items: &[generate::Item],
    tree: &AnalysisContext<'_>,
    type_aliases: &HashMap<String, Type>,
    exports: &mut HashMap<(String, String), String>,
) -> Result<(), AnalyzerError> {
    let syntax = tree.declarations();
    use celox_sv_syntax::{BodyItemKind, Direction, DpiKind, SubroutineKind};
    if declaration.specifier == "\"DPI\"" {
        return Err(unsupported("\"DPI\" export (only \"DPI-C\" is supported)"));
    }
    let name = instances::normalize_identifier(&declaration.name.spelling);
    let invalid = |detail: String| AnalyzerError::InvalidDpiExport {
        name: name.clone(),
        detail,
    };
    let scope = item.scope_name();
    let subroutine = items
        .iter()
        .find_map(|candidate| match syntax.body_item(candidate.id).kind {
            BodyItemKind::Subroutine(id) => {
                let subroutine = syntax.subroutine(id);
                (subroutine.scope.is_empty()
                    && !subroutine.prototype
                    && instances::normalize_identifier(&subroutine.name.spelling) == name)
                    .then_some(subroutine)
            }
            _ => None,
        })
        .ok_or_else(|| {
            invalid("an export names a subroutine defined in its own scope".to_owned())
        })?;
    let task = subroutine.kind == SubroutineKind::Task;
    if task != (declaration.kind == DpiKind::Task) {
        return Err(invalid(format!(
            "`{name}` is a {}",
            if task { "task" } else { "function" }
        )));
    }
    // An exported subroutine takes the argument types of an import (35.7):
    // no class handles and no `ref` formals (35.5.6). Types are bound in the
    // scope of the export, as its subroutine's are.
    let aliases = item.type_aliases(type_aliases);
    let context = PackedDimensions::new(HashMap::default(), &item.env, &aliases);
    let class_handle = |id| -> Result<bool, AnalyzerError> {
        Ok(type_binding::bind(id, tree, &context)?.is_some_and(|ty| ty.class().is_some()))
    };
    let ports = subroutine.ports.iter().flatten().chain(
        subroutine
            .port_declarations
            .iter()
            .flat_map(|group| &group.ports),
    );
    for port in ports {
        if port.direction == Some(Direction::Ref) {
            return Err(invalid("a `ref` formal cannot be exported".to_owned()));
        }
        if class_handle(port.data_type)? {
            return Err(invalid(
                "a class handle formal cannot be exported".to_owned(),
            ));
        }
    }
    if let Some(ty) = subroutine.return_type
        && class_handle(ty)?
    {
        return Err(invalid(
            "a class handle result cannot be exported".to_owned(),
        ));
    }
    let c_name = declaration.c_name.as_ref().map_or_else(
        || declaration.name.spelling.clone(),
        |name| name.spelling.clone(),
    );
    if exports
        .iter()
        .any(|((exported_scope, _), exported)| exported_scope == scope && *exported == name)
    {
        return Err(invalid("a subroutine is exported once".to_owned()));
    }
    if let Some(exported) = exports.insert((scope.to_owned(), c_name.clone()), name.clone()) {
        return Err(invalid(format!(
            "`{exported}` is already exported as `{c_name}`"
        )));
    }
    Ok(())
}

pub(super) fn import(
    id: celox_sv_syntax::DpiId,
    syntax: &celox_sv_syntax::Declarations,
) -> Result<DpiImport, AnalyzerError> {
    lower_native_dpi(syntax.dpi_declaration(id), syntax)
}

/// Bind an owned native body declaration to the existing C ABI/IR contract.
/// No legacy declaration, prototype or type nodes participate in this conversion.
fn lower_native_dpi(
    declaration: &celox_sv_syntax::DpiDeclaration,
    syntax: &celox_sv_syntax::Declarations,
) -> Result<DpiImport, AnalyzerError> {
    use celox_sv_syntax::{DataTypeKind, Direction, DpiKind, DpiProperty};
    if !declaration.import {
        return Err(unsupported("DPI-C export"));
    }
    if declaration.kind == DpiKind::Task {
        return Err(unsupported("DPI-C task import"));
    }
    if declaration.specifier == "\"DPI\"" {
        return Err(unsupported("\"DPI\" import (only \"DPI-C\" is supported)"));
    }
    let pure = match declaration.property {
        Some(DpiProperty::Context) => return Err(unsupported("DPI-C context import")),
        Some(DpiProperty::Pure) => true,
        None => false,
    };
    let name = declaration.name.spelling.clone();
    let c_name = declaration
        .c_name
        .as_ref()
        .map_or_else(|| name.clone(), |name| name.spelling.clone());
    let result = declaration
        .return_type
        .ok_or_else(|| unsupported("DPI-C import without a result type"))?;
    let return_type = if matches!(&syntax.data_type(result).kind,DataTypeKind::Builtin(name) if name=="void")
    {
        None
    } else {
        Some(native_dpi_type(result, syntax, "result")?)
    };
    let mut input = true;
    let mut arguments = Vec::new();
    for port in &declaration.ports {
        if port.constant {
            return Err(unsupported("DPI-C const ref argument"));
        }
        if let Some(direction) = port.direction {
            input = direction == Direction::Input;
        }
        if !input {
            return Err(unsupported("DPI-C output, inout or ref argument"));
        }
        let name = port
            .name
            .as_ref()
            .ok_or_else(|| unsupported("DPI-C argument without a name"))?;
        if !port.dimensions.is_empty() {
            return Err(unsupported("DPI-C unpacked array argument"));
        }
        if port.default.is_some() {
            return Err(unsupported("DPI-C argument default value"));
        }
        arguments.push(DpiArgument::new(
            name.spelling.clone(),
            native_dpi_type(port.data_type, syntax, "argument")?,
        ));
    }
    if arguments.len() > MAX_DPI_ARGUMENTS {
        return Err(unsupported(format!(
            "DPI-C import with more than {MAX_DPI_ARGUMENTS} arguments"
        )));
    }
    Ok(DpiImport::new(name, c_name, pure, return_type, arguments))
}

fn native_dpi_type(
    id: celox_sv_syntax::TypeId,
    syntax: &celox_sv_syntax::Declarations,
    role: &str,
) -> Result<DpiType, AnalyzerError> {
    use celox_sv_syntax::DataTypeKind;
    let data_type = syntax.data_type(id);
    match &data_type.kind {
        DataTypeKind::Builtin(name)
            if matches!(name.as_str(), "byte" | "shortint" | "int" | "longint") =>
        {
            let width = match name.as_str() {
                "byte" => 8,
                "shortint" => 16,
                "int" => 32,
                _ => 64,
            };
            Ok(DpiType::Integer {
                width,
                signed: data_type.signed.unwrap_or(true),
            })
        }
        DataTypeKind::Builtin(name) if matches!(name.as_str(), "integer" | "time") => Err(
            unsupported(format!("DPI-C {role} of four-state integer type")),
        ),
        DataTypeKind::Implicit => {
            if !data_type.packed_dimensions.is_empty() {
                return Err(unsupported(format!("DPI-C packed array {role}")));
            }
            Ok(DpiType::Logic)
        }
        DataTypeKind::Builtin(name) if matches!(name.as_str(), "bit" | "logic" | "reg") => {
            if !data_type.packed_dimensions.is_empty() {
                return Err(unsupported(format!("DPI-C packed array {role}")));
            }
            Ok(if name == "bit" {
                DpiType::Bit
            } else {
                DpiType::Logic
            })
        }
        _ => Err(unsupported(format!(
            "DPI-C {role} of type `{}`",
            syntax.token_text(data_type.tokens).trim()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_body_prototypes_bind_linkage_signedness_and_empty_argument_lists() {
        let code = "`define ARG input byte unsigned value\nmodule M; import \"DPI-C\" pure c_add = function shortint add(`ARG, input logic flag); import \"DPI-C\" function void tick(); endmodule";
        let source = crate::analyze_source(code, Path::new("native_dpi.sv")).unwrap();
        let imports = source.modules()[0].dpi_imports();
        assert_eq!(imports[0].c_name(), "c_add");
        assert!(imports[0].is_pure());
        assert_eq!(
            imports[0].return_type(),
            Some(DpiType::Integer {
                width: 16,
                signed: true
            })
        );
        assert_eq!(
            imports[0].arguments()[0].r#type(),
            DpiType::Integer {
                width: 8,
                signed: false
            }
        );
        assert_eq!(imports[0].arguments()[1].r#type(), DpiType::Logic);
        assert_eq!(imports[1].return_type(), None);
        assert!(imports[1].arguments().is_empty());
    }

    #[test]
    fn native_prototypes_preserve_existing_unsupported_diagnostics() {
        for (declaration, want) in [
            (
                "import \"DPI-C\" context function int f();",
                "DPI-C context import",
            ),
            ("import \"DPI-C\" task f();", "DPI-C task import"),
            (
                "export \"DPI\" function f; function int f(); return 1; endfunction",
                "\"DPI\" export (only \"DPI-C\" is supported)",
            ),
            (
                "import \"DPI-C\" function int f(input int);",
                "DPI-C argument without a name",
            ),
            (
                "import \"DPI-C\" function int f(input value);",
                "DPI-C argument without a name",
            ),
            (
                "import \"DPI-C\" function int f(output int x);",
                "DPI-C output, inout or ref argument",
            ),
            (
                "import \"DPI-C\" function int f(input int x[]);",
                "DPI-C unpacked array argument",
            ),
            (
                "import \"DPI-C\" function int f(input int x=1);",
                "DPI-C argument default value",
            ),
            (
                "import \"DPI-C\" function logic[3:0] f();",
                "DPI-C packed array result",
            ),
        ] {
            let code = format!("module M; {declaration} endmodule");
            assert_eq!(
                crate::analyze_source(&code, Path::new("unsupported_dpi.sv")).unwrap_err(),
                AnalyzerError::Unsupported(want.into())
            );
        }
    }

    #[test]
    fn exports_leave_their_subroutines_unchanged() {
        let code = "module M(output int y);
            export \"DPI-C\" function f;
            export \"DPI-C\" c_t = task t;
            function int f(input int a, string s); return a + s.len(); endfunction
            task t(output int o); o = 3; endtask
            initial begin int o; t(o); y = f(o, \"ab\"); end
        endmodule";
        let source = crate::analyze_source(code, Path::new("dpi_exports.sv")).unwrap();
        assert!(source.modules()[0].dpi_imports().is_empty());
    }

    #[test]
    fn exports_bind_formal_types_in_their_scope() {
        // A class elsewhere that a local typedef hides does not make the
        // formal a class handle.
        let code = "package p; class C; endclass endpackage
        module M(output int y);
            typedef int C;
            export \"DPI-C\" function f;
            function int f(C a); return a + 1; endfunction
            initial y = f(2);
        endmodule";
        let source = crate::analyze_source(code, Path::new("dpi_export_scopes.sv")).unwrap();
        assert!(source.modules()[0].dpi_imports().is_empty());
    }

    #[test]
    fn exports_that_35_7_forbids_are_rejected() {
        for (items, want) in [
            (
                "export \"DPI-C\" function f;",
                "an export names a subroutine defined in its own scope",
            ),
            (
                "export \"DPI-C\" function f; task f(); endtask",
                "`f` is a task",
            ),
            (
                "export \"DPI-C\" task f; function int f(); return 1; endfunction",
                "`f` is a function",
            ),
            (
                "export \"DPI-C\" function f; export \"DPI-C\" g = function f; \
                 function int f(); return 1; endfunction",
                "a subroutine is exported once",
            ),
            (
                "export \"DPI-C\" f = function f; export \"DPI-C\" f = function g; \
                 function int f(); return 1; endfunction function int g(); return 2; endfunction",
                "`f` is already exported as `f`",
            ),
            (
                "export \"DPI-C\" function f; function int f(ref int a); return a; endfunction",
                "a `ref` formal cannot be exported",
            ),
            (
                "class C; endclass export \"DPI-C\" function f; \
                 function int f(C c); return 1; endfunction",
                "a class handle formal cannot be exported",
            ),
        ] {
            let code = format!("module M; {items} endmodule");
            let error =
                crate::analyze_source(&code, Path::new("invalid_dpi_export.sv")).unwrap_err();
            assert!(
                matches!(&error, AnalyzerError::InvalidDpiExport { detail, .. } if detail == want),
                "{items}: {error:?}"
            );
        }
    }
}
