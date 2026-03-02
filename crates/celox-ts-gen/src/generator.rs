use serde::Serialize;
use std::collections::HashMap;
use veryl_analyzer::ir::{Component, Declaration, Ir, Module, TypeKind, VarKind};
use veryl_parser::resource_table;

/// A generated TypeScript module definition (`.d.ts` + `.js` + `.md` content).
#[derive(Debug, Clone)]
pub struct GeneratedModule {
    pub module_name: String,
    pub dts_content: String,
    pub js_content: String,
    pub md_content: String,
    pub ports: HashMap<String, JsonPortInfo>,
    pub events: Vec<String>,
    pub instances: Vec<JsonInstanceInfo>,
}

/// JSON-serializable port information for `--json` output.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonPortInfo {
    pub direction: &'static str,
    pub r#type: &'static str,
    pub width: usize,
    pub is4state: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub array_dims: Option<Vec<usize>>,
}

/// Top-level JSON output for `celox-gen-ts --json`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonOutput {
    pub project_path: String,
    pub modules: Vec<JsonModuleEntry>,
    pub file_modules: HashMap<String, Vec<String>>,
}

/// Per-module entry in JSON output.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonModuleEntry {
    pub module_name: String,
    pub source_file: String,
    pub dts_content: String,
    pub md_content: String,
    pub ports: HashMap<String, JsonPortInfo>,
    pub events: Vec<String>,
    pub instances: Vec<JsonInstanceInfo>,
}

/// JSON-serializable instance information for `--json` output.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonInstanceInfo {
    pub name: String,
    pub module_name: String,
    pub ports: HashMap<String, JsonPortInfo>,
    pub instances: Vec<JsonInstanceInfo>,
}

/// Extract port information from a module.
fn extract_ports(module: &Module) -> Vec<PortInfo> {
    let mut ports = Vec::new();

    for (var_path, var_id) in &module.ports {
        let variable = &module.variables[var_id];

        let name = var_path
            .0
            .iter()
            .map(|s| resource_table::get_str_value(*s).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(".");

        let is_hierarchical = var_path.0.len() > 1;

        let element_width = variable.total_width().unwrap_or(1);

        let array_dims: Option<Vec<usize>> = {
            let dims: Vec<usize> = variable
                .r#type
                .array
                .iter()
                .filter_map(|d| *d)
                .collect();
            if dims.is_empty() { None } else { Some(dims) }
        };

        let total_width = element_width
            * variable.r#type.total_array().unwrap_or(1);

        let type_info = classify_type(&variable.r#type.kind);
        let direction = match variable.kind {
            VarKind::Input => "input",
            VarKind::Output => "output",
            VarKind::Inout => "inout",
            _ => continue,
        };

        let is_4state = is_4state_type(&variable.r#type.kind);

        ports.push(PortInfo {
            name,
            direction,
            type_info,
            width: if array_dims.is_some() { element_width } else { total_width },
            is_4state,
            is_output: variable.kind == VarKind::Output,
            is_hierarchical,
            array_dims,
        });
    }

    // Sort ports for deterministic output
    ports.sort_by(|a, b| a.name.cmp(&b.name));
    ports
}

/// Instance info collected from module declarations.
struct InstanceInfo {
    name: String,
    module_name: String,
    ports: Vec<PortInfo>,
    children: Vec<InstanceInfo>,
}

/// Extract instance information from a module's declarations.
fn extract_instances(module: &Module) -> Vec<InstanceInfo> {
    let mut instances = Vec::new();

    for decl in &module.declarations {
        let Declaration::Inst(inst) = decl else {
            continue;
        };
        let Component::Module(sub_module) = &inst.component else {
            continue;
        };

        let inst_name = resource_table::get_str_value(inst.name)
            .unwrap_or_else(|| "unknown".to_string());
        let sub_module_name = resource_table::get_str_value(sub_module.name)
            .unwrap_or_else(|| "unknown".to_string());

        let ports = extract_ports(sub_module);
        let children = extract_instances(sub_module);

        instances.push(InstanceInfo {
            name: inst_name,
            module_name: sub_module_name,
            ports,
            children,
        });
    }

    // Sort instances for deterministic output
    instances.sort_by(|a, b| a.name.cmp(&b.name));
    instances
}

/// Convert ports to JSON-serializable format.
fn ports_to_json(ports: &[PortInfo]) -> HashMap<String, JsonPortInfo> {
    ports
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                JsonPortInfo {
                    direction: p.direction,
                    r#type: type_info_str(p.type_info),
                    width: p.width,
                    is4state: p.is_4state,
                    array_dims: p.array_dims.clone(),
                },
            )
        })
        .collect()
}

