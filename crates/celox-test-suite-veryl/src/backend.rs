use crate::{BigUint, Result, Scalar};
use std::cell::RefCell;
use std::path::PathBuf;

/// An in-memory Veryl compilation unit.
#[derive(Clone, Debug)]
pub struct Source {
    pub text: String,
    pub path: PathBuf,
}

/// Input to a compiler adapter. Generic `clock` and `reset` ports use Veryl's
/// defaults (positive edge, asynchronous active-low reset).
#[derive(Clone, Debug)]
pub struct Design {
    pub sources: Vec<Source>,
    pub top: String,
    pub four_state: bool,
}

impl Design {
    pub fn new(text: &str, top: &str) -> Self {
        Self {
            sources: vec![Source {
                text: text.into(),
                path: "test.veryl".into(),
            }],
            top: top.into(),
            four_state: false,
        }
    }

    pub fn four_state(mut self, enabled: bool) -> Self {
        self.four_state = enabled;
        self
    }
}

/// An instance in a hierarchical signal path. `None` identifies an ordinary
/// instance; `Some(index)` identifies an array element, including `Some(0)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub name: String,
    pub index: Option<usize>,
}

/// A signal name relative to an instance path. Empty `instances` means the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalPath {
    pub instances: Vec<Instance>,
    pub name: String,
}

/// Minimal adapter contract. Writes are staged without evaluating combinational
/// logic. `eval_comb` settles pending writes; `tick` executes the named event
/// once and settles its consequences. Reads must observe settled state.
///
/// Values are arbitrary-width unsigned bit patterns. Four-state values use
/// `(payload, mask)` with 0=(0,0), 1=(1,0), X=(1,1), Z=(0,1) for each bit.
/// Adapters translate other encodings at this boundary. Two-state adapters
/// return a zero mask. Signal widths come from the compiled design; truncate
/// writes and zero-extend values as necessary.
///
/// Construct a fresh simulator for each design. Two-state storage starts at
/// zero; four-state storage starts at X. Initial combinational outputs must be
/// readable even when no inputs have been driven.
pub trait Backend {
    fn write(&mut self, signal: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()>;
    fn read(&mut self, signal: &SignalPath) -> Result<(BigUint, BigUint)>;
    fn eval_comb(&mut self) -> Result<()>;
    fn tick(&mut self, event: &str) -> Result<()>;
}

/// Compiler callback used by every test. Capturing closures allow the caller to
/// select compiler flags, backends, or process connections.
pub type Factory<'a> = dyn FnMut(&Design) -> Result<Box<dyn Backend>> + 'a;

/// Opaque handle local to one test simulator.
#[derive(Clone, Copy, Debug)]
pub struct Signal(usize);

/// Opaque event handle local to one test simulator.
#[derive(Clone, Copy, Debug)]
pub struct Event(usize);

/// Backend-independent test driver. It batches writes in `modify`, handles
/// scalar conversion, and keeps all test stimuli independent of native APIs.
pub struct Simulator {
    backend: Box<dyn Backend>,
    signals: RefCell<Vec<SignalPath>>,
    events: RefCell<Vec<String>>,
}

impl Simulator {
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self {
            backend,
            signals: RefCell::default(),
            events: RefCell::default(),
        }
    }

    pub fn signal(&self, name: &str) -> Signal {
        self.child_signal(&[], name)
    }

    pub fn child_signal(&self, instances: &[(&str, Option<usize>)], name: &str) -> Signal {
        let path = SignalPath {
            instances: instances
                .iter()
                .map(|(name, index)| Instance {
                    name: (*name).into(),
                    index: *index,
                })
                .collect(),
            name: name.into(),
        };
        let mut signals = self.signals.borrow_mut();
        if let Some(index) = signals.iter().position(|signal| signal == &path) {
            return Signal(index);
        }
        let signal = Signal(signals.len());
        signals.push(path);
        signal
    }

    pub fn event(&self, name: &str) -> Event {
        let mut events = self.events.borrow_mut();
        let event = Event(events.len());
        events.push(name.into());
        event
    }

    pub fn set<T: Scalar>(&mut self, signal: Signal, value: T) {
        self.set_wide(signal, value.to_bits());
    }

    pub fn set_wide(&mut self, signal: Signal, value: BigUint) {
        self.set_four_state(signal, value, BigUint::default());
    }

    pub fn set_four_state(&mut self, signal: Signal, payload: BigUint, mask: BigUint) {
        let signals = self.signals.borrow();
        let path = &signals[signal.0];
        self.backend
            .write(path, payload, mask)
            .unwrap_or_else(|error| panic!("write {path:?}: {error}"));
    }

    pub fn get(&mut self, signal: Signal) -> BigUint {
        self.get_four_state(signal).0
    }

    pub fn get_as<T: Scalar>(&mut self, signal: Signal) -> T {
        T::from_bits(&self.get(signal))
    }

    pub fn get_four_state(&mut self, signal: Signal) -> (BigUint, BigUint) {
        let signals = self.signals.borrow();
        let path = &signals[signal.0];
        self.backend
            .read(path)
            .unwrap_or_else(|error| panic!("read {path:?}: {error}"))
    }

    pub fn modify(&mut self, write: impl FnOnce(&mut Self)) -> Result<()> {
        write(self);
        self.eval_comb()
    }

    pub fn eval_comb(&mut self) -> Result<()> {
        self.backend.eval_comb()
    }

    pub fn tick(&mut self, event: Event) -> Result<()> {
        self.backend.tick(&self.events.borrow()[event.0])
    }
}
