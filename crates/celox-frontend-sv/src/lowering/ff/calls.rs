//! Out-of-line calls (see [`super::super::out_of_line`]): the call sites,
//! and the lowering of a subroutine body into its function unit.

mod call_sites;
mod forks;
mod suspend;
mod unit_events;
mod units;

use super::super::out_of_line::{
    Function, Outcome, Place, Reason, Slot, automatic_names, frame_addr, function_key, layout,
};
use super::*;

pub(super) use unit_events::*;

/// The function whose unit an [`Ff`] is lowering.
pub(super) struct FrameContext {
    /// Its index among the module's functions.
    function: usize,
    /// The variables of the slots that hold the frame's base and the call
    /// depth of the activation.
    base: SourceVarId,
    depth: SourceVarId,
    /// The variables of the slots of the calling process's state that the
    /// body uses (see [`super::super::out_of_line::ProcessState`]): the RNG
    /// state and the name of the process seed, and the seed of `$random`.
    pub(super) random: Option<((SourceVarId, String), String)>,
    pub(super) dist_seed: Option<(SourceVarId, String)>,
    /// For a resumable unit (see [`Function::resume`]), its resume slots.
    resume: Option<ResumeFrame>,
    /// Registers holding a frame size not known yet (see
    /// [`Function::size_patches`]).
    size_patches: Vec<(RegisterId, usize)>,
    /// The `ref` formals the body names as variables of the module (see
    /// [`Place::Variable`]), each with the variable.
    renames: Vec<(String, SourceVarId)>,
    /// The `ref` formals bound to a property or an element, whose handle
    /// or positions are slots of the frame.
    aliases: Vec<super::references::Alias>,
    /// The first variable the lowering of the unit created: those from it
    /// on that the unit touches are slots of its frame.
    start: SourceVarId,
    /// Whether its automatic unpacked arrays stay module variables (see
    /// `ProcModule::single_activation`); otherwise they are slots.
    single: bool,
    /// The variable of the omitted arguments of the call and the positions
    /// of the formals whose defaults the unit evaluates (see
    /// [`Function::omitted`]).
    omitted: Option<(String, Vec<usize>)>,
}

/// The resume slots of a resumable unit being lowered, and its resume
/// points.
struct ResumeFrame {
    resume: SourceVarId,
    status: SourceVarId,
    request: SourceVarId,
    top: SourceVarId,
    /// The block each resume point continues in; point 0 is the start.
    blocks: Vec<BlockId>,
    /// The module variables through which its host requests pass their
    /// values: the host reads and writes them while the unit is suspended,
    /// which only its innermost activation can be, and the unit copies them
    /// from and into its frame right before and after.
    staging: HashSet<SourceVarId>,
    /// The calls at which it can be suspended inside the callee.
    calls: Vec<celox_design::CallResume>,
}

/// A variable that a fork in a unit shares between the parent and the
/// children, which run in copies of the unit's frames (see
/// [`Ff::fork_in_unit`]): a box, a heap object of one field, holds it, and
/// the frame holds the box's handle, which the copies share. A box is made
/// each time the variable's declaration runs, or at the entry for a formal,
/// as a process's automatic is a new cell each time (IEEE 1800-2023 6.21,
/// 9.3.2). The heap collector finds the handle in the frames of the parent
/// and of every child, so the box lives until the last of them that holds
/// it ends or leaves the call.
#[derive(Debug, Clone)]
pub(super) struct SharedBox {
    pub(super) var: String,
    handle: String,
    field: sv::ir::ObjectField,
}
