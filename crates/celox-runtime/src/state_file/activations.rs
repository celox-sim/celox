//! Version-three process identities and activation storage.
use super::*;

/// A static kernel by status path, or a file-local activation graph node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProcessRef {
    Kernel(String),
    Activation(u64),
}
impl From<String> for ProcessRef {
    fn from(value: String) -> Self {
        Self::Kernel(value)
    }
}
impl From<&str> for ProcessRef {
    fn from(value: &str) -> Self {
        Self::Kernel(value.into())
    }
}
impl std::fmt::Display for ProcessRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kernel(name) => name.fmt(f),
            Self::Activation(id) => write!(f, "activation {id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameValueKind {
    Bits,
    String,
    Object,
}

/// A value independent of backend layout. String elements hold file string IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameValueRecord {
    pub width: u64,
    pub is_4state: bool,
    pub kind: FrameValueKind,
    pub value: Vec<u8>,
    pub mask: Vec<u8>,
}

/// One execution of a named kernel. All numeric references are file-local IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRecord {
    pub id: u64,
    pub root: bool,
    pub kernel: String,
    pub parent: Option<u64>,
    pub live: bool,
    pub private: Vec<(String, FrameValueRecord)>,
    pub locals: Vec<(String, Option<u64>)>,
    pub pending: Vec<u64>,
    pub join: Option<(celox_design::ForkJoin, Vec<u64>)>,
}

