//! Host-side execution of IEEE 1800-2023 21.7.1 VCD tasks.
use crate::ir::SignalRef;
use crate::{Simulator, backend::SimBackend};
use celox_design::HostRequestSite;
use celox_runtime::Halt;
use num_traits::{ToPrimitive, Zero};

impl<B: SimBackend> Simulator<B> {
    pub(crate) fn run_vcd_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
        timestamp: u64,
    ) -> Result<(), Halt> {
        // VcdTask discriminants are stable within one compiled artifact.
        let result = self.vcd_request(site, timestamp);
        result.map_err(|message| {
            Halt::Error(crate::RuntimeErrorCode::Runtime {
                message,
                signals: Vec::new(),
            })
        })
    }

    fn vcd_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
        timestamp: u64,
    ) -> Result<(), String> {
        let mut args = Vec::new();
        for arg in &site.args {
            let (value, mask) = self.backend.get_four_state(arg.id);
            if !mask.is_zero() {
                return Err("VCD task argument contains x or z".into());
            }
            if matches!(site.operation, 1 | 6)
                && arg.signed
                && arg.width > 0
                && value.bit((arg.width - 1) as u64)
            {
                return Err("VCD depth and size arguments must be nonnegative".into());
            }
            args.push(value);
        }
        let io_error = |error: std::io::Error| format!("VCD task failed: {error}");
        match site.operation {
            0 => {
                if self.sv_vcd.started.is_some() {
                    return Err("$dumpfile must execute before $dumpvars".into());
                }
                let path = args
                    .first()
                    .map(|arg| {
                        let bytes = arg
                            .to_bytes_be()
                            .into_iter()
                            .skip_while(|byte| *byte == 0)
                            .collect::<Vec<_>>();
                        if bytes.contains(&0) {
                            return Err("$dumpfile filename contains a NUL byte".to_string());
                        }
                        String::from_utf8(bytes)
                            .map_err(|_| "$dumpfile filename is not UTF-8".to_string())
                    })
                    .transpose()?
                    .unwrap_or_else(|| "dump.vcd".into());
                self.sv_vcd.path = Some(path.into());
            }
            1 => {
                if self.sv_vcd.started.is_some_and(|time| time != timestamp) {
                    return Err(
                        "all $dumpvars calls must execute at the same simulation time".into(),
                    );
                }
                let levels = args
                    .first()
                    .map(|value| {
                        value
                            .to_usize()
                            .ok_or("$dumpvars levels exceeds host range".to_string())
                    })
                    .transpose()?
                    .unwrap_or(0);
                let owner = self
                    .program
                    .runtime_schema
                    .processes
                    .get(site.process)
                    .and_then(|slots| {
                        self.program
                            .design
                            .instances()
                            .find(|instance| instance.state_addresses().contains(&slots.resume))
                    })
                    .ok_or("$dumpvars process has no owning instance")?;
                let top_name = self
                    .program
                    .design
                    .instances()
                    .find(|instance| instance.path.0.is_empty())
                    .map(|instance| instance.module_name.clone())
                    .unwrap_or_else(|| "top".into());
                let top = DumpComponent::name(top_name.clone());
                let owner_components = instance_dump_components(owner, &top);
                let targets = if site.strings.is_empty() {
                    vec![DumpTarget {
                        package: false,
                        components: if args.is_empty() {
                            vec![top.clone()]
                        } else {
                            owner_components.clone()
                        },
                    }]
                } else {
                    site.strings
                        .iter()
                        .map(|name| decode_dump_target(name, &top, &owner_components))
                        .collect::<Result<Vec<_>, _>>()?
                };
                let mut descs = Vec::new();
                for instance in self.program.design.instances() {
                    let package = self.program.design.is_package_instance(instance.id);
                    let path = instance.display_path.join(".");
                    let scope = if package {
                        path.clone()
                    } else if path.is_empty() {
                        top_name.clone()
                    } else {
                        format!("{top_name}.{path}")
                    };
                    let scope_components = if package {
                        vec![DumpComponent::name(
                            path.strip_suffix("::").unwrap_or(&path).to_string(),
                        )]
                    } else {
                        instance_dump_components(instance, &top)
                    };
                    for address in instance.state_addresses() {
                        let Some(variable) = self.program.design.variable(address) else {
                            continue;
                        };
                        if !instance.resolves_path_to(&variable.path, variable.address) {
                            continue;
                        }
                        let name = variable.path.join(".");
                        // resolves_path_to above uses the frontend's module-affiliation
                        // metadata; hidden compiler slots are absent from that index.
                        let variable_components = variable
                            .path
                            .iter()
                            .flat_map(|part| generated_dump_components(part))
                            .collect::<Vec<_>>();
                        let selected = if package && args.is_empty() && site.strings.is_empty() {
                            true
                        } else {
                            dump_target_matches(
                                &scope_components,
                                &variable_components,
                                package,
                                &targets,
                                levels,
                            )
                        };
                        if !selected {
                            continue;
                        }
                        let signal = self.backend.resolve_signal(address);
                        if signal
                            .array_layout
                            .is_some_and(|array| array.element_stride * 8 != array.element_width)
                        {
                            return Err("$dumpvars requires packed array storage".into());
                        }
                        descs.push(crate::VcdSignalDesc {
                            scope: scope.clone(),
                            name,
                            offset: signal.offset,
                            width: signal.width,
                            is_4state: self.backend.layout().four_state && signal.is_4state,
                        });
                    }
                }
                let mut external = Vec::new();
                for (index, mut desc) in self.components.trace_descriptors().into_iter().enumerate()
                {
                    if desc.scope != top_name && !desc.scope.starts_with(&format!("{top_name}.")) {
                        desc.scope = format!("{top_name}.{}", desc.scope);
                    }
                    if dump_target_matches(
                        &generated_dump_components(&desc.scope),
                        &generated_dump_components(&desc.name),
                        false,
                        &targets,
                        levels,
                    ) {
                        external.push((index, desc));
                    }
                }
                if descs.is_empty() && external.is_empty() {
                    return Err("$dumpvars selection matches no dumpable variables".into());
                }
                for desc in descs {
                    if !self
                        .sv_vcd
                        .selections
                        .iter()
                        .any(|old| old.scope == desc.scope && old.name == desc.name)
                    {
                        self.sv_vcd.selections.push(desc);
                    }
                }
                for (index, desc) in external {
                    if !self
                        .sv_vcd
                        .external_selections
                        .iter()
                        .any(|(old, _)| *old == index)
                    {
                        self.sv_vcd.external_selections.push((index, desc));
                    }
                }
                self.sv_vcd.started = Some(timestamp);
                self.sv_vcd.pending = true;
            }
            2 => self.flush_vcd().map_err(io_error)?,
            3..=5 => {
                let command = match site.operation {
                    3 => "dumpon",
                    4 => "dumpoff",
                    _ => "dumpall",
                };
                self.sv_vcd.suspended =
                    site.operation == 4 || (site.operation == 5 && self.sv_vcd.suspended);
                let external = self.vcd_external_values();
                let (ptr, size) = self.backend.memory_as_ptr();
                // SAFETY: backend execution is suspended at this host boundary.
                let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
                if let Some(writer) = self.vcd_writer.as_mut() {
                    writer
                        .checkpoint(command, timestamp, memory, &external)
                        .map_err(io_error)?;
                }
            }
            6 => {
                self.sv_vcd.limit = args
                    .first()
                    .and_then(ToPrimitive::to_u64)
                    .ok_or("$dumplimit requires a nonnegative 64-bit size")?;
                if let Some(writer) = self.vcd_writer.as_mut() {
                    writer.set_byte_limit(self.sv_vcd.limit);
                }
            }
            _ => return Err("unknown VCD task operation".into()),
        }
        Ok(())
    }

    pub(super) fn source_vcd_external_values(
        &self,
    ) -> Vec<(num_bigint::BigUint, num_bigint::BigUint)> {
        let values = self.components.trace_values();
        self.sv_vcd
            .external_selections
            .iter()
            .map(|(index, _)| values[*index].clone())
            .collect()
    }

    pub(crate) fn vcd_external_values(&self) -> Vec<(num_bigint::BigUint, num_bigint::BigUint)> {
        if self.sv_vcd.source_writer {
            let values = self.components.trace_values();
            self.sv_vcd
                .active_external_indices
                .iter()
                .map(|index| values[*index].clone())
                .collect()
        } else {
            self.components.trace_values()
        }
    }

    pub(crate) fn start_sv_vcd(&mut self, timestamp: u64) -> std::io::Result<()> {
        if !self.sv_vcd.pending {
            return Ok(());
        }
        let path = self
            .sv_vcd
            .path
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("dump.vcd"));
        // Retire the buffered stream before File::create can truncate the
        // same path. Dropping the old handle later must not publish stale bytes.
        if let Some(old) = self.vcd_writer.as_mut() {
            old.flush()?;
        }
        self.vcd_writer = None;
        let mut writer = crate::VcdWriter::new_with_time_precision(
            path,
            &self.sv_vcd.selections,
            self.program.design.time_precision,
        )?;
        writer.set_byte_limit(self.sv_vcd.limit);
        let external_descs = self
            .sv_vcd
            .external_selections
            .iter()
            .map(|(_, desc)| desc.clone())
            .collect::<Vec<_>>();
        writer.add_external_signals(&external_descs)?;
        if self.sv_vcd.suspended {
            let (ptr, size) = self.backend.memory_as_ptr();
            let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
            writer.checkpoint(
                "dumpoff",
                timestamp,
                memory,
                &self.source_vcd_external_values(),
            )?;
        }
        self.vcd_writer = Some(writer);
        self.sv_vcd.source_writer = true;
        self.sv_vcd.active_external_indices = self
            .sv_vcd
            .external_selections
            .iter()
            .map(|(index, _)| *index)
            .collect();
        self.sv_vcd.pending = false;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DumpComponent {
    name: String,
    indices: Vec<i128>,
}

