//! Layout-independent state files.
//!
//! A state file records the value of every state object by its hierarchical
//! path instead of by memory offset. It can therefore be loaded into a
//! simulator built with a different backend, optimization level or memory
//! layout, as long as the design declares the same state.
//!
//! The encoding is little-endian binary:
//!
//! ```text
//! magic "CXSTATE\0" | version u16 | flags u16 (finished in version 4)
//! object count u32, then per object:
//!   path str | width u64 | role u8 | is_4state u8
//!   value [ceil(width/8) bytes] | mask [same size, only for four-state objects
//!   in a four-state file]
//! schedule (only with FLAG_SCHEDULE):
//!   time u64
//!   clock count u32, then per clock: event str | period u64
//!   event count u32, then per event: scheduled event
//!   periodic count u32, then per marker: scheduled event | count u64
//!   high count u32, then per event: event str
//!   captured count u32, then event str (FLAG_CAPTURED_EDGES)
//!   unknown-presence u8 (version 3), then unknown count and event str
//!   (version 2 without captured edges always includes unknown baselines)
//! scheduled event: time u64 | event str | signal str | value u8
//! str: byte length u32 | UTF-8
//! ```

use std::io::{self, Read, Write};

const MAGIC: &[u8; 8] = b"CXSTATE\0";
const VERSION: u16 = 4;
const FLAG_FOUR_STATE: u16 = 1 << 0;
const FLAG_SCHEDULE: u16 = 1 << 1;
const FLAG_CAPTURED_EDGES: u16 = 1 << 2;
const FLAG_FINISHED: u16 = 1 << 3;

/// How a state object gets its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateRole {
    /// Held across evaluations: registers, memories and inputs.
    State,
    /// Written by combinational logic and recomputed from the other state.
    Comb,
}

/// The saved value of one state object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateObject {
    pub path: String,
    pub width: usize,
    pub role: StateRole,
    pub is_4state: bool,
    /// `ceil(width / 8)` little-endian bytes.
    pub value: Vec<u8>,
    /// X/Z mask with the layout of `value`, present for four-state objects
    /// of a four-state simulation.
    pub mask: Option<Vec<u8>>,
}

/// A pending event of a time-based simulation, identified by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledEvent {
    pub time: u64,
    /// Path of the event (clock or reset) domain.
    pub event: String,
    /// Path of the signal the event drives.
    pub signal: String,
    pub value: u8,
}

/// Scheduling state of a time-based simulation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScheduleRecord {
    pub time: u64,
    /// The design has requested simulation termination.
    pub finished: bool,
    /// Periodic clocks as (event path, period).
    pub clocks: Vec<(String, u64)>,
    /// Pending events, in no particular order.
    pub events: Vec<ScheduledEvent>,
    /// Pending events that a periodic clock re-schedules when they fire, with
    /// the number of identical such events.
    pub periodic: Vec<(ScheduledEvent, u64)>,
    /// Event paths whose signal was high when edge detection last sampled it.
    pub high_events: Vec<String>,
    /// FF occurrences captured by a host evaluation.
    pub captured_edges: Vec<String>,
    /// Events whose baseline contains X/Z bits. Legacy version-1 files do
    /// not carry this information; their baselines are reconstructed on load.
    pub unknown_events: Option<Vec<String>>,
}

/// Contents of a state file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StateFile {
    pub four_state: bool,
    pub objects: Vec<StateObject>,
    pub schedule: Option<ScheduleRecord>,
}

/// A state file that could not be read.
#[derive(Debug, thiserror::Error)]
pub enum StateFileError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("not a Celox state file")]
    BadMagic,
    #[error("unsupported state file version {0} (this build reads version {VERSION})")]
    UnsupportedVersion(u16),
    #[error("malformed state file: {0}")]
    Malformed(String),
}

/// A state object whose saved values differ between two files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateDifference {
    /// Present in both files with a different width or value.
    Changed {
        path: String,
        left: StateObject,
        right: StateObject,
    },
    /// Present only in the left file.
    OnlyLeft(StateObject),
    /// Present only in the right file.
    OnlyRight(StateObject),
}

