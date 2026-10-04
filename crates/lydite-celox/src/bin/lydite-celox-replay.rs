//! Concrete source-RTL execution only. This process receives no expected values.
use celox::{BigUint, OptLevel, Simulator};
use celox_design::{DomainKind, PortTypeKind};
use celox_runtime::SignalDirection;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read},
    path::Path,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| format!("missing {key}").into())
}
fn run(v: &Value) -> Result<Value> {
    if v["version"] != 1 || v["design"]["four_state"] != false {
        return Err("only replay v1 and two-state designs supported".into());
    }
    let d = &v["design"];
    let sources = d["sources"]
        .as_array()
        .ok_or("missing sources")?
        .iter()
        .map(|s| Ok((text(s, "text")?, Path::new(text(s, "path")?))))
        .collect::<Result<Vec<_>>>()?;
    let mut sim = Simulator::from_sources(sources, text(d, "top")?)
        .four_state(false)
        .opt_level(OptLevel::O0)
        .build_native()?;
    let event_name = text(v, "event")?;
    let event = sim.try_event(event_name)?;
    let code = sim.backend_ref().shared_code();
    let reflection = code.program_image().reflection();
    let mut ports = BTreeMap::new();
    let mut clocks = 0;
    for s in reflection.signals() {
        if matches!(
            s.type_kind,
            PortTypeKind::ResetAsyncHigh
                | PortTypeKind::ResetAsyncLow
                | PortTypeKind::ResetSyncHigh
                | PortTypeKind::ResetSyncLow
        ) || matches!(
            s.domain_kind,
            DomainKind::ResetAsyncHigh | DomainKind::ResetAsyncLow
        ) || s.direction == SignalDirection::Inout
        {
            return Err("typed reset/inout unsupported; use explicit synchronous bit reset".into());
        }
        if matches!(s.type_kind, PortTypeKind::Clock) {
            clocks += 1;
            if s.name != event_name || s.domain_kind != DomainKind::ClockPosedge {
                return Err("only one positive-edge clock supported".into());
            }
        }
        let top = reflection
            .scope(s.parent)
            .ok_or("missing scope")?
            .parent
            .is_none();
        if top && s.direction == SignalDirection::Input && s.name != event_name {
            if s.signal.width == 0 || s.signal.width > 64 || s.signal.array_layout.is_some() {
                return Err("only scalar 1..64-bit inputs supported".into());
            }
            ports.insert(s.name.clone(), s.signal);
        }
    }
    if clocks != 1 {
        return Err("exactly one clock required".into());
    }
    let reset = text(v, "reset_input")?;
    if ports.get(reset).is_none_or(|s| s.width != 1) {
        return Err("reset must be an external one-bit input".into());
    }
    let active = v["reset_active"]
        .as_u64()
        .filter(|n| *n <= 1)
        .ok_or("invalid reset polarity")?;
    let observe = v["observe"]
        .as_array()
        .ok_or("missing observe")?
        .iter()
        .map(|n| {
            let name = n.as_str().ok_or("invalid observation")?;
            Ok((name.to_owned(), sim.try_signal(name)?))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let frames = v["frames"]
        .as_array()
        .filter(|f| !f.is_empty() && f.len() <= 65)
        .ok_or("trace must have 1..65 frames")?;
    let mut trace = vec![];
    for (edge, frame) in frames.iter().enumerate() {
        let input = frame.as_object().ok_or("invalid frame")?;
        if input.keys().collect::<BTreeSet<_>>() != ports.keys().collect() {
            return Err("frame must drive every external nonclock input exactly once; state writes forbidden".into());
        }
        if input[reset].as_u64() != Some(if edge == 0 { active } else { 1 - active }) {
            return Err("trace reset indexing mismatch".into());
        }
        for (name, signal) in &ports {
            let value = input[name]
                .as_u64()
                .ok_or("input must be an unsigned integer")?;
            if signal.width < 64 && value >= (1u64 << signal.width) {
                return Err("input exceeds signal width".into());
            }
            sim.set_wide(*signal, BigUint::from(value));
        }
        sim.eval_comb()?;
        let mut before = BTreeMap::new();
        for (name, signal) in &observe {
            let (value, mask) = sim.get_four_state(*signal);
            if mask != BigUint::default() {
                return Err("unknown simulation value".into());
            }
            before.insert(name, value.to_string());
        }
        sim.tick(event)?;
        let mut after = BTreeMap::new();
        for (name, signal) in &observe {
            let (value, mask) = sim.get_four_state(*signal);
            if mask != BigUint::default() {
                return Err("unknown simulation value".into());
            }
            after.insert(name, value.to_string());
        }
        trace.push(json!({"edge":edge,"before":before,"after":after}));
    }
    Ok(
        json!({"version":1,"status":"simulated","backend":"celox-native-o0","celox_version":env!("CARGO_PKG_VERSION"),"trace":trace}),
    )
}
fn main() {
    let result = (|| -> Result<Value> {
        let mut s = String::new();
        io::stdin()
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut s)?;
        if s.len() > 16 * 1024 * 1024 {
            return Err("request too large".into());
        }
        run(&serde_json::from_str(&s)?)
    })();
    match result {
        Ok(v) => println!("{v}"),
        Err(e) => {
            println!(
                "{}",
                json!({"status":"simulation_error","error":e.to_string()})
            );
            std::process::exit(2);
        }
    }
}
