//! CXXRTL Debug Server Protocol v0 over a single loopback TCP connection.
//! https://cxxrtl.org/protocol.html

use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    ops::Range,
    rc::Rc,
    time::Duration,
};

use celox::{InstanceHierarchy, RuntimeEvent, SignalRef, SimBackend, Simulation, VariableKind};
use celox_design::{AssertionDirective, AssertionOutcome};
use data_encoding::BASE64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::SvOutput;

const FS_PER_SECOND: u128 = 1_000_000_000_000_000;
const MAX_TIME: u128 = (i32::MAX as u128 + 1) * FS_PER_SECOND - 1;
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const ENCODING: &str = "base64(u32)";

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Greeting {
        version: u32,
    },
    Command {
        #[serde(flatten)]
        command: Command,
    },
}

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Command {
    ListScopes {
        scope: Option<String>,
    },
    ListItems {
        scope: Option<String>,
    },
    ReferenceItems {
        reference: String,
        items: Option<Vec<(String,)>>,
    },
    QueryInterval {
        interval: [String; 2],
        collapse: bool,
        items: Option<String>,
        item_values_encoding: Option<String>,
        diagnostics: bool,
    },
    GetSimulationStatus,
    RunSimulation {
        until_time: Option<String>,
        until_diagnostics: Vec<DiagnosticKind>,
        sample_item_values: bool,
    },
    PauseSimulation,
}

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticKind {
    Break,
    Print,
    Assert,
    Assume,
}

#[derive(Serialize)]
struct Diagnostic {
    #[serde(rename = "type")]
    kind: DiagnosticKind,
    text: String,
    src: Option<String>,
}

impl Diagnostic {
    fn from_event(event: &RuntimeEvent) -> Option<Self> {
        let (kind, text, src) = match event {
            RuntimeEvent::Display { message } => (
                DiagnosticKind::Print,
                format!("{}\n", String::from_utf8_lossy(message)),
                None,
            ),
            RuntimeEvent::Write { message } => (
                DiagnosticKind::Print,
                String::from_utf8_lossy(message).into_owned(),
                None,
            ),
            RuntimeEvent::Report(report) => {
                let kind = match report
                    .assertion
                    .as_ref()
                    .filter(|site| site.outcome == AssertionOutcome::Fail)
                    .map(|site| site.directive)
                {
                    Some(AssertionDirective::Assert) => DiagnosticKind::Assert,
                    Some(AssertionDirective::Assume) => DiagnosticKind::Assume,
                    _ => DiagnosticKind::Print,
                };
                let mut text = String::from_utf8_lossy(&report.render()).into_owned();
                if kind == DiagnosticKind::Print {
                    text.push('\n');
                }
                let src = report
                    .location
                    .as_ref()
                    .map(|location| format!("{}:{}", location.file, location.line));
                (kind, text, src)
            }
            RuntimeEvent::Stop => return Some(Self::stopped()),
            RuntimeEvent::Finish | RuntimeEvent::Missed { .. } => return None,
        };
        Some(Self { kind, text, src })
    }

    fn stopped() -> Self {
        Self {
            kind: DiagnosticKind::Break,
            text: "SystemVerilog testbench requested $stop".into(),
            src: None,
        }
    }
}

fn parse_time(time: &str) -> Result<u128, &'static str> {
    let (seconds, fs) = time.split_once('.').ok_or("invalid time point")?;
    if !seconds
        .bytes()
        .chain(fs.bytes())
        .all(|byte| byte.is_ascii_digit())
    {
        return Err("invalid time point");
    }
    let seconds = seconds.parse::<i32>().map_err(|_| "invalid seconds")?;
    let fs = fs.parse::<u128>().map_err(|_| "invalid femtoseconds")?;
    if fs >= FS_PER_SECOND {
        return Err("femtoseconds are out of range");
    }
    Ok(seconds as u128 * FS_PER_SECOND + fs)
}

fn format_time(time: u128) -> String {
    format!("{}.{:015}", time / FS_PER_SECOND, time % FS_PER_SECOND)
}

struct Item {
    signal: SignalRef,
    bytes: Range<usize>,
    input: bool,
    output: bool,
}

#[derive(Default)]
struct Waveforms {
    scopes: BTreeMap<String, String>,
    items: BTreeMap<String, Item>,
    sample_bytes: usize,
    samples: Vec<Sample>,
}

struct Sample {
    time: u128,
    values: Option<Rc<[u8]>>,
    diagnostics: Vec<Diagnostic>,
}