impl DumpComponent {
    fn name(name: String) -> Self {
        Self {
            name,
            indices: Vec::new(),
        }
    }
}

struct DumpTarget {
    package: bool,
    components: Vec<DumpComponent>,
}

// Generated hierarchy names may combine scopes (g[0].u). Escaped authored
// names remain one component, including any punctuation in their spelling.
fn generated_dump_components(text: &str) -> Vec<DumpComponent> {
    let mut components = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.starts_with('\\') {
            components.push(DumpComponent::name(rest.to_string()));
            break;
        }
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        if end == 0 {
            return vec![DumpComponent::name(text.to_string())];
        }
        let mut component = DumpComponent::name(rest[..end].to_string());
        rest = &rest[end..];
        while let Some(indexed) = rest.strip_prefix('[') {
            let Some(end) = indexed.find(']') else {
                return vec![DumpComponent::name(text.to_string())];
            };
            let Ok(index) = indexed[..end].parse::<i128>() else {
                return vec![DumpComponent::name(text.to_string())];
            };
            component.indices.push(index);
            rest = &indexed[end + 1..];
        }
        components.push(component);
        if let Some(next) = rest.strip_prefix('.') {
            rest = next;
        } else if !rest.is_empty() {
            return vec![DumpComponent::name(text.to_string())];
        }
    }
    components
}