impl StateFile {
    pub fn write_to(&self, mut writer: impl Write) -> io::Result<()> {
        let w = &mut writer;
        w.write_all(MAGIC)?;
        let captured = self
            .schedule
            .as_ref()
            .is_some_and(|s| !s.captured_edges.is_empty());
        let finished = self
            .schedule
            .as_ref()
            .is_some_and(|schedule| schedule.finished);
        let version = if finished {
            4
        } else if captured {
            3
        } else if self
            .schedule
            .as_ref()
            .is_none_or(|s| s.unknown_events.is_none())
        {
            1
        } else {
            2
        };
        put_u16(w, version)?;
        let mut flags = 0;
        if self.four_state {
            flags |= FLAG_FOUR_STATE;
        }
        if self.schedule.is_some() {
            flags |= FLAG_SCHEDULE;
        }
        if captured {
            flags |= FLAG_CAPTURED_EDGES;
        }
        if finished {
            flags |= FLAG_FINISHED;
        }
        put_u16(w, flags)?;
        put_len(w, self.objects.len())?;
        for object in &self.objects {
            put_str(w, &object.path)?;
            put_u64(w, object.width as u64)?;
            w.write_all(&[
                match object.role {
                    StateRole::State => 0,
                    StateRole::Comb => 1,
                },
                object.is_4state as u8,
            ])?;
            let size = object.width.div_ceil(8);
            assert_eq!(object.value.len(), size, "value size of `{}`", object.path);
            w.write_all(&object.value)?;
            if self.four_state && object.is_4state {
                let mask = object.mask.as_deref().unwrap_or_default();
                assert_eq!(mask.len(), size, "mask size of `{}`", object.path);
                w.write_all(mask)?;
            }
        }
        if let Some(schedule) = &self.schedule {
            put_u64(w, schedule.time)?;
            put_len(w, schedule.clocks.len())?;
            for (event, period) in &schedule.clocks {
                put_str(w, event)?;
                put_u64(w, *period)?;
            }
            put_len(w, schedule.events.len())?;
            for event in &schedule.events {
                put_event(w, event)?;
            }
            put_len(w, schedule.periodic.len())?;
            for (event, count) in &schedule.periodic {
                put_event(w, event)?;
                put_u64(w, *count)?;
            }
            put_len(w, schedule.high_events.len())?;
            for event in &schedule.high_events {
                put_str(w, event)?;
            }
            if captured {
                put_len(w, schedule.captured_edges.len())?;
                for event in &schedule.captured_edges {
                    put_str(w, event)?;
                }
            }
            if version >= 3 {
                // Version 3 preserves the distinction between legacy unknown
                // baselines and an explicitly known baseline.
                w.write_all(&[u8::from(schedule.unknown_events.is_some())])?;
            }
            if let Some(unknown_events) = &schedule.unknown_events {
                put_len(w, unknown_events.len())?;
                for event in unknown_events {
                    put_str(w, event)?;
                }
            }
        }
        Ok(())
    }