/// Convert instance info to JSON-serializable format.
fn instances_to_json(instances: &[InstanceInfo]) -> Vec<JsonInstanceInfo> {
    instances
        .iter()
        .map(|inst| JsonInstanceInfo {
            name: inst.name.clone(),
            module_name: inst.module_name.clone(),
            ports: ports_to_json(&inst.ports),
            instances: instances_to_json(&inst.children),
        })
        .collect()
}

/// Generate TypeScript type definitions and JS metadata for all modules in the IR.
pub fn generate_all(ir: &Ir) -> Vec<GeneratedModule> {
    let mut result = Vec::new();

    for component in &ir.components {
        let Component::Module(module) = component else {
            continue;
        };

        let module_name = resource_table::get_str_value(module.name)
            .unwrap_or_else(|| "unknown".to_string());

        let ports = extract_ports(module);
        let instances = extract_instances(module);

        let dts_content = generate_dts(&module_name, &ports, &instances);
        let js_content = generate_js(&module_name, &ports);
        let md_content = generate_md(&module_name, &ports);

        let json_ports = ports_to_json(&ports);

        let events: Vec<String> = ports
            .iter()
            .filter(|p| p.type_info == TypeInfo::Clock)
            .map(|p| p.name.clone())
            .collect();

        let json_instances = instances_to_json(&instances);

        result.push(GeneratedModule {
            module_name,
            dts_content,
            js_content,
            md_content,
            ports: json_ports,
            events,
            instances: json_instances,
        });
    }

    // Sort modules for deterministic output
    result.sort_by(|a, b| a.module_name.cmp(&b.module_name));
    result
}

struct PortInfo {
    name: String,
    direction: &'static str,
    type_info: TypeInfo,
    width: usize,
    is_4state: bool,
    is_output: bool,
    is_hierarchical: bool,
    array_dims: Option<Vec<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TypeInfo {
    Clock,
    Reset,
    Logic,
    Bit,
    Other,
}

fn classify_type(kind: &TypeKind) -> TypeInfo {
    match kind {
        TypeKind::Clock | TypeKind::ClockPosedge | TypeKind::ClockNegedge => TypeInfo::Clock,
        TypeKind::Reset
        | TypeKind::ResetAsyncHigh
        | TypeKind::ResetAsyncLow
        | TypeKind::ResetSyncHigh
        | TypeKind::ResetSyncLow => TypeInfo::Reset,
        TypeKind::Logic => TypeInfo::Logic,
        TypeKind::Bit => TypeInfo::Bit,
        _ => TypeInfo::Other,
    }
}

fn is_4state_type(kind: &TypeKind) -> bool {
    matches!(
        kind,
        TypeKind::Clock
            | TypeKind::ClockPosedge
            | TypeKind::ClockNegedge
            | TypeKind::Reset
            | TypeKind::ResetAsyncHigh
            | TypeKind::ResetAsyncLow
            | TypeKind::ResetSyncHigh
            | TypeKind::ResetSyncLow
            | TypeKind::Logic
    )
}

fn ts_type_for_width(_width: usize) -> &'static str {
    "bigint"
}

fn type_info_str(info: TypeInfo) -> &'static str {
    match info {
        TypeInfo::Clock => "clock",
        TypeInfo::Reset => "reset",
        TypeInfo::Logic => "logic",
        TypeInfo::Bit => "bit",
        TypeInfo::Other => "other",
    }
}

fn generate_dts(module_name: &str, ports: &[PortInfo], instances: &[InstanceInfo]) -> String {
    let mut out = String::new();

    out.push_str("import type { ModuleDefinition } from \"@celox-sim/celox\";\n\n");

    // Ports interface — exclude clock ports (they go to events)
    out.push_str(&format!("export interface {}Ports {{\n", module_name));
    write_dts_port_members(&mut out, ports, "  ");
    write_dts_instance_members(&mut out, instances, "  ");
    out.push_str("}\n\n");

    // Module definition export
    out.push_str(&format!(
        "export declare const {}: ModuleDefinition<{}Ports>;\n",
        module_name, module_name
    ));

    out
}