impl Waveforms {
    fn add_instance(&mut self, scope: &str, instance: InstanceHierarchy) {
        self.scopes.insert(scope.into(), instance.module_name);
        for named in instance.signals {
            if named.info.value_kind.is_handle() || named.signal.width == 0 {
                continue;
            }
            let end = self.sample_bytes + named.signal.width.div_ceil(32) * 4;
            self.items.insert(
                child_path(scope, &named.name),
                Item {
                    signal: named.signal,
                    bytes: self.sample_bytes..end,
                    input: matches!(
                        named.info.var_kind,
                        VariableKind::Input | VariableKind::Inout
                    ),
                    output: matches!(
                        named.info.var_kind,
                        VariableKind::Output | VariableKind::Inout
                    ),
                },
            );
            self.sample_bytes = end;
        }
        for (name, instances) in instance.children {
            for child in instances {
                let name = if child.indexed {
                    format!("{name}[{}]", child.index)
                } else {
                    name.clone()
                };
                self.add_instance(&child_path(scope, &name), child);
            }
        }
    }

    fn record<B: SimBackend>(
        &mut self,
        simulation: &Simulation<B>,
        time: u128,
        sample_values: bool,
        diagnostics: Vec<Diagnostic>,
    ) {
        let values = sample_values.then(|| {
            let mut values = vec![0; self.sample_bytes];
            for item in self.items.values() {
                let (value, unknown) = simulation.peek_four_state(item.signal);
                let data = &mut values[item.bytes.clone()];
                for (slot, byte) in data.iter_mut().zip(value.to_bytes_le()) {
                    *slot = byte;
                }
                // CDSP v0 carries only binary values. Present X and Z as zero.
                if unknown.bits() != 0 {
                    for (slot, mask) in data.iter_mut().zip(unknown.to_bytes_le()) {
                        *slot &= !mask;
                    }
                }
            }
            match self
                .samples
                .last()
                .and_then(|sample| sample.values.as_ref())
            {
                Some(previous) if **previous == values => Rc::clone(previous),
                _ => Rc::from(values),
            }
        });
        self.samples.push(Sample {
            time,
            values,
            diagnostics,
        });
    }

    fn latest_time(&self) -> u128 {
        self.samples.last().expect("initial CDSP sample").time
    }

    fn query(
        &self,
        range: [String; 2],
        collapse: bool,
        items: Option<&[Range<usize>]>,
        diagnostics: bool,
    ) -> Result<Vec<Value>, &'static str> {
        let [begin, end] = range.each_ref().map(|time| parse_time(time));
        let (begin, end) = (begin?, end?);
        if begin > end || end > self.latest_time() {
            return Err("interval is outside the recorded waveform");
        }
        let mut first = self.samples.partition_point(|sample| sample.time < begin);
        let last = self.samples.partition_point(|sample| sample.time <= end);
        if first > 0 && (first == last || self.samples[first].time > begin) {
            first -= 1;
            if collapse {
                let time = self.samples[first].time;
                first = self.samples.partition_point(|sample| sample.time < time);
            }
        }
        let mut samples = Vec::new();
        let mut bytes = Vec::new();
        let mut encode = |group: &[Sample]| {
            let sample = group.last().expect("nonempty CDSP sample group");
            let mut result = json!({ "time": format_time(sample.time) });
            if let Some(items) = items {
                result["item_values"] = sample.values.as_ref().map_or(Value::Null, |values| {
                    bytes.clear();
                    for item in items {
                        bytes.extend_from_slice(&values[item.clone()]);
                    }
                    Value::String(BASE64.encode(&bytes))
                });
            }
            if diagnostics {
                result["diagnostics"] = json!(
                    group
                        .iter()
                        .flat_map(|sample| &sample.diagnostics)
                        .collect::<Vec<_>>()
                );
            }
            samples.push(result);
        };
        let selected = &self.samples[first..last];
        if collapse {
            for group in selected.chunk_by(|left, right| left.time == right.time) {
                encode(group);
            }
        } else {
            for sample in selected {
                encode(std::slice::from_ref(sample));
            }
        }
        Ok(samples)
    }
}

fn child_path(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.into()
    } else {
        format!("{scope} {name}")
    }
}

fn in_scope(path: &str, scope: Option<&str>) -> bool {
    scope.is_none_or(|scope| {
        !path.is_empty() && path.rsplit_once(' ').map_or("", |(parent, _)| parent) == scope
    })
}

struct Run {
    until_time: Option<u128>,
    diagnostics: Vec<DiagnosticKind>,
    sample_values: bool,
}

enum Status {
    Paused,
    Running(Run),
    Finished,
}