fn instance_dump_components(
    instance: &crate::ir::RuntimeInstance,
    top: &DumpComponent,
) -> Vec<DumpComponent> {
    let mut components = vec![top.clone()];
    for ((name, _), display) in instance.path.0.iter().zip(&instance.display_path) {
        let mut part = generated_dump_components(name);
        // The authored name and its indexed display name are separate in
        // metadata, so an escaped literal bracket cannot become an index.
        if let Some(suffix) = display
            .strip_prefix(name)
            .filter(|suffix| !suffix.is_empty())
        {
            let indexed = generated_dump_components(&format!("instance{suffix}"));
            if let (Some(last), Some(indices)) = (part.last_mut(), indexed.first()) {
                last.indices.extend_from_slice(&indices.indices);
            }
        }
        components.extend(part);
    }
    components
}

fn decode_dump_target(
    text: &str,
    top: &DumpComponent,
    owner: &[DumpComponent],
) -> Result<DumpTarget, String> {
    let mut fields = text.split('\0');
    let (kind, mut components) = if fields.next() == Some("celox.dump.path.v1") {
        let kind = fields.next().ok_or("invalid $dumpvars path metadata")?;
        let mut components: Vec<DumpComponent> = Vec::new();
        while let Some(tag) = fields.next() {
            let value = fields.next().ok_or("invalid $dumpvars path metadata")?;
            match tag {
                "name" => components.push(DumpComponent::name(value.to_string())),
                "index" => components
                    .last_mut()
                    .ok_or("invalid $dumpvars index metadata")?
                    .indices
                    .push(
                        value
                            .parse()
                            .map_err(|_| "invalid $dumpvars index metadata")?,
                    ),
                _ => return Err("invalid $dumpvars path metadata".into()),
            }
        }
        (kind, components)
    } else {
        // Existing plain site metadata remains readable by this runtime.
        if let Some(absolute) = text.strip_prefix("$root.") {
            ("absolute", generated_dump_components(absolute))
        } else if let Some((package, variable)) = text.split_once("::") {
            let mut components = vec![DumpComponent::name(package.to_string())];
            components.extend(generated_dump_components(variable));
            ("package", components)
        } else {
            ("relative", generated_dump_components(text))
        }
    };
    if !matches!(kind, "relative" | "local" | "absolute" | "package") {
        return Err("invalid $dumpvars path kind metadata".into());
    }
    if components.is_empty() {
        return Err("$dumpvars selection is empty".into());
    }
    if kind == "local" || (kind == "relative" && components.first() != Some(top)) {
        let mut qualified = owner.to_vec();
        qualified.extend(components);
        components = qualified;
    }
    Ok(DumpTarget {
        package: kind == "package",
        components,
    })
}