/// Write port members to a DTS interface body at the given indentation level.
fn write_dts_port_members(out: &mut String, ports: &[PortInfo], indent: &str) {
    for port in ports {
        if port.type_info == TypeInfo::Clock {
            continue;
        }
        let ts_type = ts_type_for_width(port.width);
        let readonly = if port.is_output { "readonly " } else { "" };
        if port.array_dims.is_some() {
            let set_method = if port.is_output {
                String::new()
            } else {
                format!(" set(i: number, value: {}): void;", ts_type)
            };
            out.push_str(&format!(
                "{}{}{}: {{ at(i: number): {};{} readonly length: number }};\n",
                indent, readonly, port.name, ts_type, set_method,
            ));
        } else {
            out.push_str(&format!("{}{}{}: {};\n", indent, readonly, port.name, ts_type));
        }
    }
}

/// Write instance members as inline object types in a DTS interface body.
fn write_dts_instance_members(out: &mut String, instances: &[InstanceInfo], indent: &str) {
    for inst in instances {
        let child_indent = format!("{}  ", indent);
        out.push_str(&format!("{}readonly {}: {{\n", indent, inst.name));
        write_dts_port_members(out, &inst.ports, &child_indent);
        write_dts_instance_members(out, &inst.children, &child_indent);
        out.push_str(&format!("{}}};\n", indent));
    }
}

fn generate_js(module_name: &str, ports: &[PortInfo]) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "exports.{} = {{\n  __celox_module: true,\n  name: \"{}\",\n",
        module_name, module_name
    ));
    out.push_str(&format!(
        "  source: require(\"fs\").readFileSync(__dirname + \"/../{}.veryl\", \"utf-8\"),\n",
        module_name
    ));

    // Ports object
    out.push_str("  ports: {\n");
    for port in ports {
        let type_str = type_info_str(port.type_info);
        let four_state_str = if port.is_4state { ", is4state: true" } else { "" };
        let hierarchical_str = if port.is_hierarchical {
            ", hierarchical: true"
        } else {
            ""
        };
        let array_dims_str = match &port.array_dims {
            Some(dims) => {
                let dims_str = dims
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(", arrayDims: [{}]", dims_str)
            }
            None => String::new(),
        };
        out.push_str(&format!(
            "    {}: {{ direction: \"{}\", type: \"{}\", width: {}{}{}{} }},\n",
            port.name, port.direction, type_str, port.width, four_state_str, hierarchical_str, array_dims_str
        ));
    }
    out.push_str("  },\n");

    // Events list (clock ports)
    let events: Vec<&str> = ports
        .iter()
        .filter(|p| p.type_info == TypeInfo::Clock)
        .map(|p| p.name.as_str())
        .collect();
    out.push_str("  events: [");
    for (i, ev) in events.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("\"{}\"", ev));
    }
    out.push_str("],\n");

    out.push_str("};\n");

    out
}

