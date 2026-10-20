//! Project and file-list resolution shared by `run` and `vpi build`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use clap::Args;
use serde::Deserialize;

#[derive(Args, Default)]
pub(super) struct DesignArgs {
    /// Ordered SV sources, or one Veryl project entry file.
    pub sources: Vec<PathBuf>,
    /// Override a compilation diagnostic: CODE=allow|warn|error (repeatable).
    #[arg(long = "diagnostic", value_name = "CODE=LEVEL")]
    diagnostics: Vec<String>,
    /// Optimization level: O0, O1, O2 (default: project setting or O2 for run, O1 for vpi build).
    #[arg(short = 'O', long, value_parser = super::parse_opt_level)]
    opt_level: Option<celox::OptLevel>,
    /// Preserve a hierarchical signal for observation (repeatable; replaces project list).
    #[arg(
        long = "live-signal",
        value_name = "PATH",
        conflicts_with = "no_live_signals"
    )]
    live_signals: Vec<String>,
    /// Permit force/release on a packed SV signal (repeatable; replaces project list).
    #[arg(
        long = "force-signal",
        value_name = "PATH",
        conflicts_with = "no_force_signals"
    )]
    force_signals: Vec<String>,
    /// Clear the project's live_signals list.
    #[arg(long)]
    no_live_signals: bool,
    /// Clear the project's force_signals list.
    #[arg(long)]
    no_force_signals: bool,
    /// Project manifest (otherwise discover celox.toml when no inputs are given).
    #[arg(long, value_name = "FILE")]
    project: Option<PathBuf>,
    /// Named target in celox.toml.
    #[arg(long, value_name = "NAME")]
    target: Option<String>,
    /// Ordered SV file list; relative entries resolve from the list's directory.
    #[arg(short = 'f', long = "filelist", value_name = "FILE")]
    filelists: Vec<PathBuf>,
    /// SystemVerilog include search directory (repeatable).
    #[arg(short = 'I', long = "include-dir", value_name = "DIR")]
    include_dirs: Vec<PathBuf>,
    /// SystemVerilog macro, NAME or NAME=VALUE (repeatable; last value wins).
    #[arg(short = 'D', long = "define", value_name = "NAME[=VALUE]")]
    defines: Vec<String>,
    /// Override a numeric top parameter (repeatable; last value wins).
    #[arg(
        short = 'G',
        long = "parameter",
        visible_alias = "param",
        value_name = "NAME=VALUE"
    )]
    parameters: Vec<String>,
    /// Top-level module; overrides the selected target.
    #[arg(long)]
    top: Option<String>,
    #[arg(skip)]
    input_order: Vec<Input>,
}