struct Session {
    waves: Waveforms,
    references: BTreeMap<String, Vec<Range<usize>>>,
    status: Status,
    fs_per_tick: u128,
    output: SvOutput,
    steps: u64,
    max_steps: u64,
    failure: Option<String>,
}

impl Session {
    fn time<B: SimBackend>(&self, simulation: &Simulation<B>) -> u128 {
        u128::from(simulation.time()) * self.fs_per_tick
    }

    fn next_time<B: SimBackend>(&self, simulation: &Simulation<B>) -> Option<u128> {
        simulation
            .next_event_time()
            .map(|ticks| u128::from(ticks) * self.fs_per_tick)
    }

    fn initialize<B: SimBackend>(&mut self, simulation: &mut Simulation<B>, out: &mut impl Write) {
        let result = simulation.eval_comb();
        self.record_result(simulation, out, result, true);
        if !simulation.is_finished()
            && self.failure.is_none()
            && !simulation.is_stopped()
            && simulation.next_event_time() == Some(0)
        {
            let result = simulation.step().map(|_| ());
            self.steps += 1;
            self.record_result(simulation, out, result, true);
        }
        if simulation.is_finished() || self.failure.is_some() {
            self.finish(simulation);
        }
    }

    fn record_result<B: SimBackend>(
        &mut self,
        simulation: &mut Simulation<B>,
        out: &mut impl Write,
        result: Result<(), celox::RuntimeErrorCode>,
        sample_values: bool,
    ) -> Vec<DiagnosticKind> {
        let events = simulation.drain_runtime_events();
        let mut diagnostics: Vec<_> = events.iter().filter_map(Diagnostic::from_event).collect();
        if simulation.is_stopped()
            && !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == DiagnosticKind::Break)
        {
            diagnostics.push(Diagnostic::stopped());
        }
        let kinds = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.kind)
            .collect();
        let written = self
            .output
            .write(events, out)
            .and_then(|()| self.output.check_errors());
        if let Err(message) = written.and(result.map_err(|error| error.to_string())) {
            self.failure = Some(message);
        }
        self.waves.record(
            simulation,
            self.time(simulation),
            sample_values,
            diagnostics,
        );
        kinds
    }

    fn finish<B: SimBackend>(&mut self, simulation: &mut Simulation<B>) -> Value {
        self.status = Status::Finished;
        if let Err(error) = simulation.flush_vcd() {
            self.failure
                .get_or_insert_with(|| format!("waveform flush failed: {error}"));
        }
        if self.output.failed {
            self.failure.get_or_insert_with(|| {
                "SystemVerilog testbench reported an assertion failure or $error".into()
            });
        }
        json!({ "type": "event", "event": "simulation_finished", "time": format_time(self.waves.latest_time()) })
    }

    fn pause<B: SimBackend>(&mut self, simulation: &Simulation<B>) {
        if matches!(&self.status, Status::Running(run) if !run.sample_values) {
            self.waves
                .record(simulation, self.time(simulation), true, Vec::new());
        }
        self.status = Status::Paused;
    }

    fn pause_event<B: SimBackend>(&mut self, simulation: &Simulation<B>, cause: &str) -> Value {
        self.pause(simulation);
        json!({
            "type": "event", "event": "simulation_paused",
            "time": format_time(self.waves.latest_time()), "cause": cause,
        })
    }

    fn advance<B: SimBackend>(
        &mut self,
        simulation: &mut Simulation<B>,
        out: &mut impl Write,
    ) -> Option<Value> {
        let Status::Running(run) = &self.status else {
            return None;
        };
        let next = self.next_time(simulation);
        if run.until_time.is_some_and(|limit| {
            next.is_some_and(|next| next > limit) || self.time(simulation) >= limit
        }) {
            return Some(self.pause_event(simulation, "until_time"));
        }
        if next.is_some_and(|time| time > MAX_TIME) {
            self.failure = Some("simulation time exceeds the CDSP time range".into());
        } else if (next.is_some() || simulation.is_stopped()) && self.steps == self.max_steps {
            self.failure = Some(format!(
                "simulation exceeded --max-steps {}",
                self.max_steps
            ));
        } else {
            let result = if next.is_some() || simulation.is_stopped() {
                self.steps += 1;
                simulation.step().map(|_| ())
            } else {
                simulation.finish()
            };
            let kinds = self.record_result(simulation, out, result, run.sample_values);
            if !simulation.is_finished() && self.failure.is_none() {
                let Status::Running(run) = &self.status else {
                    unreachable!()
                };
                if simulation.is_stopped()
                    || kinds.iter().any(|kind| {
                        *kind == DiagnosticKind::Break || run.diagnostics.contains(kind)
                    })
                {
                    return Some(self.pause_event(simulation, "until_diagnostics"));
                }
                return None;
            }
        }
        if matches!(&self.status, Status::Running(run) if !run.sample_values) {
            self.waves
                .record(simulation, self.time(simulation), true, Vec::new());
        }
        Some(self.finish(simulation))
    }

    fn command<B: SimBackend>(
        &mut self,
        command: Command,
        simulation: &Simulation<B>,
    ) -> Result<Value, &'static str> {
        Ok(match command {
            Command::ListScopes { scope } => {
                let scopes: serde_json::Map<_, _> = self
                    .waves
                    .scopes
                    .iter()
                    .filter(|(name, _)| in_scope(name, scope.as_deref()))
                    .map(|(name, module)| {
                        (
                            name.clone(),
                            json!({
                                "type": "module",
                                "definition": { "name": module, "src": null, "attributes": {} },
                                "instantiation": { "src": null, "attributes": {} },
                            }),
                        )
                    })
                    .collect();
                json!({ "command": "list_scopes", "scopes": scopes })
            }
            Command::ListItems { scope } => {
                let items: serde_json::Map<_, _> = self
                    .waves
                    .items
                    .iter()
                    .filter(|(name, _)| in_scope(name, scope.as_deref()))
                    .map(|(name, item)| {
                        (
                            name.clone(),
                            json!({
                                "type": "node", "width": item.signal.width, "lsb_at": 0,
                                "input": item.input, "output": item.output, "settable": false,
                                "src": null, "attributes": {},
                            }),
                        )
                    })
                    .collect();
                json!({ "command": "list_items", "items": items })
            }
            Command::ReferenceItems { reference, items } => {
                if reference.is_empty() {
                    return Err("reference identifier is empty");
                }
                if let Some(items) = items {
                    let ranges = items
                        .iter()
                        .map(|(name,)| {
                            self.waves
                                .items
                                .get(name)
                                .map(|item| item.bytes.clone())
                                .ok_or("unknown item")
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    self.references.insert(reference, ranges);
                } else {
                    self.references.remove(&reference);
                }
                json!({ "command": "reference_items" })
            }
            Command::QueryInterval {
                interval,
                collapse,
                items,
                item_values_encoding,
                diagnostics,
            } => {
                let ranges = match (item_values_encoding.as_deref(), items) {
                    (None, _) | (Some(ENCODING), None) => None,
                    (Some(ENCODING), Some(reference)) => Some(
                        self.references
                            .get(&reference)
                            .filter(|ranges| !ranges.is_empty())
                            .ok_or("unknown or empty item reference")?
                            .as_slice(),
                    ),
                    _ => return Err("unsupported item values encoding"),
                };
                let samples = self.waves.query(interval, collapse, ranges, diagnostics)?;
                json!({ "command": "query_interval", "samples": samples })
            }
            Command::GetSimulationStatus => {
                let status = match self.status {
                    Status::Paused => "paused",
                    Status::Running(_) => "running",
                    Status::Finished => "finished",
                };
                let mut reply = json!({
                    "command": "get_simulation_status", "status": status,
                    "latest_time": format_time(self.waves.latest_time()),
                });
                if matches!(self.status, Status::Paused) {
                    let next = self
                        .next_time(simulation)
                        .unwrap_or_else(|| self.time(simulation));
                    reply["next_sample_time"] = json!(format_time(next.min(MAX_TIME)));
                }
                reply
            }
            Command::RunSimulation {
                until_time,
                until_diagnostics,
                sample_item_values,
            } => {
                if !matches!(self.status, Status::Paused) {
                    return Err("the simulation is not paused");
                }
                self.status = Status::Running(Run {
                    until_time: until_time.as_deref().map(parse_time).transpose()?,
                    diagnostics: until_diagnostics,
                    sample_values: sample_item_values,
                });
                json!({ "command": "run_simulation" })
            }
            Command::PauseSimulation => {
                if matches!(self.status, Status::Running(_)) {
                    self.pause(simulation);
                }
                json!({ "command": "pause_simulation", "time": format_time(self.waves.latest_time()) })
            }
        })
    }
}

