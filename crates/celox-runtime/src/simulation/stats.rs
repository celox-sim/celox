//! Which loop runs the time slots of a timed simulation, and what it ran.

/// Which loop runs the time slots of a timed simulation. A test option.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SchedulerMode {
    /// The runtime's loop runs every time slot.
    #[default]
    Reference,
    /// The design's compiled scheduler runs the time slots it can, and the
    /// runtime's loop the others. A design without a compiled scheduler
    /// runs as in [`Self::Reference`].
    Compiled,
    /// [`Self::Compiled`], checked after every time slot against a shadow
    /// simulation of the same design in [`Self::Reference`].
    Differential,
}

/// How one process kernel is scheduled.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KernelSchedule {
    /// The compiled scheduler runs it.
    Compiled,
    /// The runtime's loop runs it, for the reason given.
    Reference(&'static str),
}

/// The loop that runs time slots and how each process kernel is scheduled.
#[doc(hidden)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchedulerStats {
    pub mode: SchedulerMode,
    /// Time slots the compiled scheduler ran.
    pub compiled_slots: u64,
    /// Steps checked against the shadow simulation in
    /// [`SchedulerMode::Differential`]. Steps of the reference loop are not
    /// counted otherwise, which would cost every step.
    pub checked_steps: u64,
    /// How each process kernel is scheduled, by process index.
    pub kernels: Vec<KernelSchedule>,
}