enum Input {
    Source(usize),
    Filelist(usize),
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Manifest {
    // Existing NAPI project settings can share this manifest. Their consumers
    // remain in celox-napi; keep these types aligned with CeloxConfig there.
    #[serde(rename = "exclude")]
    _exclude: Vec<String>,
    #[serde(rename = "test")]
    _test: VerylTestConfig,
    #[serde(rename = "simulation")]
    _simulation: VerylSimulationConfig,
    diagnostics: celox::DiagnosticPolicy,
    sources: Vec<PathBuf>,
    filelists: Vec<PathBuf>,
    include_dirs: Vec<PathBuf>,
    defines: BTreeMap<String, String>,
    parameters: BTreeMap<String, ParameterValue>,
    top: Option<String>,
    opt_level: Option<String>,
    live_signals: Option<Vec<String>>,
    force_signals: Option<Vec<String>>,
    max_steps: Option<u64>,
    default_target: Option<String>,
    targets: BTreeMap<String, Target>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct VerylTestConfig {
    #[serde(rename = "sources")]
    _sources: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct VerylSimulationConfig {
    #[serde(rename = "max_steps")]
    _max_steps: Option<u32>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Target {
    diagnostics: celox::DiagnosticPolicy,
    sources: Vec<PathBuf>,
    filelists: Vec<PathBuf>,
    include_dirs: Vec<PathBuf>,
    defines: BTreeMap<String, String>,
    parameters: BTreeMap<String, ParameterValue>,
    top: Option<String>,
    opt_level: Option<String>,
    live_signals: Option<Vec<String>>,
    force_signals: Option<Vec<String>>,
    max_steps: Option<u64>,
}

#[derive(Default)]
pub(super) struct Design {
    pub diagnostics: celox::DiagnosticPolicy,
    pub sources: Vec<PathBuf>,
    pub include_dirs: Vec<PathBuf>,
    pub defines: BTreeMap<String, String>,
    pub parameters: BTreeMap<String, u64>,
    pub top: String,
    pub opt_level: Option<celox::OptLevel>,
    pub signal_access: celox::SignalAccessOptions,
    pub max_steps: Option<u64>,
}

impl DesignArgs {
    pub fn capture_input_order(&mut self, matches: &clap::ArgMatches) {
        let mut inputs = Vec::new();
        for (id, is_list) in [("sources", false), ("filelists", true)] {
            if let Some(indices) = matches.indices_of(id) {
                inputs.extend(indices.enumerate().map(|(offset, position)| {
                    (
                        position,
                        if is_list {
                            Input::Filelist(offset)
                        } else {
                            Input::Source(offset)
                        },
                    )
                }));
            }
        }
        inputs.sort_by_key(|(position, _)| *position);
        self.input_order = inputs.into_iter().map(|(_, input)| input).collect();
    }

    pub fn resolve(self) -> Result<Design, String> {
        let manifest_path = match self.project {
            Some(path) => Some(path),
            None if self.target.is_some()
                || (self.sources.is_empty() && self.filelists.is_empty()) =>
            {
                let cwd = std::env::current_dir()
                    .map_err(|error| format!("failed to locate current directory: {error}"))?;
                cwd.ancestors()
                    .map(|path| path.join("celox.toml"))
                    .find(|path| path.is_file())
            }
            None => None,
        };
        if self.target.is_some() && manifest_path.is_none() {
            return Err(
                "--target requires --project FILE or a discoverable celox.toml".to_string(),
            );
        }
        let mut design = Design::default();
        if let Some(path) = manifest_path {
            let path = path
                .canonicalize()
                .map_err(|error| format!("failed to open project {}: {error}", path.display()))?;
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("failed to read project {}: {error}", path.display()))?;
            let config: Manifest = toml::from_str(&text)
                .map_err(|error| format!("invalid project {}: {error}", path.display()))?;
            let base = path.parent().expect("canonical project has a parent");
            let target_name = self.target.as_ref().or(config.default_target.as_ref());
            let selected = match target_name {
                Some(name) => Some(config.targets.get(name).ok_or_else(|| {
                    format!(
                        "unknown target `{name}` in {}; available targets: {}",
                        path.display(),
                        config
                            .targets
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?),
                None if config.top.is_some() || config.targets.is_empty() => None,
                None if config.targets.len() == 1 => config.targets.values().next(),
                None => {
                    return Err(format!(
                        "select --target NAME; available targets: {}",
                        config
                            .targets
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            };
            design.extend(
                base,
                &config.sources,
                &config.filelists,
                &config.include_dirs,
                &config.defines,
            )?;
            design.extend_parameters(&config.parameters)?;
            design.diagnostics = config.diagnostics.clone();
            design.top = config.top.clone().unwrap_or_default();
            let mut opt_level = config.opt_level.as_deref();
            design.signal_access.live_signals = config.live_signals.clone().unwrap_or_default();
            design.signal_access.force_signals = config.force_signals.clone().unwrap_or_default();
            design.max_steps = config.max_steps;
            if let Some(target) = selected {
                design.extend_parameters(&target.parameters)?;
                design.diagnostics.0.extend(target.diagnostics.0.clone());
                design.extend(
                    base,
                    &target.sources,
                    &target.filelists,
                    &target.include_dirs,
                    &target.defines,
                )?;
                if let Some(top) = &target.top {
                    design.top.clone_from(top);
                }
                opt_level = target.opt_level.as_deref().or(opt_level);
                if let Some(paths) = &target.live_signals {
                    design.signal_access.live_signals.clone_from(paths);
                }
                if let Some(paths) = &target.force_signals {
                    design.signal_access.force_signals.clone_from(paths);
                }
                design.max_steps = target.max_steps.or(design.max_steps);
            }
            design.opt_level = opt_level
                .map(|value| {
                    celox::OptLevel::parse(value).ok_or_else(|| {
                        format!("invalid opt_level `{value}`: expected O0, O1, or O2")
                    })
                })
                .transpose()?;
            if design.max_steps == Some(0) {
                return Err("max_steps must be positive".to_string());
            }
        }
        design.opt_level = self.opt_level.or(design.opt_level);
        if self.no_live_signals {
            design.signal_access.live_signals.clear();
        } else if !self.live_signals.is_empty() {
            design.signal_access.live_signals = self.live_signals;
        }
        if self.no_force_signals {
            design.signal_access.force_signals.clear();
        } else if !self.force_signals.is_empty() {
            design.signal_access.force_signals = self.force_signals;
        }
        let inputs = if self.input_order.is_empty() {
            (0..self.sources.len())
                .map(Input::Source)
                .chain((0..self.filelists.len()).map(Input::Filelist))
                .collect()
        } else {
            self.input_order
        };
        for input in inputs {
            match input {
                Input::Source(index) => design.sources.push(self.sources[index].clone()),
                Input::Filelist(index) => {
                    design.read_filelist(&self.filelists[index], &mut Vec::new())?
                }
            }
        }
        for diagnostic in &self.diagnostics {
            design.diagnostics.set_override(diagnostic)?;
        }
        design.include_dirs.extend(self.include_dirs);
        for define in &self.defines {
            design.define(define)?;
        }
        for parameter in self.parameters {
            let (name, value) = parameter
                .split_once('=')
                .ok_or_else(|| format!("invalid parameter `{parameter}`: expected NAME=VALUE"))?;
            design.set_parameter(name, parse_parameter_value(value)?)?;
        }
        if let Some(top) = self.top {
            design.top = top;
        }
        if design.sources.is_empty() {
            return Err(
                "provide sources, -f FILE, or a celox.toml project with sources".to_string(),
            );
        }
        if design.top.trim().is_empty() {
            return Err("provide --top MODULE or set top in celox.toml".to_string());
        }
        if !design.is_sv() && (!design.include_dirs.is_empty() || !design.defines.is_empty()) {
            return Err(
                "include directories and defines require SystemVerilog sources".to_string(),
            );
        }
        Ok(design)
    }
}

/// TOML integers cover ordinary values; strings also permit radix prefixes
/// and the full unsigned 64-bit range supported by SimulatorBuilder::param.
#[derive(Deserialize)]
#[serde(untagged)]
enum ParameterValue {
    Integer(u64),
    String(String),
}

fn parse_parameter_value(value: &str) -> Result<u64, String> {
    let (radix, digits) = if let Some(digits) = value.strip_prefix("0x") {
        (16, digits)
    } else if let Some(digits) = value.strip_prefix("0b") {
        (2, digits)
    } else if let Some(digits) = value.strip_prefix("0o") {
        (8, digits)
    } else {
        (10, value)
    };
    let invalid = || {
        format!(
            "invalid parameter value `{value}`: expected an unsigned 64-bit integer (decimal, 0x, 0o, or 0b)"
        )
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || digits.starts_with('+')
    {
        return Err(invalid());
    }
    u64::from_str_radix(&digits.replace('_', ""), radix).map_err(|_| invalid())
}

impl Design {
    pub fn apply_parameters<'a, T>(
        &self,
        mut builder: celox::SimulatorBuilder<'a, T>,
    ) -> celox::SimulatorBuilder<'a, T> {
        for (name, value) in &self.parameters {
            builder = builder.param(name, *value);
        }
        builder
    }

    fn extend_parameters(
        &mut self,
        parameters: &BTreeMap<String, ParameterValue>,
    ) -> Result<(), String> {
        for (name, value) in parameters {
            let value = match value {
                ParameterValue::Integer(value) => *value,
                ParameterValue::String(value) => parse_parameter_value(value)?,
            };
            self.set_parameter(name, value)?;
        }
        Ok(())
    }

    fn set_parameter(&mut self, name: &str, value: u64) -> Result<(), String> {
        if name.is_empty()
            || !name.bytes().enumerate().all(|(index, byte)| {
                byte == b'_'
                    || byte.is_ascii_alphabetic()
                    || (index != 0 && (byte.is_ascii_digit() || byte == b'$'))
            })
        {
            return Err(format!("invalid top parameter name `{name}`"));
        }
        self.parameters.insert(name.to_string(), value);
        Ok(())
    }

    pub fn is_sv(&self) -> bool {
        self.sources.iter().all(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sv" || extension == "v")
        })
    }

    fn extend(
        &mut self,
        base: &Path,
        sources: &[PathBuf],
        lists: &[PathBuf],
        includes: &[PathBuf],
        defines: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        self.sources
            .extend(sources.iter().map(|path| base.join(path)));
        self.include_dirs
            .extend(includes.iter().map(|path| base.join(path)));
        for (name, value) in defines {
            self.define(&format!("{name}={value}"))?;
        }
        for list in lists {
            self.read_filelist(&base.join(list), &mut Vec::new())?;
        }
        Ok(())
    }

    fn define(&mut self, definition: &str) -> Result<(), String> {
        let (name, value) = definition.split_once('=').unwrap_or((definition, "1"));
        if name.is_empty()
            || !name.bytes().enumerate().all(|(index, byte)| {
                byte == b'_'
                    || byte.is_ascii_alphabetic()
                    || (index != 0 && (byte.is_ascii_digit() || byte == b'$'))
            })
            || value.contains(['\n', '\r'])
        {
            return Err(format!("invalid macro definition `{definition}`"));
        }
        self.defines.insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn read_filelist(&mut self, path: &Path, active: &mut Vec<PathBuf>) -> Result<(), String> {
        let path = path
            .canonicalize()
            .map_err(|error| format!("failed to open file list {}: {error}", path.display()))?;
        if active.contains(&path) {
            return Err(format!("recursive file list: {}", path.display()));
        }
        if active.len() >= 64 {
            return Err("file-list nesting exceeds 64 levels".to_string());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("failed to read file list {}: {error}", path.display()))?;
        let tokens = tokenize(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        let base = path.parent().expect("canonical file list has a parent");
        active.push(path.clone());
        let mut tokens = tokens.iter();
        while let Some(token) = tokens.next() {
            match token.as_str() {
                "-f" | "-I" | "-D" => {
                    let value = tokens
                        .next()
                        .ok_or_else(|| format!("{}: {token} requires a value", path.display()))?;
                    match token.as_str() {
                        "-f" => self.read_filelist(&base.join(value), active)?,
                        "-I" => self.include_dirs.push(base.join(value)),
                        _ => self.define(value)?,
                    }
                }
                _ if token.starts_with("+incdir+") => {
                    for value in token[8..].split('+') {
                        if value.is_empty() {
                            return Err(format!("{}: empty +incdir+ path", path.display()));
                        }
                        self.include_dirs.push(base.join(value));
                    }
                }
                _ if token.starts_with("+define+") => {
                    for value in token[8..].split('+') {
                        self.define(value)?;
                    }
                }
                _ if token.starts_with("-I") && token.len() > 2 => {
                    self.include_dirs.push(base.join(&token[2..]))
                }
                _ if token.starts_with("-D") && token.len() > 2 => self.define(&token[2..])?,
                _ if token.starts_with('-') || token.starts_with('+') => {
                    return Err(format!(
                        "{}: unsupported file-list option `{token}`",
                        path.display()
                    ));
                }
                _ => self.sources.push(base.join(token)),
            }
        }
        active.pop();
        Ok(())
    }

    #[cfg(feature = "systemverilog")]
    pub fn sv_sources(&self) -> Result<Vec<(String, PathBuf)>, String> {
        let macros = self
            .defines
            .iter()
            .map(|(name, value)| (name.clone(), Some(value.clone())))
            .collect::<Vec<_>>();
        let includes = self
            .include_dirs
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        self.sources
            .iter()
            .map(|path| {
                let text = std::fs::read_to_string(path)
                    .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
                let text = if macros.is_empty() && includes.is_empty() {
                    text
                } else {
                    celox_sv_analyzer::syntax::preprocess_source(&text, path, &macros, &includes)
                        .map_err(|error| {
                            format!("preprocessing {} failed: {error}", path.display())
                        })?
                };
                Ok((text, path.clone()))
            })
            .collect()
    }
}

/// Whitespace-separated paths/options, quoted paths, backslash escapes and
/// #, // and /* */ comments. No shell expansion or execution is performed.
fn tokenize(text: &str) -> Result<Vec<String>, String> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    while let Some(c) = chars.next() {
        if let Some(delimiter) = quote {
            if c == delimiter {
                quote = None;
            } else if c == '\\' && delimiter == '"' {
                token.push(chars.next().ok_or("trailing escape in quoted path")?);
            } else {
                token.push(c);
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == '\\' {
            token.push(chars.next().ok_or("trailing escape in file list")?);
        } else if (c == '#' && token.is_empty())
            || (c == '/' && chars.peek() == Some(&'/') && token.is_empty())
        {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    closed = true;
                    break;
                }
            }
            if !closed {
                return Err("unterminated file-list comment".to_string());
            }
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else if c.is_whitespace() {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(c);
        }
    }
    if quote.is_some() {
        return Err("unterminated quoted path in file list".to_string());
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}