struct Connection {
    stream: TcpStream,
    buffer: Vec<u8>,
    scanned: usize,
    nonblocking: bool,
}

enum Incoming {
    Frame(Result<Request, serde_json::Error>),
    Idle,
    Closed,
}

impl Connection {
    fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        if self.nonblocking != nonblocking {
            self.stream.set_nonblocking(nonblocking)?;
            self.nonblocking = nonblocking;
        }
        Ok(())
    }

    fn receive(&mut self) -> io::Result<Incoming> {
        // Only scan newly appended bytes when a large request spans reads.
        loop {
            if let Some(end) = self.buffer[self.scanned..]
                .iter()
                .position(|byte| *byte == 0)
            {
                let end = self.scanned + end;
                let request = serde_json::from_slice(&self.buffer[..end]);
                self.buffer.drain(..=end);
                self.scanned = 0;
                return Ok(Incoming::Frame(request));
            }
            self.scanned = self.buffer.len();
            if self.scanned == MAX_REQUEST_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "CDSP request is too large",
                ));
            }
            let mut chunk = [0; 8192];
            let limit = chunk.len().min(MAX_REQUEST_BYTES - self.scanned);
            match self.stream.read(&mut chunk[..limit]) {
                Ok(0) => return Ok(Incoming::Closed),
                Ok(size) => self.buffer.extend_from_slice(&chunk[..size]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(Incoming::Idle);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn send(&mut self, response: &Value) -> io::Result<()> {
        self.set_nonblocking(false)?;
        let mut writer = io::BufWriter::new(&mut self.stream);
        serde_json::to_writer(&mut writer, response)?;
        writer.write_all(&[0])?;
        writer.flush()
    }
}

fn serve_session<B: SimBackend>(
    simulation: &mut Simulation<B>,
    session: &mut Session,
    listener: TcpListener,
    out: &mut impl Write,
) -> io::Result<()> {
    writeln!(
        io::stderr(),
        "CDSP listening at cxxrtl+tcp://{}",
        listener.local_addr()?
    )?;
    let (stream, _) = listener.accept()?;
    drop(listener);
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut connection = Connection {
        stream,
        buffer: Vec::new(),
        scanned: 0,
        nonblocking: false,
    };
    let mut greeted = false;
    loop {
        connection.set_nonblocking(matches!(session.status, Status::Running(_)))?;
        let response = match connection.receive()? {
            Incoming::Closed => return Ok(()),
            Incoming::Idle => {
                if let Some(event) = session.advance(simulation, out) {
                    connection.send(&event)?;
                }
                continue;
            }
            Incoming::Frame(Ok(Request::Greeting { version: 0 })) if !greeted => {
                greeted = true;
                Ok(json!({
                    "type": "greeting", "version": 0,
                    "commands": ["list_scopes", "list_items", "reference_items", "query_interval",
                                 "get_simulation_status", "run_simulation", "pause_simulation"],
                    "events": ["simulation_paused", "simulation_finished"],
                    "features": { "item_values_encoding": [ENCODING] },
                }))
            }
            Incoming::Frame(Ok(Request::Command { command })) if greeted => {
                session.command(command, simulation).map(|mut response| {
                    response["type"] = json!("response");
                    response
                })
            }
            Incoming::Frame(Ok(_)) => Err("expected one CDSP version 0 greeting before commands"),
            Incoming::Frame(Err(_)) => Err("invalid CDSP request"),
        };
        connection.send(&response.unwrap_or_else(
            |message| json!({ "type": "error", "error": "invalid_request", "message": message }),
        ))?;
    }
}

pub(super) fn serve<B: SimBackend>(
    simulation: &mut Simulation<B>,
    max_steps: u64,
    port: u16,
    out: &mut impl Write,
) -> Result<(), String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|error| format!("cannot listen for CDSP on 127.0.0.1:{port}: {error}"))?;
    let exponent = simulation.program().design.time_precision.exponent();
    let mut waves = Waveforms::default();
    waves.add_instance("", simulation.named_hierarchy());
    let mut session = Session {
        waves,
        references: BTreeMap::new(),
        status: Status::Paused,
        fs_per_tick: 10u128.pow((15 + exponent) as u32),
        output: SvOutput::default(),
        steps: 0,
        max_steps,
        failure: None,
    };
    session.initialize(simulation, out);
    let result = serve_session(simulation, &mut session, listener, out).or_else(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
        ) {
            Ok(())
        } else {
            Err(format!("CDSP connection failed: {error}"))
        }
    });
    simulation
        .flush_vcd()
        .map_err(|error| format!("waveform flush failed: {error}"))?;
    result?;
    if let Some(error) = session.failure {
        return Err(error);
    }
    if session.output.failed {
        return Err("SystemVerilog testbench reported an assertion failure or $error".into());
    }
    Ok(())
}
