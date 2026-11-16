//! Running native testbenches, with components injected from JavaScript.

use super::*;

// ---------------------------------------------------------------------------
//  Native testbench execution
// ---------------------------------------------------------------------------

/// Result of a single `$assert` evaluation in a native testbench.
#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiAssertionResult {
    pub passed: bool,
    pub message: Option<Uint8Array>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// Detailed result of running a native testbench.
#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiTestResult {
    pub passed: bool,
    pub assertions: Vec<NapiAssertionResult>,
    pub error: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn convert_test_result(r: celox::TestResultDetailed) -> NapiTestResult {
    NapiTestResult {
        passed: r.passed,
        error: r.error,
        assertions: r
            .assertions
            .into_iter()
            .map(|a| {
                let (file, line, column) = match a.location {
                    Some(loc) => (Some(loc.file), Some(loc.line), Some(loc.column)),
                    None => (None, None, None),
                };
                NapiAssertionResult {
                    passed: a.passed,
                    message: a.message.map(Uint8Array::from),
                    file,
                    line,
                    column,
                }
            })
            .collect(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiInjectedValue {
    pub name: Option<String>,
    pub bits: Option<BigInt>,
    pub mask_xz: Option<BigInt>,
    pub width: Option<u32>,
    pub string_value: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiInjectedCall {
    pub instance: String,
    pub phase: String,
    pub method: Option<String>,
    pub inputs: Vec<NapiInjectedValue>,
    pub params: Vec<NapiInjectedValue>,
    pub ports: Vec<NapiInjectedPort>,
    pub args: Vec<NapiInjectedValue>,
    pub cycle: BigInt,
    pub time: BigInt,
    pub seed: BigInt,
    pub fired_clock: Option<String>,
    pub four_state: bool,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiInjectedPort {
    pub name: String,
    pub direction: String,
    pub role: Option<String>,
    pub width: u32,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiInjectedResult {
    pub outputs: Option<Vec<NapiInjectedValue>>,
    pub return_value: Option<NapiInjectedValue>,
    pub failures: Option<Vec<String>>,
    pub logs: Option<Vec<String>>,
    pub finish: Option<bool>,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object, object_to_js = false)]
pub struct NapiInjectedComponent {
    pub name: String,
    pub manifest: String,
    pub handler: FunctionRef<NapiInjectedCall, NapiInjectedResult>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct NapiInjectedHandler {
    pub(super) env: Env,
    pub(super) handler: FunctionRef<NapiInjectedCall, NapiInjectedResult>,
}

// Injected callbacks are only accepted by the synchronous runTest APIs and
// are invoked on the JS thread which supplied this Env. The core trait is
// Send + Sync because compiled component hooks may otherwise be movable.
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Send for NapiInjectedHandler {}

#[cfg(not(target_arch = "wasm32"))]
unsafe impl Sync for NapiInjectedHandler {}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn to_napi_injected_value(
    name: Option<String>,
    value: celox::InjectedValue,
) -> NapiInjectedValue {
    match value {
        celox::InjectedValue::Bits {
            words,
            mask_xz,
            width,
        } => NapiInjectedValue {
            name,
            bits: Some(BigInt {
                sign_bit: false,
                words,
            }),
            mask_xz: Some(BigInt {
                sign_bit: false,
                words: mask_xz,
            }),
            width: Some(width),
            string_value: None,
        },
        celox::InjectedValue::String(value) => NapiInjectedValue {
            name,
            bits: None,
            mask_xz: None,
            width: None,
            string_value: Some(value),
        },
        celox::InjectedValue::Unit => NapiInjectedValue {
            name,
            bits: None,
            mask_xz: None,
            width: None,
            string_value: None,
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn from_napi_injected_value(
    value: NapiInjectedValue,
) -> std::result::Result<celox::InjectedValue, String> {
    if let Some(bits) = value.bits {
        if bits.sign_bit || value.mask_xz.as_ref().is_some_and(|mask| mask.sign_bit) {
            return Err("component callback values cannot be negative".into());
        }
        let width = value
            .width
            .ok_or_else(|| "component callback bit value has no width".to_string())?;
        return Ok(celox::InjectedValue::Bits {
            words: bits.words,
            mask_xz: value.mask_xz.map(|mask| mask.words).unwrap_or_default(),
            width,
        });
    }
    Ok(match value.string_value {
        Some(value) => celox::InjectedValue::String(value),
        None => celox::InjectedValue::Unit,
    })
}

#[cfg(not(target_arch = "wasm32"))]
impl celox::InjectedComponentHandler for NapiInjectedHandler {
    fn call(
        &self,
        call: celox::InjectedCall,
    ) -> std::result::Result<celox::InjectedResult, String> {
        let (phase, method, args) = match call.hook {
            celox::InjectedHook::Create => ("create", None, Vec::new()),
            celox::InjectedHook::Init => ("init", None, Vec::new()),
            celox::InjectedHook::Reset => ("reset", None, Vec::new()),
            celox::InjectedHook::Clock => ("clock", None, Vec::new()),
            celox::InjectedHook::Finish => ("finish", None, Vec::new()),
            celox::InjectedHook::Method { name, args } => ("method", Some(name), args),
        };
        let request = NapiInjectedCall {
            instance: call.instance,
            phase: phase.into(),
            method,
            inputs: call
                .inputs
                .into_iter()
                .map(|value| to_napi_injected_value(Some(value.name), value.value))
                .collect(),
            params: call
                .params
                .into_iter()
                .map(|value| to_napi_injected_value(Some(value.name), value.value))
                .collect(),
            ports: call
                .ports
                .into_iter()
                .map(|port| NapiInjectedPort {
                    name: port.name,
                    direction: port.direction,
                    role: port.role,
                    width: port.width,
                })
                .collect(),
            args: args
                .into_iter()
                .map(|value| to_napi_injected_value(None, value))
                .collect(),
            cycle: napi_bigint(call.cycle),
            time: napi_bigint(call.time),
            seed: napi_bigint(call.seed),
            fired_clock: call.fired_clock,
            four_state: call.four_state,
        };
        let result = self
            .handler
            .borrow_back(&self.env)
            .and_then(|handler| handler.call(request))
            .map_err(|error| error.to_string())?;
        let outputs = result
            .outputs
            .unwrap_or_default()
            .into_iter()
            .map(|value| {
                let name = value
                    .name
                    .clone()
                    .ok_or_else(|| "component callback output has no name".to_string())?;
                Ok(celox::InjectedNamedValue {
                    name,
                    value: from_napi_injected_value(value)?,
                })
            })
            .collect::<std::result::Result<Vec<_>, String>>()?;
        Ok(celox::InjectedResult {
            outputs,
            return_value: result
                .return_value
                .map(from_napi_injected_value)
                .transpose()?,
            failures: result.failures.unwrap_or_default(),
            logs: result.logs.unwrap_or_default(),
            finish: result.finish.unwrap_or(false),
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn injected_components(
    env: Env,
    definitions: Option<Vec<NapiInjectedComponent>>,
) -> Result<celox::InjectedComponents> {
    let mut components = celox::InjectedComponents::new();
    for definition in definitions.unwrap_or_default() {
        components
            .insert(
                definition.name,
                &definition.manifest,
                Arc::new(NapiInjectedHandler {
                    env,
                    handler: definition.handler,
                }),
            )
            .map_err(Error::from_reason)?;
    }
    Ok(components)
}

/// Run a native testbench from Veryl source code.
///
/// Compiles the given sources and runs the `#[test]` module specified by `top`,
/// returning assertion results observed before that test finishes or stops on a
/// fatal failure.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub fn run_test(
    env: Env,
    sources: Vec<NapiSourceFile>,
    top: String,
    options: Option<NapiOptions>,
    components: Option<Vec<NapiInjectedComponent>>,
) -> Result<NapiTestResult> {
    let opts = parse_options(&options)?;
    let mut src_pairs: Vec<(String, std::path::PathBuf)> = sources
        .into_iter()
        .map(|s| (s.content, std::path::PathBuf::from(s.path)))
        .collect();
    append_extra_source(&mut src_pairs, &opts.extra_source);

    let source_refs: Vec<(&str, &std::path::Path)> = src_pairs
        .iter()
        .map(|(s, p)| (s.as_str(), p.as_path()))
        .collect();
    let builder = apply_options(celox::Simulator::from_sources(source_refs, &top), &opts)
        .with_injected_components(injected_components(env, components)?);
    let result = builder
        .run_test_detailed()
        .map_err(|e| Error::from_reason(format!("{e}")))?;
    Ok(convert_test_result(result))
}

/// Run a Veryl native testbench against an external frontend artifact.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub fn run_test_with_frontend_artifact(
    env: Env,
    artifact_json: String,
    sources: Vec<NapiSourceFile>,
    top: String,
    options: Option<NapiOptions>,
    components: Option<Vec<NapiInjectedComponent>>,
) -> Result<NapiTestResult> {
    let opts = parse_options(&options)?;
    let artifact = celox::FrontendArtifact::from_json(&artifact_json)
        .map_err(|error| Error::from_reason(error.to_string()))?;
    let mut src_pairs: Vec<(String, std::path::PathBuf)> = sources
        .into_iter()
        .map(|source| (source.content, std::path::PathBuf::from(source.path)))
        .collect();
    append_extra_source(&mut src_pairs, &opts.extra_source);
    let source_refs: Vec<(&str, &std::path::Path)> = src_pairs
        .iter()
        .map(|(source, path)| (source.as_str(), path.as_path()))
        .collect();
    let builder = apply_options(
        celox::Simulator::from_frontend_with_testbench(artifact, source_refs, &top),
        &opts,
    )
    .with_injected_components(injected_components(env, components)?);
    let result = builder
        .run_test_detailed()
        .map_err(|error| Error::from_reason(error.to_string()))?;
    Ok(convert_test_result(result))
}

/// Run a native testbench from a Veryl project directory.
///
/// Searches upward from `project_path` for `Veryl.toml`, gathers all
/// `.veryl` source files, and runs the `#[test]` module specified by `top`.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub fn run_test_from_project(
    env: Env,
    project_path: String,
    top: String,
    options: Option<NapiOptions>,
    components: Option<Vec<NapiInjectedComponent>>,
) -> Result<NapiTestResult> {
    let mut opts = parse_options(&options)?;
    let (mut sources, metadata, celox_cfg) = load_project_sources(&project_path)?;
    opts.common.diagnostic_policy = celox_cfg.diagnostics.clone();
    append_extra_source(&mut sources, &opts.extra_source);

    let source_refs: Vec<(&str, &std::path::Path)> = sources
        .iter()
        .map(|(s, p)| (s.as_str(), p.as_path()))
        .collect();
    let builder = apply_options(
        celox::Simulator::from_sources(source_refs, &top).with_metadata(metadata),
        &opts,
    )
    .with_injected_components(injected_components(env, components)?);
    let result = builder
        .run_test_detailed()
        .map_err(|e| Error::from_reason(format!("{e}")))?;
    Ok(convert_test_result(result))
}
