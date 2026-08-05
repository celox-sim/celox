use std::collections::HashMap;

use celox_testbench::{
    CompiledExpr, ComponentLibrary, ComponentParameterValue, ExecutableComponentBinding,
    TestbenchComponent, TestbenchTarget, TestbenchValue,
};
use veryl_metadata::component_manifest::{ConnectionFacts, ConnectionTarget, ConnectionViolation};

mod host;
mod injected;
mod loader;
#[cfg(not(target_family = "wasm"))]
mod wasm;

use host::{ExternalInstance, HostContext, HostValue, PortDir, PortRole};
use loader::lookup_component_backend;

pub use injected::{
    InjectedCall, InjectedComponentHandler, InjectedComponents, InjectedHook, InjectedNamedValue,
    InjectedPort, InjectedResult, InjectedValue,
};

use crate::{EventHandle, SignalRef, SimBackend};

/// Registers an in-process Veryl component implementation.
///
/// Dynamic and Wasm component libraries are discovered from their configured
/// paths; this entry point is intended for builtin components and tests.
pub fn register_static_component(
    name: &str,
    vtable: &'static veryl_component_sys::VrlComponentVTable,
) {
    loader::register_static_component(name, vtable);
}

/// Registers the manifest paired with an in-process component implementation.
pub fn register_static_component_manifest(name: &str, json: &str) {
    loader::register_static_manifest(name, json);
}

struct LiveInput {
    port: u32,
    expr: CompiledExpr,
    mask_source: Option<TestbenchTarget<SignalRef, CompiledExpr>>,
    width: usize,
}

struct LiveOutput {
    port: u32,
    name: String,
    target: TestbenchTarget<SignalRef, CompiledExpr>,
    rtl_driven: bool,
}

struct LiveEvent {
    event_id: usize,
    port: u32,
    reset: bool,
}

struct LiveComponent {
    name: String,
    instance: ExternalInstance,
    host: HostContext,
    inputs: Vec<LiveInput>,
    outputs: Vec<LiveOutput>,
    events: Vec<LiveEvent>,
    fire_count: u64,
}

#[derive(Default)]
pub(crate) struct ComponentRuntime {
    components: Vec<LiveComponent>,
    last_trace_values: Vec<(num_bigint::BigUint, num_bigint::BigUint)>,
    active_reset_event: Option<usize>,
    injected: InjectedComponents,
}

impl ComponentRuntime {
    pub(crate) fn set_injected(&mut self, injected: InjectedComponents) {
        self.injected = injected;
    }
}

pub(crate) struct ComponentWrite {
    pub(crate) target: TestbenchTarget<SignalRef, CompiledExpr>,
    pub(crate) value: num_bigint::BigUint,
    pub(crate) mask_xz: num_bigint::BigUint,
}

fn width_mask(width: usize) -> num_bigint::BigUint {
    if width == 0 {
        num_bigint::BigUint::default()
    } else {
        (num_bigint::BigUint::from(1u8) << width) - 1u8
    }
}

impl ComponentWrite {
    pub(crate) fn apply<B: SimBackend>(self, backend: &mut B) {
        let Some(selection) = &self.target.selection else {
            backend.set_four_state(self.target.signal, self.value, self.mask_xz);
            return;
        };
        let (values, _) = backend.memory_as_mut_ptr();
        let offset = selection.offset.eval_u64(values) as usize;
        let width = selection
            .width
            .min(self.target.signal.width.saturating_sub(offset));
        if width == 0 {
            return;
        }
        let (root, root_mask) = backend.get_four_state(self.target.signal);
        let selected_mask = width_mask(width) << offset;
        let clear_mask = width_mask(self.target.signal.width) ^ &selected_mask;
        let value = (self.value & width_mask(width)) << offset;
        let mask_xz = (self.mask_xz & width_mask(width)) << offset;
        backend.set_four_state(
            self.target.signal,
            (root & &clear_mask) | value,
            (root_mask & &clear_mask) | mask_xz,
        );
    }
}