    pub fn read_from(mut reader: impl Read) -> Result<Self, StateFileError> {
        let r = &mut reader;
        let mut magic = [0; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(StateFileError::BadMagic);
        }
        let version = get_u16(r)?;
        if !(1..=VERSION).contains(&version) {
            return Err(StateFileError::UnsupportedVersion(version));
        }
        let flags = get_u16(r)?;
        if flags & FLAG_CAPTURED_EDGES != 0 && version < 2 {
            return Err(StateFileError::Malformed(
                "captured edges require version 2".into(),
            ));
        }
        if flags & FLAG_FINISHED != 0 && (version < 4 || flags & FLAG_SCHEDULE == 0) {
            return Err(StateFileError::Malformed(
                "finished state requires a version-4 schedule".into(),
            ));
        }
        let four_state = flags & FLAG_FOUR_STATE != 0;
        let count = get_u32(r)?;
        let mut objects = Vec::new();
        for _ in 0..count {
            let path = get_str(r)?;
            let width = usize::try_from(get_u64(r)?)
                .map_err(|_| StateFileError::Malformed(format!("width of `{path}`")))?;
            let mut tags = [0; 2];
            r.read_exact(&mut tags)?;
            let role = match tags[0] {
                0 => StateRole::State,
                1 => StateRole::Comb,
                other => {
                    return Err(StateFileError::Malformed(format!(
                        "role {other} of `{path}`"
                    )));
                }
            };
            let is_4state = tags[1] != 0;
            let size = width.div_ceil(8);
            let value = get_bytes(r, size)?;
            let mask = if four_state && is_4state {
                Some(get_bytes(r, size)?)
            } else {
                None
            };
            objects.push(StateObject {
                path,
                width,
                role,
                is_4state,
                value,
                mask,
            });
        }
        let schedule = if flags & FLAG_SCHEDULE != 0 {
            let time = get_u64(r)?;
            let clocks = (0..get_u32(r)?)
                .map(|_| Ok((get_str(r)?, get_u64(r)?)))
                .collect::<Result<_, StateFileError>>()?;
            let events = (0..get_u32(r)?)
                .map(|_| get_event(r))
                .collect::<Result<_, _>>()?;
            let periodic = (0..get_u32(r)?)
                .map(|_| Ok((get_event(r)?, get_u64(r)?)))
                .collect::<Result<_, StateFileError>>()?;
            let high_events = (0..get_u32(r)?)
                .map(|_| get_str(r))
                .collect::<Result<_, _>>()?;
            let captured_edges = if flags & FLAG_CAPTURED_EDGES != 0 {
                (0..get_u32(r)?)
                    .map(|_| get_str(r))
                    .collect::<Result<_, _>>()?
            } else {
                Vec::new()
            };
            let unknown_present = if version >= 3 {
                let mut tag = [0];
                r.read_exact(&mut tag)?;
                match tag[0] {
                    0 => false,
                    1 => true,
                    _ => {
                        return Err(StateFileError::Malformed(
                            "unknown baseline presence".into(),
                        ));
                    }
                }
            } else {
                version >= 2 && flags & FLAG_CAPTURED_EDGES == 0
            };
            let unknown_events = if unknown_present {
                Some(
                    (0..get_u32(r)?)
                        .map(|_| get_str(r))
                        .collect::<Result<_, _>>()?,
                )
            } else {
                None
            };
            Some(ScheduleRecord {
                finished: flags & FLAG_FINISHED != 0,
                captured_edges,
                unknown_events,
                time,
                clocks,
                events,
                periodic,
                high_events,
            })
        } else {
            None
        };
        let mut rest = [0; 1];
        if r.read(&mut rest)? != 0 {
            return Err(StateFileError::Malformed(
                "trailing bytes after the last section".into(),
            ));
        }
        Ok(Self {
            four_state,
            objects,
            schedule,
        })
    }

    /// Objects whose saved values differ between `self` and `other`, by
    /// path. Combinational objects are recomputed from the other state, so
    /// they are compared only with `include_comb`; an object counts as
    /// combinational if either file says so, which hides values that
    /// optimizations left stale.
    pub fn diff(&self, other: &Self, include_comb: bool) -> Vec<StateDifference> {
        use std::collections::BTreeMap;

        let left: BTreeMap<_, _> = self.objects.iter().map(|o| (&o.path, o)).collect();
        let right: BTreeMap<_, _> = other.objects.iter().map(|o| (&o.path, o)).collect();
        let compared = |object: &StateObject| include_comb || object.role == StateRole::State;
        let mut differences = Vec::new();
        for (path, l) in &left {
            match right.get(path) {
                Some(r) => {
                    if !(compared(l) && compared(r)) {
                        continue;
                    }
                    let lmask = l.mask.as_deref().unwrap_or_default();
                    let rmask = r.mask.as_deref().unwrap_or_default();
                    if l.width != r.width || l.value != r.value || !masks_equal(lmask, rmask) {
                        differences.push(StateDifference::Changed {
                            path: (*path).clone(),
                            left: (*l).clone(),
                            right: (*r).clone(),
                        });
                    }
                }
                None if compared(l) => differences.push(StateDifference::OnlyLeft((*l).clone())),
                None => {}
            }
        }
        for (path, r) in &right {
            if !left.contains_key(path) && compared(r) {
                differences.push(StateDifference::OnlyRight((*r).clone()));
            }
        }
        differences
    }
}