pub(super) fn needs_v3(p: &ProcessRecord) -> bool {
    !p.activations.is_empty()
        || !p.cells.is_empty()
        || !p.reactive.is_empty()
        || !p.re_inactive.is_empty()
        || !p.re_nba_queue.is_empty()
        || p.wakeups
            .iter()
            .map(|(p, _)| p)
            .chain(p.waiters.iter().map(|w| &w.process))
            .chain(&p.ready)
            .chain(&p.inactive)
            .any(|p| matches!(p, ProcessRef::Activation(_)))
}
pub(super) fn put_process(w: &mut impl Write, p: &ProcessRef, version: u16) -> io::Result<()> {
    match p {
        ProcessRef::Kernel(name) => {
            if version >= 3 {
                w.write_all(&[0])?;
            }
            put_str(w, name)
        }
        ProcessRef::Activation(id) => {
            w.write_all(&[1])?;
            put_u64(w, *id)
        }
    }
}
pub(super) fn get_process(r: &mut impl Read, version: u16) -> Result<ProcessRef, StateFileError> {
    if version < 3 || !get_flag(r)? {
        Ok(ProcessRef::Kernel(get_str(r)?))
    } else {
        Ok(ProcessRef::Activation(get_u64(r)?))
    }
}
pub(super) fn put_process_list(
    w: &mut impl Write,
    list: &[ProcessRef],
    version: u16,
) -> io::Result<()> {
    put_len(w, list.len())?;
    for p in list {
        put_process(w, p, version)?;
    }
    Ok(())
}
pub(super) fn get_process_list(
    r: &mut impl Read,
    version: u16,
) -> Result<Vec<ProcessRef>, StateFileError> {
    (0..get_u32(r)?).map(|_| get_process(r, version)).collect()
}
fn put_ids(w: &mut impl Write, ids: &[u64]) -> io::Result<()> {
    put_len(w, ids.len())?;
    for &id in ids {
        put_u64(w, id)?;
    }
    Ok(())
}
fn get_ids(r: &mut impl Read) -> Result<Vec<u64>, StateFileError> {
    (0..get_u32(r)?).map(|_| Ok(get_u64(r)?)).collect()
}
fn put_optional_id(w: &mut impl Write, id: Option<u64>) -> io::Result<()> {
    w.write_all(&[id.is_some() as u8])?;
    if let Some(id) = id {
        put_u64(w, id)?;
    }
    Ok(())
}
fn get_optional_id(r: &mut impl Read) -> Result<Option<u64>, StateFileError> {
    Ok(if get_flag(r)? {
        Some(get_u64(r)?)
    } else {
        None
    })
}
fn put_value(w: &mut impl Write, value: &FrameValueRecord) -> io::Result<()> {
    put_u64(w, value.width)?;
    w.write_all(&[
        value.is_4state as u8,
        match value.kind {
            FrameValueKind::Bits => 0,
            FrameValueKind::String => 1,
            FrameValueKind::Object => 2,
        },
    ])?;
    put_bytes(w, &value.value)?;
    put_bytes(w, &value.mask)
}
fn get_value(r: &mut impl Read) -> Result<FrameValueRecord, StateFileError> {
    let width = get_u64(r)?;
    let is_4state = get_flag(r)?;
    let mut tag = [0];
    r.read_exact(&mut tag)?;
    let kind = match tag[0] {
        0 => FrameValueKind::Bits,
        1 => FrameValueKind::String,
        2 => FrameValueKind::Object,
        _ => return Err(StateFileError::Malformed("frame value kind".into())),
    };
    Ok(FrameValueRecord {
        width,
        is_4state,
        kind,
        value: get_len_bytes(r)?,
        mask: get_len_bytes(r)?,
    })
}
pub(super) fn put_extension(w: &mut impl Write, p: &ProcessRecord) -> io::Result<()> {
    put_len(w, p.activations.len())?;
    for a in &p.activations {
        put_u64(w, a.id)?;
        w.write_all(&[a.root as u8])?;
        put_str(w, &a.kernel)?;
        put_optional_id(w, a.parent)?;
        w.write_all(&[a.live as u8])?;
        put_len(w, a.private.len())?;
        for (name, value) in &a.private {
            put_str(w, name)?;
            put_value(w, value)?;
        }
        put_len(w, a.locals.len())?;
        for (name, cell) in &a.locals {
            put_str(w, name)?;
            put_optional_id(w, *cell)?;
        }
        put_ids(w, &a.pending)?;
        w.write_all(&[a.join.is_some() as u8])?;
        if let Some((kind, ids)) = &a.join {
            w.write_all(&[match kind {
                celox_design::ForkJoin::All => 0,
                celox_design::ForkJoin::Any => 1,
                celox_design::ForkJoin::None => 2,
            }])?;
            put_ids(w, ids)?;
        }
    }
    put_len(w, p.cells.len())?;
    for cell in &p.cells {
        put_value(w, cell)?;
    }
    put_process_list(w, &p.reactive, 3)?;
    put_process_list(w, &p.re_inactive, 3)?;
    put_len(w, p.re_nba_queue.len())?;
    for update in &p.re_nba_queue {
        put_str(w, &update.target)?;
        put_u64(w, update.lsb)?;
        put_u64(w, update.width)?;
        put_bytes(w, &update.value)?;
        put_bytes(w, &update.mask)?;
    }
    Ok(())
}
pub(super) fn get_extension(
    r: &mut impl Read,
    p: &mut ProcessRecord,
) -> Result<(), StateFileError> {
    p.activations = (0..get_u32(r)?)
        .map(|_| {
            let id = get_u64(r)?;
            let root = get_flag(r)?;
            let kernel = get_str(r)?;
            let parent = get_optional_id(r)?;
            let live = get_flag(r)?;
            let private = (0..get_u32(r)?)
                .map(|_| Ok((get_str(r)?, get_value(r)?)))
                .collect::<Result<_, StateFileError>>()?;
            let locals = (0..get_u32(r)?)
                .map(|_| Ok((get_str(r)?, get_optional_id(r)?)))
                .collect::<Result<_, StateFileError>>()?;
            let pending = get_ids(r)?;
            let join = if get_flag(r)? {
                let mut tag = [0];
                r.read_exact(&mut tag)?;
                let kind = match tag[0] {
                    0 => celox_design::ForkJoin::All,
                    1 => celox_design::ForkJoin::Any,
                    2 => celox_design::ForkJoin::None,
                    _ => return Err(StateFileError::Malformed("fork join kind".into())),
                };
                Some((kind, get_ids(r)?))
            } else {
                None
            };
            Ok(ActivationRecord {
                id,
                root,
                kernel,
                parent,
                live,
                private,
                locals,
                pending,
                join,
            })
        })
        .collect::<Result<_, StateFileError>>()?;
    p.cells = (0..get_u32(r)?)
        .map(|_| get_value(r))
        .collect::<Result<_, _>>()?;
    p.reactive = get_process_list(r, 3)?;
    p.re_inactive = get_process_list(r, 3)?;
    p.re_nba_queue = (0..get_u32(r)?)
        .map(|_| {
            Ok(NbaRecord {
                target: get_str(r)?,
                lsb: get_u64(r)?,
                width: get_u64(r)?,
                value: get_len_bytes(r)?,
                mask: get_len_bytes(r)?,
            })
        })
        .collect::<Result<_, StateFileError>>()?;
    Ok(())
}