fn instance_seed(base: u64, test_name: &str, instance: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in base
        .to_le_bytes()
        .iter()
        .copied()
        .chain(test_name.bytes())
        .chain(instance.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[derive(Clone, Copy)]
struct ComponentOutputRange {
    start: usize,
    end: usize,
}

impl ComponentOutputRange {
    fn from_target(target: &TestbenchTarget<SignalRef, CompiledExpr>) -> Option<Self> {
        let (start, width) = match &target.selection {
            Some(selection) => (
                usize::try_from(selection.offset.constant_u64()?).ok()?,
                selection.width,
            ),
            None => (0, target.signal.width),
        };
        let end = start.saturating_add(width).min(target.signal.width);
        Some(Self { start, end })
    }

    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

struct ComponentOutputDriver {
    instance: String,
    range: Option<ComponentOutputRange>,
}

fn parameter_host_value(value: &ComponentParameterValue) -> HostValue {
    match value {
        ComponentParameterValue::Bits { words, width } => HostValue::Bits {
            words: words.clone(),
            width: *width,
        },
        ComponentParameterValue::String(value) => HostValue::Str(value.clone()),
    }
}

fn component_manifest(
    library: Option<&ComponentLibrary>,
    type_name: &str,
) -> Result<Option<veryl_metadata::ComponentManifest>, String> {
    if let Some(library) = library {
        let Some(json) = loader::library_manifest(&library.path) else {
            return Ok(None);
        };
        return loader::parse_library_manifest_json(&json, type_name);
    }
    loader::static_manifest(type_name)
        .map(|json| {
            veryl_metadata::ComponentManifest::parse(&json)
                .ok_or_else(|| format!("manifest of `{type_name}` cannot be parsed"))
        })
        .transpose()
}

fn validate_manifest(
    descriptor: &TestbenchComponent,
    type_name: &str,
    manifest: &veryl_metadata::ComponentManifest,
) -> Result<(), String> {
    let has_ports = !(manifest.ports.is_empty() && manifest.groups.is_empty());
    let mut bound = std::collections::HashSet::new();
    for connection in &descriptor.connections {
        if !has_ports {
            break;
        }
        let Some(target) = manifest.connection_target(
            &connection.port,
            connection.group.as_deref(),
            connection.member.as_deref(),
        ) else {
            if connection.group.is_none() {
                return Err(format!(
                    "component `{}`: `{type_name}` declares no port named `{}`",
                    descriptor.instance, connection.port
                ));
            }
            continue;
        };
        let display = match &target {
            ConnectionTarget::Loose(port) => port.name.clone(),
            ConnectionTarget::Member(_, member) => connection
                .group
                .as_deref()
                .map(|group| format!("{group}.{}", member.member))
                .unwrap_or_else(|| connection.port.clone()),
        };
        if !bound.insert(display.clone()) {
            return Err(format!(
                "component `{}`: port `{display}` is connected more than once",
                descriptor.instance
            ));
        }
        let facts = ConnectionFacts {
            input: connection.input,
            drivable: connection.has_output,
            is_clock: connection.is_clock,
            is_reset: connection.is_reset,
        };
        if let Some(violation) = target.check(&facts).into_iter().next() {
            return Err(format!(
                "component `{}`: port `{display}` violates its manifest: {}",
                descriptor.instance,
                match violation {
                    ConnectionViolation::InvalidDirection(x) => format!("invalid direction `{x}`"),
                    ConnectionViolation::NotInput => "connection is not readable".into(),
                    ConnectionViolation::NotDrivable => "connection is not drivable".into(),
                    ConnectionViolation::NotClock | ConnectionViolation::ClockUndeclared => {
                        "connection is not a clock".into()
                    }
                    ConnectionViolation::NotReset | ConnectionViolation::ResetUndeclared => {
                        "connection is not a reset".into()
                    }
                }
            ));
        }
    }
    for (name, _) in &descriptor.params {
        if manifest.param(name).is_none() {
            return Err(format!(
                "component `{}`: `{type_name}` declares no parameter named `{name}`",
                descriptor.instance
            ));
        }
    }
    Ok(())
}

fn drain_logs(component: &mut LiveComponent) {
    use std::io::Write as _;

    let mut stderr = std::io::stderr().lock();
    for message in component.host.take_logs() {
        let _ = writeln!(stderr, "{message}");
    }
    let _ = stderr.flush();
}

fn words_to_biguint(words: &[u64]) -> num_bigint::BigUint {
    let bytes: Vec<_> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    num_bigint::BigUint::from_bytes_le(&bytes)
}

fn take_output_writes(component: &mut LiveComponent) -> Vec<ComponentWrite> {
    let mut writes = Vec::new();
    for output in &component.outputs {
        if component.host.output_dirty_idx(output.port) {
            writes.push(ComponentWrite {
                target: output.target.clone(),
                value: words_to_biguint(component.host.output_words(&output.name)),
                mask_xz: words_to_biguint(component.host.output_mask_xz(&output.name)),
            });
        }
    }
    component.host.clear_output_dirty();
    writes
}

fn stage_component_inputs<B: SimBackend>(component: &mut LiveComponent, backend: &mut B) {
    let (values, _) = backend.memory_as_mut_ptr();
    for input in &component.inputs {
        let value = input.expr.eval_value(values).to_biguint();
        let mask_xz = input
            .mask_source
            .as_ref()
            .map(|source| {
                let (_, mut mask) = backend.get_four_state(source.signal);
                if let Some(selection) = &source.selection {
                    let offset = selection.offset.eval_u64(values) as usize;
                    mask >>= offset;
                    mask &= width_mask(selection.width);
                }
                mask
            })
            .unwrap_or_default();
        let mut words: Vec<_> = value.iter_u64_digits().collect();
        let mut mask_words: Vec<_> = mask_xz.iter_u64_digits().collect();
        words.resize(input.width.div_ceil(64).max(1), 0);
        mask_words.resize(input.width.div_ceil(64).max(1), 0);
        component
            .host
            .set_input_masked(input.port, &words, &mask_words);
    }
}

impl ComponentRuntime {
    pub(crate) fn initialize<B: SimBackend>(
        &mut self,
        descriptors: &[TestbenchComponent],
        bindings: &[ExecutableComponentBinding<impl EventHandle, SignalRef>],
        libraries: &[ComponentLibrary],
        file_base: Option<&std::path::Path>,
        seed_base: u64,
        test_name: &str,
        use_4state: bool,
        simulator_backend: &mut B,
    ) -> Result<Vec<ComponentWrite>, String> {
        self.components.clear();
        self.last_trace_values.clear();
        self.active_reset_event = None;
        let mut initialized = Vec::with_capacity(descriptors.len());
        let mut initial_writes = Vec::new();
        let mut driven_outputs = HashMap::<SignalRef, Vec<ComponentOutputDriver>>::new();
        let libraries: HashMap<_, _> = libraries
            .iter()
            .map(|library| (library.export.as_str(), library))
            .collect();

        for descriptor in descriptors {
            let binding = bindings
                .iter()
                .find(|binding| binding.instance == descriptor.instance)
                .ok_or_else(|| {
                    format!(
                        "component `{}` has no runtime connection binding",
                        descriptor.instance
                    )
                })?;
            if binding.connections.len() != descriptor.connections.len() {
                return Err(format!(
                    "component `{}` connection metadata does not match its runtime bindings",
                    descriptor.instance
                ));
            }
            let injected = self.injected.get(&descriptor.component).cloned();
            let library = libraries.get(descriptor.component.as_str()).copied();
            let type_name = library
                .map(|library| library.type_name.as_str())
                .unwrap_or(descriptor.component.as_str());
            let component_backend = if injected.is_none() {
                Some(
                    lookup_component_backend(
                        library.map(|library| library.path.as_path()),
                        type_name,
                    )
                    .map_err(|error| format!("component `{}`: {error}", descriptor.instance))?,
                )
            } else {
                None
            };
            let manifest = match &injected {
                Some(definition) => Some(definition.manifest.clone()),
                None => component_manifest(library, type_name)?,
            };
            if let Some(manifest) = &manifest {
                validate_manifest(descriptor, type_name, manifest)?;
            }
            let kind = injected
                .as_ref()
                .map(|definition| definition.kind)
                .unwrap_or_else(|| component_backend.as_ref().unwrap().kind());
            if descriptor.is_var_form && kind == veryl_component_sys::VRL_KIND_CLOCKED {
                return Err(format!(
                    "component `{}`: clocked component `{type_name}` must use inst form",
                    descriptor.instance
                ));
            }
            if !descriptor.is_var_form && kind == veryl_component_sys::VRL_KIND_METHOD_ONLY {
                return Err(format!(
                    "component `{}`: method-only component `{type_name}` must use var form",
                    descriptor.instance
                ));
            }

            let mut host = HostContext::new();
            host.label = descriptor.instance.clone();
            host.use_4state = use_4state;
            host.seed = instance_seed(seed_base, test_name, &descriptor.instance);
            if let Some(base) = file_base {
                host.read_base = Some(base.to_path_buf());
                host.write_base = Some(
                    base.join("target")
                        .join("veryl-components")
                        .join("out")
                        .join(test_name),
                );
            }
            for (name, value) in &descriptor.params {
                host.add_param(name, parameter_host_value(value));
            }

            let mut inputs = Vec::new();
            let mut outputs = Vec::new();
            let mut events = Vec::new();
            let mut offered_ports = Vec::new();
            for (connection, binding) in descriptor.connections.iter().zip(&binding.connections) {
                if connection.port != binding.port {
                    return Err(format!(
                        "component `{}` connection `{}` is bound as `{}`",
                        descriptor.instance, connection.port, binding.port
                    ));
                }
                if connection.width == 0 {
                    return Err(format!(
                        "component `{}` port `{}` has undetermined width",
                        descriptor.instance, connection.port
                    ));
                }
                let input_port = if connection.input {
                    let role = if connection.is_clock {
                        PortRole::Clock
                    } else if connection.is_reset {
                        PortRole::Reset
                    } else {
                        PortRole::Data
                    };
                    let port = host.add_port_role(
                        &connection.port,
                        PortDir::Input,
                        role,
                        connection.width,
                    );
                    let expr = binding.input.as_ref().ok_or_else(|| {
                        format!(
                            "component `{}` input port `{}` has no source expression",
                            descriptor.instance, connection.port
                        )
                    })?;
                    inputs.push(LiveInput {
                        port,
                        expr: expr.clone(),
                        mask_source: binding.input_target.clone(),
                        width: connection.width as usize,
                    });
                    if connection.is_clock || connection.is_reset {
                        match binding.event {
                            Some(event) => events.push(LiveEvent {
                                event_id: event.id(),
                                port,
                                reset: connection.is_reset,
                            }),
                            None if connection.is_clock => {
                                return Err(format!(
                                    "component `{}` clock port `{}` has no runtime event source; connect it to a named clock signal",
                                    descriptor.instance, connection.port
                                ));
                            }
                            None => {
                                // Synchronous resets are ordinary staged inputs. They
                                // intentionally have no standalone runtime event and are
                                // observed by the component on its clock hook.
                            }
                        }
                    }
                    Some(port)
                } else {
                    None
                };
                let output_port = if connection.has_output {
                    let target = binding.output.clone().ok_or_else(|| {
                        format!(
                            "component `{}` output port `{}` has no destination",
                            descriptor.instance, connection.port
                        )
                    })?;
                    let port = host.add_port(&connection.port, PortDir::Output, connection.width);
                    outputs.push(LiveOutput {
                        port,
                        name: connection.port.clone(),
                        target,
                        rtl_driven: binding.output_rtl_driven,
                    });
                    Some(port)
                } else {
                    None
                };
                offered_ports.push((
                    connection.port.clone(),
                    connection.group.clone(),
                    input_port,
                    output_port,
                    connection.is_clock,
                    connection.is_reset,
                ));
            }

            if injected.is_some() {
                for (port_name, _, input, output, is_clock, is_reset) in &offered_ports {
                    if let Some(port) = input {
                        let direction = if *is_clock {
                            veryl_component_sys::VRL_DIR_CLOCK
                        } else if *is_reset {
                            veryl_component_sys::VRL_DIR_RESET
                        } else {
                            veryl_component_sys::VRL_DIR_INPUT
                        };
                        debug_assert_eq!(host.svc_port_index(port_name, direction), *port as i32);
                    }
                    if let Some(port) = output {
                        debug_assert_eq!(
                            host.svc_port_index(port_name, veryl_component_sys::VRL_DIR_OUTPUT),
                            *port as i32
                        );
                    }
                }
            }
            let instance = match injected {
                Some(definition) => ExternalInstance::create_injected(definition, &mut host),
                None => ExternalInstance::create(component_backend.unwrap(), &mut host),
            }
            .map_err(|error| format!("component `{}`: {error}", descriptor.instance))?;
            for (port_name, group, input, output, is_clock, is_reset) in &offered_ports {
                if group.is_none()
                    && !input.is_some_and(|port| host.port_touched(port))
                    && !output.is_some_and(|port| host.port_touched(port))
                {
                    return Err(format!(
                        "component `{}` did not resolve connected port `{port_name}`",
                        descriptor.instance
                    ));
                }
                if group.is_none()
                    && (*is_clock || *is_reset)
                    && let Some(port) = input
                {
                    let expected = if *is_clock {
                        PortRole::Clock
                    } else {
                        PortRole::Reset
                    };
                    if host.port_resolved_role(*port) != Some(expected) {
                        return Err(format!(
                            "component `{}` did not resolve `{port_name}` as a {} port",
                            descriptor.instance,
                            if *is_clock { "clock" } else { "reset" }
                        ));
                    }
                }
            }
            for group in offered_ports
                .iter()
                .filter_map(|(_, group, _, _, _, _)| group.as_deref())
            {
                let touched = offered_ports
                    .iter()
                    .any(|(_, candidate, input, output, _, _)| {
                        candidate.as_deref() == Some(group)
                            && (input.is_some_and(|port| host.port_touched(port))
                                || output.is_some_and(|port| host.port_touched(port)))
                    });
                if !touched {
                    return Err(format!(
                        "component `{}` did not resolve any member of connected interface `{group}`",
                        descriptor.instance
                    ));
                }
            }
            inputs.retain(|input| host.port_touched(input.port));
            outputs.retain(|output| host.port_touched(output.port));
            events.retain(|event| {
                host.port_resolved_role(event.port)
                    == Some(if event.reset {
                        PortRole::Reset
                    } else {
                        PortRole::Clock
                    })
            });
            for output in &outputs {
                if output.rtl_driven {
                    return Err(format!(
                        "component `{}` output `{}` conflicts with an RTL driver",
                        descriptor.instance, output.name
                    ));
                }
                let range = ComponentOutputRange::from_target(&output.target);
                let drivers = driven_outputs.entry(output.target.signal).or_default();
                if let Some(other) = drivers.iter().find(|other| {
                    range
                        .zip(other.range)
                        .is_none_or(|(range, other)| range.overlaps(other))
                }) {
                    return Err(format!(
                        "component `{}` output `{}` conflicts with component `{}`",
                        descriptor.instance, output.name, other.instance
                    ));
                }
                drivers.push(ComponentOutputDriver {
                    instance: descriptor.instance.clone(),
                    range,
                });
            }
            if kind == veryl_component_sys::VRL_KIND_CLOCKED
                && !events.iter().any(|event| !event.reset)
            {
                return Err(format!(
                    "component `{}`: clocked component `{type_name}` resolved no clock port",
                    descriptor.instance
                ));
            }
            let mut component = LiveComponent {
                name: descriptor.instance.clone(),
                instance,
                host,
                inputs,
                outputs,
                events,
                fire_count: 0,
            };
            stage_component_inputs(&mut component, simulator_backend);
            let failures_before = component.host.failures().len();
            let rc = component.instance.on_init(&mut component.host);
            drain_logs(&mut component);
            let mut failures = component.host.take_failures();
            if rc != 0 && failures_before == 0 && failures.is_empty() {
                failures.push(format!(
                    "component `{}` hook `on_init` failed",
                    component.name
                ));
            }
            if !failures.is_empty() {
                return Err(failures.join("\n"));
            }
            initial_writes.extend(take_output_writes(&mut component));
            initialized.push(component);
        }
        self.components = initialized;
        Ok(initial_writes)
    }

    pub(crate) fn trace_descriptors(&self) -> Vec<celox_runtime::VcdExternalSignalDesc> {
        self.components
            .iter()
            .flat_map(|component| {
                component
                    .host
                    .trace_vars
                    .iter()
                    .map(|trace| celox_runtime::VcdExternalSignalDesc {
                        scope: component.name.clone(),
                        name: trace.name.clone(),
                        width: trace.width as usize,
                    })
            })
            .collect()
    }

    pub(crate) fn trace_values(&self) -> Vec<(num_bigint::BigUint, num_bigint::BigUint)> {
        if self.components.is_empty() {
            return self.last_trace_values.clone();
        }
        self.components
            .iter()
            .flat_map(|component| {
                component.host.trace_vars.iter().map(|trace| {
                    (
                        words_to_biguint(&trace.words),
                        num_bigint::BigUint::default(),
                    )
                })
            })
            .collect()
    }

    pub(crate) fn has_scheduled_components(&self) -> bool {
        self.components
            .iter()
            .any(|component| !component.events.is_empty())
    }

    pub(crate) fn stage_inputs<B: SimBackend>(&mut self, event_id: usize, backend: &mut B) {
        for component in &mut self.components {
            if !component
                .events
                .iter()
                .any(|event| event.event_id == event_id)
            {
                continue;
            }
            stage_component_inputs(component, backend);
        }
    }

    pub(crate) fn fire(
        &mut self,
        event_id: usize,
        time: u64,
    ) -> Result<Vec<ComponentWrite>, String> {
        let active_reset_event = self.active_reset_event;
        self.fire_matching(time, |component| {
            let triggered = component
                .events
                .iter()
                .find(|event| event.event_id == event_id)?;
            let reset = if triggered.reset {
                None
            } else {
                active_reset_event.and_then(|reset_event_id| {
                    component
                        .events
                        .iter()
                        .find(|event| event.reset && event.event_id == reset_event_id)
                })
            };
            let event = reset.unwrap_or(triggered);
            Some((event.port, event.reset))
        })
    }

    pub(crate) fn begin_reset_cycles(&mut self, reset_event_id: Option<usize>) {
        self.active_reset_event = reset_event_id;
    }

    pub(crate) fn end_reset_cycles(&mut self) {
        self.active_reset_event = None;
    }

    fn fire_matching(
        &mut self,
        time: u64,
        select: impl Fn(&LiveComponent) -> Option<(u32, bool)>,
    ) -> Result<Vec<ComponentWrite>, String> {
        let mut writes = Vec::new();
        let mut failures = Vec::new();
        for component in &mut self.components {
            let Some((port, reset)) = select(component) else {
                continue;
            };
            component.host.time = time;
            component.host.fired_clock = port;
            let hook = if reset {
                "on_reset"
            } else {
                component.fire_count = component.fire_count.saturating_add(1);
                component.host.cycle = component.fire_count;
                "on_clock"
            };
            let failures_before = component.host.failures().len();
            let rc = if reset {
                component.instance.on_reset(&mut component.host)
            } else {
                component.instance.on_clock(&mut component.host)
            };
            if rc != 0 && component.host.failures().len() == failures_before {
                failures.push(format!(
                    "component `{}` hook `{hook}` failed",
                    component.name
                ));
            }
            drain_logs(component);
            failures.extend(component.host.take_failures());
            writes.extend(take_output_writes(component));
        }
        if failures.is_empty() {
            Ok(writes)
        } else {
            Err(failures.join("\n"))
        }
    }

    pub(crate) fn call_method<B: SimBackend>(
        &mut self,
        instance: &str,
        method: &str,
        args: &[HostValue],
        time: u64,
        backend: &mut B,
    ) -> Result<(HostValue, Vec<ComponentWrite>), String> {
        let Some(component) = self
            .components
            .iter_mut()
            .find(|component| component.name == instance)
        else {
            return Err(format!("unknown component instance `{instance}`"));
        };
        stage_component_inputs(component, backend);
        component.host.time = time;
        let result = component
            .instance
            .call_method(&mut component.host, method, args);
        drain_logs(component);
        let failures = component.host.take_failures();
        let writes = take_output_writes(component);
        match result {
            Some(value) if failures.is_empty() => Ok((value, writes)),
            None if failures.is_empty() => {
                Err(format!("component method `{instance}.{method}` failed"))
            }
            _ => Err(failures.join("\n")),
        }
    }

    pub(crate) fn finish_requested(&self) -> bool {
        self.components
            .iter()
            .any(|component| component.host.finish_requested())
    }

    pub(crate) fn finish(&mut self, time: u64) -> Result<(), String> {
        let mut failures = Vec::new();
        for component in &mut self.components {
            component.host.time = time;
            let failures_before = component.host.failures().len();
            let rc = component.instance.on_finish(&mut component.host);
            drain_logs(component);
            let mut component_failures = component.host.take_failures();
            if rc != 0 && failures_before == 0 && component_failures.is_empty() {
                component_failures.push(format!(
                    "component `{}` hook `on_finish` failed",
                    component.name
                ));
            }
            failures.extend(component_failures);
        }
        self.last_trace_values = self.trace_values();
        self.components.clear();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n"))
        }
    }
}

pub(crate) fn host_value_from_argument(
    value: TestbenchValue,
    width: usize,
    is_string: bool,
) -> HostValue {
    let value = value.to_biguint();
    if is_string {
        let byte_len = width.div_ceil(8);
        let mut bytes = value.to_bytes_be();
        if bytes.len() < byte_len {
            let mut padded = vec![0; byte_len - bytes.len()];
            padded.extend(bytes);
            bytes = padded;
        }
        HostValue::Str(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        let mut words: Vec<_> = value.iter_u64_digits().collect();
        words.resize(width.div_ceil(64).max(1), 0);
        HostValue::Bits {
            words,
            width: width as u32,
        }
    }
}

pub(crate) fn host_bits(value: &HostValue) -> Option<(num_bigint::BigUint, u32)> {
    let HostValue::Bits { words, width } = value else {
        return None;
    };
    let bytes: Vec<_> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    Some((num_bigint::BigUint::from_bytes_le(&bytes), *width))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_without_parameters_rejects_supplied_parameter() {
        let manifest = veryl_metadata::ComponentManifest::parse(r#"{"kind":"method_only"}"#)
            .expect("valid component manifest");
        let descriptor = TestbenchComponent {
            instance: "component".into(),
            component: "paramless".into(),
            params: vec![(
                "UNKNOWN".into(),
                ComponentParameterValue::Bits {
                    words: vec![1],
                    width: 1,
                },
            )],
            connections: Vec::new(),
            is_var_form: true,
            source: None,
        };

        let error = validate_manifest(&descriptor, "paramless", &manifest).unwrap_err();
        assert!(
            error.contains("declares no parameter named `UNKNOWN`"),
            "{error}"
        );
    }
}
