//! DPI-C imports (IEEE 1800-2023 clause 35).
//!
//! An imported function is called through the C ABI. Arguments and results
//! are passed by value as the C types of IEEE 1800-2023 35.5.6; other
//! argument kinds, tasks, exports and `context` imports are rejected.

use super::*;
use crate::ir::{DpiArgument, DpiImport, DpiType};

/// The largest number of arguments an imported function may take; the
/// limit of `celox_sir::MAX_EXTERN_CALL_ARGUMENTS`.
const MAX_DPI_ARGUMENTS: usize = 16;

/// Every DPI-C import of the active generate items.
pub(super) fn dpi_imports_from_module_node(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<DpiImport>, AnalyzerError> {
    let mut imports = Vec::new();
    for item in generate::items(node, tree, const_env, type_aliases)? {
        for child in RefNode::ModuleOrGenerateItem(item.node) {
            if let RefNode::DpiImportExport(declaration) = child {
                imports.push(dpi_import(declaration, tree)?);
            }
        }
    }
    Ok(imports)
}

fn dpi_import(
    declaration: &sv_parser::DpiImportExport,
    tree: &SyntaxTree,
) -> Result<DpiImport, AnalyzerError> {
    let function = match declaration {
        sv_parser::DpiImportExport::ImportFunction(function) => function,
        sv_parser::DpiImportExport::ImportTask(_) => {
            return Err(unsupported("DPI-C task import"));
        }
        sv_parser::DpiImportExport::ExportFunction(_)
        | sv_parser::DpiImportExport::ExportTask(_) => {
            return Err(unsupported("DPI-C export"));
        }
    };
    let (_, spec, property, linkage, prototype, _) = &function.nodes;
    if matches!(spec, sv_parser::DpiSpecString::Dpi(_)) {
        return Err(unsupported("\"DPI\" import (only \"DPI-C\" is supported)"));
    }
    let pure = match property {
        None => false,
        Some(sv_parser::DpiFunctionImportProperty::Pure(_)) => true,
        Some(sv_parser::DpiFunctionImportProperty::Context(_)) => {
            return Err(unsupported("DPI-C context import"));
        }
    };
    let (_, return_type, name, ports) = &prototype.nodes.0.nodes;
    let name = identifier_text(RefNode::FunctionIdentifier(name), tree)
        .ok_or_else(|| unsupported("DPI-C import without a name"))?;
    let c_name = match linkage {
        Some((c_identifier, _)) => tree
            .get_str(&c_identifier.nodes.0)
            .map(str::to_string)
            .ok_or_else(|| unsupported("DPI-C import with an unreadable C name"))?,
        None => name.clone(),
    };
    let return_type = match return_type {
        sv_parser::DataTypeOrVoid::Void(_) => None,
        sv_parser::DataTypeOrVoid::DataType(data_type) => {
            Some(dpi_type(data_type, tree, "result")?)
        }
    };
    let mut arguments = Vec::new();
    if let Some(list) = ports.as_ref().and_then(|ports| ports.nodes.1.as_ref()) {
        // An argument without a direction has the previous one's (IEEE
        // 1800-2023 13.3), and the first defaults to `input`.
        let mut input = true;
        for item in list.nodes.0.contents() {
            let (_, item_direction, _, data_type, declarator) = &item.nodes;
            match item_direction {
                Some(sv_parser::TfPortDirection::PortDirection(port_direction)) => {
                    input = matches!(**port_direction, sv_parser::PortDirection::Input(_));
                }
                Some(sv_parser::TfPortDirection::ConstRef(_)) => {
                    return Err(unsupported("DPI-C const ref argument"));
                }
                None => {}
            }
            if !input {
                return Err(unsupported("DPI-C output, inout or ref argument"));
            }
            let Some((identifier, dimensions, default)) = declarator else {
                return Err(unsupported("DPI-C argument without a name"));
            };
            if !dimensions.is_empty() {
                return Err(unsupported("DPI-C unpacked array argument"));
            }
            if default.is_some() {
                return Err(unsupported("DPI-C argument default value"));
            }
            let r#type = match data_type {
                sv_parser::DataTypeOrImplicit::DataType(data_type) => {
                    dpi_type(data_type, tree, "argument")?
                }
                // An argument without a type is a one-bit `logic`.
                sv_parser::DataTypeOrImplicit::ImplicitDataType(implicit) => {
                    if !implicit.nodes.1.is_empty() {
                        return Err(unsupported("DPI-C packed array argument"));
                    }
                    DpiType::Logic
                }
            };
            arguments.push(DpiArgument::new(
                identifier_text(RefNode::PortIdentifier(identifier), tree)
                    .ok_or_else(|| unsupported("DPI-C argument without a name"))?,
                r#type,
            ));
        }
    }
    if arguments.len() > MAX_DPI_ARGUMENTS {
        return Err(unsupported(format!(
            "DPI-C import with more than {MAX_DPI_ARGUMENTS} arguments"
        )));
    }
    Ok(DpiImport::new(name, c_name, pure, return_type, arguments))
}

/// The C type passed for a DPI-C argument or result (IEEE 1800-2023 35.5.6).
fn dpi_type(
    data_type: &sv_parser::DataType,
    tree: &SyntaxTree,
    role: &str,
) -> Result<DpiType, AnalyzerError> {
    let signed = |signing: &Option<sv_parser::Signing>, default: bool| match signing {
        Some(sv_parser::Signing::Signed(_)) => true,
        Some(sv_parser::Signing::Unsigned(_)) => false,
        None => default,
    };
    match data_type {
        sv_parser::DataType::Atom(atom) => {
            let (atom_type, signing) = &atom.nodes;
            let width = match atom_type {
                sv_parser::IntegerAtomType::Byte(_) => 8,
                sv_parser::IntegerAtomType::Shortint(_) => 16,
                sv_parser::IntegerAtomType::Int(_) => 32,
                sv_parser::IntegerAtomType::Longint(_) => 64,
                sv_parser::IntegerAtomType::Integer(_) | sv_parser::IntegerAtomType::Time(_) => {
                    return Err(unsupported(format!(
                        "DPI-C {role} of four-state integer type"
                    )));
                }
            };
            Ok(DpiType::Integer {
                width,
                signed: signed(signing, true),
            })
        }
        sv_parser::DataType::Vector(vector) => {
            let (vector_type, _, dimensions) = &vector.nodes;
            if !dimensions.is_empty() {
                return Err(unsupported(format!("DPI-C packed array {role}")));
            }
            Ok(match vector_type {
                sv_parser::IntegerVectorType::Bit(_) => DpiType::Bit,
                sv_parser::IntegerVectorType::Logic(_) | sv_parser::IntegerVectorType::Reg(_) => {
                    DpiType::Logic
                }
            })
        }
        _ => {
            let text = tree
                .get_str(data_type)
                .map(str::trim)
                .unwrap_or("this type")
                .to_string();
            Err(unsupported(format!("DPI-C {role} of type `{text}`")))
        }
    }
}