fn generate_md(module_name: &str, ports: &[PortInfo]) -> String {
    let mut out = String::new();

    out.push_str(&format!("# {}\n\n", module_name));

    // Ports table
    out.push_str("## Ports\n\n");
    out.push_str("| Port | Direction | Type | Width | TS Type | 4-State |\n");
    out.push_str("|------|-----------|------|-------|---------|--------|\n");

    for port in ports {
        if port.type_info == TypeInfo::Clock {
            continue;
        }
        let ts_type = ts_type_for_width(port.width);
        let readonly_note = if port.is_output {
            format!("`{}` (readonly)", ts_type)
        } else {
            format!("`{}`", ts_type)
        };
        let four_state = if port.is_4state { "yes" } else { "no" };
        let type_str = type_info_str(port.type_info);

        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            port.name, port.direction, type_str, port.width, readonly_note, four_state,
        ));
    }

    // Events table
    let clock_ports: Vec<&PortInfo> = ports
        .iter()
        .filter(|p| p.type_info == TypeInfo::Clock)
        .collect();

    if !clock_ports.is_empty() {
        out.push_str("\n## Events\n\n");
        out.push_str("| Event | Port |\n");
        out.push_str("|-------|------|\n");
        for port in &clock_ports {
            out.push_str(&format!("| {} | {} |\n", port.name, port.name));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use veryl_analyzer::{Analyzer, Context, attribute_table, ir::Ir, symbol_table};
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

    fn generate_from_source(code: &str) -> Vec<GeneratedModule> {
        symbol_table::clear();
        attribute_table::clear();

        let metadata = Metadata::create_default("prj").unwrap();
        let parser = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = Ir::default();

        analyzer.analyze_pass1("prj", &parser.veryl);
        Analyzer::analyze_post_pass1();
        analyzer.analyze_pass2("prj", &parser.veryl, &mut context, Some(&mut ir));
        Analyzer::analyze_post_pass2();

        generate_all(&ir)
    }

    #[test]
    fn test_basic_adder() {
        let code = r#"
module Adder (
    clk: input clock,
    rst: input reset,
    a: input logic<16>,
    b: input logic<16>,
    sum: output logic<17>,
) {
    always_comb {
        sum = a + b;
    }
}
"#;
        let modules = generate_from_source(code);
        assert_eq!(modules.len(), 1);
        assert_snapshot!("basic_adder_dts", modules[0].dts_content);
        assert_snapshot!("basic_adder_js", modules[0].js_content);
        assert_snapshot!("basic_adder_md", modules[0].md_content);
    }

    #[test]
    fn test_wide_port_bigint() {
        let code = r#"
module WideAdder (
    clk: input clock,
    a: input logic<64>,
    b: input logic<64>,
    sum: output logic<65>,
) {
    always_comb {
        sum = a + b;
    }
}
"#;
        let modules = generate_from_source(code);
        assert_eq!(modules.len(), 1);
        assert_snapshot!("wide_port_dts", modules[0].dts_content);
        assert_snapshot!("wide_port_js", modules[0].js_content);
        assert_snapshot!("wide_port_md", modules[0].md_content);
    }

    #[test]
    fn test_bit_type() {
        let code = r#"
module BitModule (
    clk: input clock,
    en: input bit,
    data: input bit<8>,
    result: output bit<8>,
) {
    always_comb {
        result = data;
    }
}
"#;
        let modules = generate_from_source(code);
        assert_eq!(modules.len(), 1);
        assert_snapshot!("bit_type_dts", modules[0].dts_content);
        assert_snapshot!("bit_type_js", modules[0].js_content);
        assert_snapshot!("bit_type_md", modules[0].md_content);
    }

    #[test]
    fn test_output_only() {
        let code = r#"
module ConstGen (
    val: output logic<8>,
) {
    always_comb {
        val = 42;
    }
}
"#;
        let modules = generate_from_source(code);
        assert_eq!(modules.len(), 1);
        assert_snapshot!("output_only_dts", modules[0].dts_content);
        assert_snapshot!("output_only_js", modules[0].js_content);
        assert_snapshot!("output_only_md", modules[0].md_content);
    }

    #[test]
    fn test_no_clock_comb_only() {
        let code = r#"
module PureAdder (
    a: input logic<8>,
    b: input logic<8>,
    sum: output logic<9>,
) {
    always_comb {
        sum = a + b;
    }
}
"#;
        let modules = generate_from_source(code);
        assert_eq!(modules.len(), 1);
        assert_snapshot!("no_clock_dts", modules[0].dts_content);
        assert_snapshot!("no_clock_js", modules[0].js_content);
        assert_snapshot!("no_clock_md", modules[0].md_content);
    }

    #[test]
    fn test_array_port() {
        let code = r#"
module Counter #(
    param N: u32 = 4,
)(
    clk: input clock,
    rst: input reset,
    cnt: output logic<32>[N],
) {
    for i in 0..N: g {
        always_ff (clk, rst) {
            if_reset {
                cnt[i] = 0;
            } else {
                cnt[i] += 1;
            }
        }
    }
}
"#;
        let modules = generate_from_source(code);
        assert_eq!(modules.len(), 1);
        assert_snapshot!("array_port_dts", modules[0].dts_content);
        assert_snapshot!("array_port_js", modules[0].js_content);
        assert_snapshot!("array_port_md", modules[0].md_content);

        // Verify arrayDims is set correctly
        let cnt_port = &modules[0].ports["cnt"];
        assert_eq!(cnt_port.width, 32);
        assert_eq!(cnt_port.array_dims, Some(vec![4]));
    }

    #[test]
    fn test_instance_hierarchy() {
        let code = r#"
module Sub (
    clk: input clock,
    i_data: input logic<8>,
    o_data: output logic<8>,
) {
    always_comb {
        o_data = i_data;
    }
}

module Top (
    clk: input clock,
    rst: input reset,
    top_in: input logic<8>,
    top_out: output logic<8>,
) {
    inst u_sub: Sub (
        clk,
        i_data: top_in,
        o_data: top_out,
    );
}
"#;
        let modules = generate_from_source(code);
        let top = modules.iter().find(|m| m.module_name == "Top").unwrap();
        assert_snapshot!("instance_hierarchy_dts", top.dts_content);
        assert_snapshot!("instance_hierarchy_js", top.js_content);
        assert_snapshot!("instance_hierarchy_md", top.md_content);

        // Verify instance info
        assert_eq!(top.instances.len(), 1);
        assert_eq!(top.instances[0].name, "u_sub");
        assert!(top.instances[0].ports.contains_key("i_data"));
        assert!(top.instances[0].ports.contains_key("o_data"));
    }
}