/// A missing mask means all bits are known.
fn masks_equal(left: &[u8], right: &[u8]) -> bool {
    let len = left.len().max(right.len());
    (0..len).all(|i| left.get(i).copied().unwrap_or(0) == right.get(i).copied().unwrap_or(0))
}

/// Format little-endian bytes as a hexadecimal number of `width` bits, with
/// X/Z bits from `mask` shown as `x`.
pub fn format_state_value(value: &[u8], mask: Option<&[u8]>, width: usize) -> String {
    let digits = width.div_ceil(4).max(1);
    (0..digits)
        .rev()
        .map(|digit| {
            let nibble = |bytes: &[u8]| {
                let byte = bytes.get(digit / 2).copied().unwrap_or(0);
                (byte >> ((digit % 2) * 4)) & 0xf
            };
            if mask.is_some_and(|mask| nibble(mask) != 0) {
                'x'
            } else {
                char::from_digit(u32::from(nibble(value)), 16).unwrap()
            }
        })
        .collect()
}

fn put_u16(w: &mut impl Write, value: u16) -> io::Result<()> {
    w.write_all(&value.to_le_bytes())
}

fn put_u64(w: &mut impl Write, value: u64) -> io::Result<()> {
    w.write_all(&value.to_le_bytes())
}

fn put_len(w: &mut impl Write, len: usize) -> io::Result<()> {
    let len = u32::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many entries"))?;
    w.write_all(&len.to_le_bytes())
}

fn put_str(w: &mut impl Write, value: &str) -> io::Result<()> {
    put_len(w, value.len())?;
    w.write_all(value.as_bytes())
}

fn put_event(w: &mut impl Write, event: &ScheduledEvent) -> io::Result<()> {
    put_u64(w, event.time)?;
    put_str(w, &event.event)?;
    put_str(w, &event.signal)?;
    w.write_all(&[event.value])
}