fn dump_target_matches(
    scope: &[DumpComponent],
    variable: &[DumpComponent],
    package: bool,
    targets: &[DumpTarget],
    levels: usize,
) -> bool {
    let full = scope.iter().chain(variable).cloned().collect::<Vec<_>>();
    targets.iter().any(|target| {
        if target.package != package {
            return false;
        }
        if full == target.components {
            return true;
        }
        if package {
            return false;
        }
        if scope.starts_with(&target.components) {
            return levels == 0 || scope.len() - target.components.len() < levels;
        }
        // A named generate scope may own variables without a child module.
        let variable_scope = &full[..full.len().saturating_sub(1)];
        target.components.len() > scope.len()
            && variable_scope.starts_with(&target.components)
            && (levels == 0 || variable_scope.len() - target.components.len() < levels)
    })
}

#[cfg(all(test, feature = "systemverilog"))]
mod tests {
    #[test]
    fn relaxed_limit_rescans_consumed_backend_writes() {
        for new_limit in [0, 1_000_000] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("resume.vcd");
            let source = "module Top(output logic [3:0] value); initial value=0; endmodule";
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("vcd.sv"))],
                "Top",
            )
            .vcd(&path)
            .build()
            .unwrap();
            simulation.run_until(0).unwrap();
            simulation
                .simulator
                .vcd_writer
                .as_mut()
                .unwrap()
                .set_byte_limit(1);
            let value = simulation.signal("value");
            simulation.modify(|io| io.set(value, 3u8)).unwrap();
            simulation.run_until(1).unwrap();
            // Limit changes made by the host do not themselves dirty a group,
            // so this exercises the empty activity list on resumption.
            simulation
                .simulator
                .vcd_writer
                .as_mut()
                .unwrap()
                .set_byte_limit(new_limit);
            simulation.run_until(2).unwrap();
            simulation.flush_vcd().unwrap();
            let text = std::fs::read_to_string(path).unwrap();
            assert!(text.contains("#2\nb11 "), "limit {new_limit}: {text}");
        }
    }
}
