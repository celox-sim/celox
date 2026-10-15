//! Defer implicit continuous drivers on callable input-port actuals.
use super::*;
use sv_parser::{ListOfPortConnections, NamedPortConnection, RefNode};

pub(super) fn rewrites(
    source: &str,
    names: &TimedNames,
    tasks: &timed_ff::TaskTiming,
) -> Result<BTreeMap<usize, (usize, String)>> {
    let mut result = BTreeMap::new();
    if !sv_words(source)
        .iter()
        .any(|(_, _, word)| matches!(*word, "module" | "interface"))
    {
        return Ok(result);
    }
    let tree = celox_sv_analyzer::syntax::parse_source(source, Path::new("timed.sv"))?;
    let mut used_names = sv_words(source)
        .into_iter()
        .map(|(_, _, name)| name.to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    for node in &tree {
        let RefNode::ModuleInstantiation(module) = node else {
            continue;
        };
        let (start, end) = timed_initializers::span(&tree, node).ok_or("missing instance span")?;
        let module_name = tree
            .get_str(&module.nodes.0)
            .unwrap_or("")
            .trim()
            .trim_start_matches('\\');
        let mut replacements = Vec::new();
        let mut drivers = String::new();
        for instance in module.nodes.2.contents() {
            // Array connections have separate broadcast/distribution rules.
            if !instance.nodes.0.nodes.1.is_empty() {
                continue;
            }
            let Some(connections) = &instance.nodes.1.nodes.1 else {
                continue;
            };
            let (name_start, name_end) = timed_initializers::span(
                &tree,
                RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
            )
            .ok_or("missing instance name")?;
            let instance_name = &source[name_start..name_end];
            let connections: Vec<_> = match connections {
                ListOfPortConnections::Ordered(ordered) => ordered
                    .nodes
                    .0
                    .contents()
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, connection)| {
                        Some((
                            tasks.ports.get(module_name)?.get(index)?.clone(),
                            connection.nodes.1.as_ref()?,
                        ))
                    })
                    .collect(),
                ListOfPortConnections::Named(named) => named
                    .nodes
                    .0
                    .contents()
                    .into_iter()
                    .filter_map(|connection| {
                        let NamedPortConnection::Identifier(connection) = connection else {
                            return None;
                        };
                        Some((
                            tree.get_str(&connection.nodes.2)?.trim().to_owned(),
                            connection.nodes.3.as_ref()?.nodes.1.as_ref()?,
                        ))
                    })
                    .collect(),
            };
            for (port, expression) in connections {
                if !tasks
                    .inputs
                    .get(module_name)
                    .is_some_and(|inputs| inputs.contains(port.trim_start_matches('\\')))
                {
                    continue;
                }
                let (begin, finish) =
                    timed_initializers::span(&tree, RefNode::Expression(expression))
                        .ok_or("missing connection expression")?;
                let Some(expression) = timed_ff::gated_rhs(source, begin, finish, tasks) else {
                    continue;
                };
                let mut suffix = begin;
                let name = loop {
                    let name = format!("{}_connection_{suffix}", names.package);
                    if used_names.insert(name.clone()) {
                        break name;
                    }
                    suffix += 1;
                };
                drivers.push_str(&format!("reg [$bits({instance_name} .{port} )-1:0] {name};\nalways_comb begin if ({}::started) {name} = {expression}; end\n", names.package));
                replacements.push((begin, finish, name));
            }
        }
        if replacements.is_empty() {
            continue;
        }
        replacements.sort_by_key(|r| r.0);
        let mut body = source[start..end].to_owned();
        for (begin, finish, name) in replacements.into_iter().rev() {
            body.replace_range(begin - start..finish - start, &name);
        }
        let body = format!("{drivers}{body}");
        result.insert(
            start,
            (
                end,
                if timed_ff::single_generate_body(source, start) {
                    format!("begin\n{body}\nend")
                } else {
                    body
                },
            ),
        );
    }
    Ok(result)
}