fn get_u16(r: &mut impl Read) -> io::Result<u16> {
    let mut bytes = [0; 2];
    r.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn get_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    r.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn get_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    r.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn get_bytes(r: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    // Read through `take` so a corrupt length fails at end of input instead
    // of allocating the claimed size up front.
    let mut bytes = Vec::new();
    r.take(len as u64).read_to_end(&mut bytes)?;
    if bytes.len() != len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(bytes)
}

fn get_str(r: &mut impl Read) -> Result<String, StateFileError> {
    let len = get_u32(r)? as usize;
    String::from_utf8(get_bytes(r, len)?)
        .map_err(|_| StateFileError::Malformed("path is not UTF-8".into()))
}

fn get_event(r: &mut impl Read) -> Result<ScheduledEvent, StateFileError> {
    let time = get_u64(r)?;
    let event = get_str(r)?;
    let signal = get_str(r)?;
    let mut value = [0; 1];
    r.read_exact(&mut value)?;
    Ok(ScheduledEvent {
        time,
        event,
        signal,
        value: value[0],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(path: &str, role: StateRole, value: u8) -> StateObject {
        StateObject {
            path: path.into(),
            width: 8,
            role,
            is_4state: true,
            value: vec![value],
            mask: Some(vec![0]),
        }
    }

    fn sample() -> StateFile {
        let event = ScheduledEvent {
            time: 15,
            event: "clk".into(),
            signal: "clk".into(),
            value: 1,
        };
        StateFile {
            four_state: true,
            objects: vec![
                object("q", StateRole::State, 3),
                StateObject {
                    path: "wide".into(),
                    width: 70,
                    role: StateRole::State,
                    is_4state: false,
                    value: (0..9).collect(),
                    mask: None,
                },
                object("y", StateRole::Comb, 4),
            ],
            schedule: Some(ScheduleRecord {
                time: 10,
                finished: false,
                clocks: vec![("clk".into(), 10)],
                events: vec![event.clone()],
                periodic: vec![(event, 1)],
                high_events: vec!["clk".into()],
                captured_edges: Vec::new(),
                unknown_events: Some(vec!["clk".into()]),
            }),
        }
    }

    #[test]
    fn untimed_states_keep_the_version_one_encoding() {
        let mut file = sample();
        file.schedule = None;
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).unwrap();
        assert_eq!(&bytes[8..10], &1u16.to_le_bytes());
        assert_eq!(StateFile::read_from(bytes.as_slice()).unwrap(), file);
    }

    #[test]
    fn finished_schedules_preserve_optional_baselines() {
        for unknown in [None, Some(vec![])] {
            for captured in [vec![], vec!["clk".into()]] {
                let mut file = sample();
                let schedule = file.schedule.as_mut().unwrap();
                schedule.finished = true;
                schedule.unknown_events = unknown.clone();
                schedule.captured_edges = captured;
                let mut bytes = Vec::new();
                file.write_to(&mut bytes).unwrap();
                assert_eq!(u16::from_le_bytes(bytes[8..10].try_into().unwrap()), 4);
                assert_eq!(StateFile::read_from(bytes.as_slice()).unwrap(), file);
            }
        }
    }

    #[test]
    fn legacy_schedule_round_trips_preserve_missing_mask_metadata() {
        let mut file = sample();
        file.schedule.as_mut().unwrap().unknown_events = None;
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).unwrap();
        assert_eq!(&bytes[8..10], &1u16.to_le_bytes());
        let loaded = StateFile::read_from(bytes.as_slice()).unwrap();
        assert_eq!(loaded, file);
        let mut again = Vec::new();
        loaded.write_to(&mut again).unwrap();
        assert_eq!(again, bytes);
    }

    #[test]
    fn captured_edges_preserve_legacy_and_explicit_unknown_baselines() {
        for unknown in [None, Some(vec!["clk".into()])] {
            let mut file = sample();
            let schedule = file.schedule.as_mut().unwrap();
            schedule.captured_edges = vec!["clk".into()];
            schedule.unknown_events = unknown;
            let mut bytes = Vec::new();
            file.write_to(&mut bytes).unwrap();
            assert_eq!(StateFile::read_from(bytes.as_slice()).unwrap(), file);
            if file.schedule.as_ref().unwrap().unknown_events.is_none() {
                // #1158's version-2 captured-edge encoding has no unknown marker.
                assert_eq!(bytes.pop(), Some(0));
                bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
                assert_eq!(StateFile::read_from(bytes.as_slice()).unwrap(), file);
            }
        }
    }

    #[test]
    fn round_trips() {
        let file = sample();
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).unwrap();
        assert_eq!(StateFile::read_from(bytes.as_slice()).unwrap(), file);
    }

    #[test]
    fn rejects_foreign_and_truncated_input() {
        assert!(matches!(
            StateFile::read_from(&b"not a state file"[..]),
            Err(StateFileError::BadMagic)
        ));
        let mut bytes = Vec::new();
        sample().write_to(&mut bytes).unwrap();
        for len in [10, bytes.len() - 1] {
            assert!(StateFile::read_from(&bytes[..len]).is_err());
        }
        bytes.push(0);
        assert!(matches!(
            StateFile::read_from(bytes.as_slice()),
            Err(StateFileError::Malformed(_))
        ));
    }

    #[test]
    fn diff_ignores_combinational_objects_unless_asked() {
        let left = sample();
        let mut right = sample();
        right.objects[0].value = vec![5];
        right.objects[2].value = vec![9];
        let paths = |differences: Vec<StateDifference>| -> Vec<String> {
            differences
                .into_iter()
                .map(|difference| match difference {
                    StateDifference::Changed { path, .. } => path,
                    StateDifference::OnlyLeft(o) | StateDifference::OnlyRight(o) => o.path,
                })
                .collect()
        };
        assert_eq!(paths(left.diff(&right, false)), ["q"]);
        assert_eq!(paths(left.diff(&right, true)), ["q", "y"]);

        // A stale value under a state role on one side is still hidden when
        // the other side knows the object is combinational.
        right.objects[2].role = StateRole::State;
        assert_eq!(paths(left.diff(&right, false)), ["q"]);
    }

    #[test]
    fn formats_values_with_unknown_bits() {
        assert_eq!(format_state_value(&[0x3c, 0x01], None, 12), "13c");
        assert_eq!(format_state_value(&[0x3c], Some(&[0xf0]), 8), "xc");
    }
}
