//! What an SIR instruction defines, uses, reads, writes, and makes
//! observable.
//!
//! This module is the single classification of instructions. Analyses and
//! transformations ask these questions here instead of matching on
//! instruction variants, so an instruction's semantics are stated once and a
//! new instruction is classified in one place.

use crate::{RegisterId, SIRInstruction, SIROffset};

/// One state access whose address is known.
#[derive(Debug)]
pub struct MemoryAccess<'a, A> {
    pub addr: &'a A,
    pub offset: &'a SIROffset,
    pub width: usize,
}

impl<A> Clone for MemoryAccess<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A> Copy for MemoryAccess<'_, A> {}

/// All the simulation state an instruction reads, or all it writes: the one
/// question every analysis that keeps facts about state asks of an
/// instruction it does not handle itself. An instruction with
/// [`StateAccess::None`] leaves every such fact intact.
#[derive(Debug)]
pub enum StateAccess<'a, A> {
    /// No state.
    None,
    /// One access, of known bits.
    Access(MemoryAccess<'a, A>),
    /// Any part of these whole objects: the effects of a function call.
    Objects(&'a [A]),
    /// Any state: a native kernel, or a call that reaches one.
    Everything,
}

impl<A> Clone for StateAccess<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A> Copy for StateAccess<'_, A> {}

impl<A: PartialEq> StateAccess<'_, A> {
    /// Whether it may touch any part of the object `addr`.
    pub fn touches(&self, addr: &A) -> bool {
        match self {
            StateAccess::None => false,
            StateAccess::Access(access) => access.addr == addr,
            StateAccess::Objects(objects) => objects.contains(addr),
            StateAccess::Everything => true,
        }
    }

    /// Whether it may touch any of bits `start..start + width` of the object
    /// `addr`. An access at a dynamic offset may touch any bit of its object.
    pub fn overlaps(&self, addr: &A, start: usize, width: usize) -> bool {
        match self {
            StateAccess::Access(access) => {
                access.addr == addr
                    && access.offset.constant_bit_offset().is_none_or(|offset| {
                        offset < start.saturating_add(width)
                            && start < offset.saturating_add(access.width)
                    })
            }
            _ => self.touches(addr),
        }
    }
}

impl<A> StateAccess<'_, A> {
    /// Whether it touches no state.
    pub fn is_none(&self) -> bool {
        match self {
            StateAccess::None => true,
            StateAccess::Objects(objects) => objects.is_empty(),
            StateAccess::Access(_) | StateAccess::Everything => false,
        }
    }
}

impl SIROffset {
    /// Visit every register the offset reads, allowing it to be replaced.
    pub fn for_each_register_mut(&mut self, mut visit: impl FnMut(&mut RegisterId)) {
        match self {
            SIROffset::Static(_) | SIROffset::PackedElements { .. } => {}
            SIROffset::Dynamic(register) => visit(register),
            SIROffset::Element {
                index,
                dynamic_bit_offset,
                ..
            } => {
                visit(index);
                if let Some(register) = dynamic_bit_offset {
                    visit(register);
                }
            }
        }
    }
}

impl<A> SIRInstruction<A> {
    /// Visit every register the instruction uses, in operand order.
    pub fn for_each_use(&self, mut visit: impl FnMut(RegisterId)) {
        match self {
            SIRInstruction::Imm(..) => {}
            SIRInstruction::Binary(_, lhs, _, rhs) => {
                visit(*lhs);
                visit(*rhs);
            }
            SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
                visit(*source);
            }
            SIRInstruction::Load(_, _, offset, _) | SIRInstruction::Commit(_, _, offset, _, _) => {
                for register in offset.dynamic_registers().into_iter().flatten() {
                    visit(register);
                }
            }
            SIRInstruction::Store(_, offset, _, source, _, _) => {
                for register in offset.dynamic_registers().into_iter().flatten() {
                    visit(register);
                }
                visit(*source);
            }
            SIRInstruction::Concat(_, arguments)
            | SIRInstruction::RuntimeEvent {
                args: arguments, ..
            }
            | SIRInstruction::CombCaptureEvent {
                args: arguments, ..
            }
            | SIRInstruction::ExternCall {
                args: arguments, ..
            } => {
                for &argument in arguments {
                    visit(argument);
                }
            }
            SIRInstruction::Call { frame, args, .. } => {
                visit(*frame);
                for &(_, argument) in args {
                    visit(argument);
                }
            }
            SIRInstruction::Mux(_, condition, then_value, else_value) => {
                visit(*condition);
                visit(*then_value);
                visit(*else_value);
            }
            SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
                visit(*old);
                visit(*new);
            }
        }
    }

    /// Visit every register the instruction uses, allowing it to be replaced.
    /// The order matches [`SIRInstruction::for_each_use`].
    pub fn for_each_use_mut(&mut self, mut visit: impl FnMut(&mut RegisterId)) {
        match self {
            SIRInstruction::Imm(..) => {}
            SIRInstruction::Binary(_, lhs, _, rhs) => {
                visit(lhs);
                visit(rhs);
            }
            SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
                visit(source);
            }
            SIRInstruction::Load(_, _, offset, _) | SIRInstruction::Commit(_, _, offset, _, _) => {
                offset.for_each_register_mut(&mut visit);
            }
            SIRInstruction::Store(_, offset, _, source, _, _) => {
                offset.for_each_register_mut(&mut visit);
                visit(source);
            }
            SIRInstruction::Concat(_, arguments)
            | SIRInstruction::RuntimeEvent {
                args: arguments, ..
            }
            | SIRInstruction::CombCaptureEvent {
                args: arguments, ..
            }
            | SIRInstruction::ExternCall {
                args: arguments, ..
            } => {
                for argument in arguments {
                    visit(argument);
                }
            }
            SIRInstruction::Call { frame, args, .. } => {
                visit(frame);
                for (_, argument) in args {
                    visit(argument);
                }
            }
            SIRInstruction::Mux(_, condition, then_value, else_value) => {
                visit(condition);
                visit(then_value);
                visit(else_value);
            }
            SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
                visit(old);
                visit(new);
            }
        }
    }

    /// The register the instruction defines, if it produces a value.
    pub fn defined_register(&self) -> Option<RegisterId> {
        match self {
            SIRInstruction::Imm(dst, _)
            | SIRInstruction::Binary(dst, _, _, _)
            | SIRInstruction::Unary(dst, _, _)
            | SIRInstruction::Load(dst, _, _, _)
            | SIRInstruction::Concat(dst, _)
            | SIRInstruction::Slice(dst, _, _, _)
            | SIRInstruction::Mux(dst, _, _, _) => Some(*dst),
            SIRInstruction::ExternCall { dst, .. } | SIRInstruction::Call { dst, .. } => *dst,
            SIRInstruction::Store(..)
            | SIRInstruction::Commit(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
        }
    }

    /// The register the instruction defines, allowing it to be renamed.
    pub fn defined_register_mut(&mut self) -> Option<&mut RegisterId> {
        match self {
            SIRInstruction::Imm(dst, _)
            | SIRInstruction::Binary(dst, _, _, _)
            | SIRInstruction::Unary(dst, _, _)
            | SIRInstruction::Load(dst, _, _, _)
            | SIRInstruction::Concat(dst, _)
            | SIRInstruction::Slice(dst, _, _, _)
            | SIRInstruction::Mux(dst, _, _, _) => Some(dst),
            SIRInstruction::ExternCall { dst, .. } | SIRInstruction::Call { dst, .. } => {
                dst.as_mut()
            }
            SIRInstruction::Store(..)
            | SIRInstruction::Commit(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
        }
    }

    /// The simulation state the instruction reads.
    ///
    /// Runtime events and comb-capture instructions read only their register
    /// operands; what they write goes to host memory outside the simulation
    /// state. See [`SIRInstruction::is_host_interaction`].
    pub fn memory_read(&self) -> Option<MemoryAccess<'_, A>> {
        match self {
            SIRInstruction::Load(_, addr, offset, width)
            | SIRInstruction::Commit(addr, _, offset, width, _) => Some(MemoryAccess {
                addr,
                offset,
                width: *width,
            }),
            SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Store(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
            | SIRInstruction::ExternCall { .. }
            | SIRInstruction::Call { .. } => None,
        }
    }

    /// All the simulation state the instruction may read, a call's or a
    /// native kernel's included. An analysis that tracks state facts asks
    /// this (and [`Self::state_writes`]) of every instruction it does not
    /// model exactly, instead of assuming other instructions are pure.
    pub fn state_reads(&self) -> StateAccess<'_, A> {
        if self.accesses_unknown_state() {
            return StateAccess::Everything;
        }
        match (self.memory_read(), self.call_effects()) {
            (Some(access), _) => StateAccess::Access(access),
            (None, Some(effects)) => StateAccess::Objects(&effects.reads),
            (None, None) => StateAccess::None,
        }
    }

    /// All the simulation state the instruction may write (see
    /// [`Self::state_reads`]).
    pub fn state_writes(&self) -> StateAccess<'_, A> {
        if self.accesses_unknown_state() {
            return StateAccess::Everything;
        }
        match (self.memory_write(), self.call_effects()) {
            (Some(access), _) => StateAccess::Access(access),
            (None, Some(effects)) => StateAccess::Objects(&effects.writes),
            (None, None) => StateAccess::None,
        }
    }

    /// Whether the instruction reaches state other than through the one
    /// access of a Load, Store or Commit: a call whose effects read or write
    /// state, or a native kernel. A pass that models those three exactly asks
    /// this of every other instruction, and treats one that does as a barrier
    /// for the objects its [`Self::state_reads`] and [`Self::state_writes`]
    /// touch.
    ///
    /// A call is a barrier for those objects only: a store to another object
    /// may move across it. A call can end its caller with the callee's
    /// status, so the state left at such an Error exit is not ordered
    /// against stores to objects the call does not touch, which is the
    /// contract an [`crate::SIRTerminator::Error`] has for the objects the
    /// path to it does not touch.
    pub fn has_opaque_state_access(&self) -> bool {
        self.memory_read().is_none()
            && self.memory_write().is_none()
            && (!self.state_reads().is_none() || !self.state_writes().is_none())
    }

    /// The objects the instruction may read or write other than through the
    /// one access of a Load, Store or Commit (see
    /// [`Self::has_opaque_state_access`]), each whole; `None` when it may
    /// access any state. An object both read and written is listed twice.
    pub fn opaque_accessed_objects(&self) -> Option<impl Iterator<Item = &A>> {
        if self.accesses_unknown_state() {
            return None;
        }
        Some(
            self.call_effects()
                .into_iter()
                .flat_map(|effects| effects.reads.iter().chain(&effects.writes)),
        )
    }

    /// The objects the instruction may write other than through the one
    /// access of a Store or Commit, each whole; `None` when it may write any
    /// state.
    pub fn opaque_written_objects(&self) -> Option<impl Iterator<Item = &A>> {
        if self.accesses_unknown_state() {
            return None;
        }
        Some(
            self.call_effects()
                .into_iter()
                .flat_map(|effects| &effects.writes),
        )
    }

    /// The simulation state the instruction writes.
    pub fn memory_write(&self) -> Option<MemoryAccess<'_, A>> {
        match self {
            SIRInstruction::Store(addr, offset, width, _, _, _)
            | SIRInstruction::Commit(_, addr, offset, width, _) => Some(MemoryAccess {
                addr,
                offset,
                width: *width,
            }),
            SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
            | SIRInstruction::ExternCall { .. }
            | SIRInstruction::Call { .. } => None,
        }
    }

    /// Whether executing the instruction is visible outside the state it
    /// writes: it raises triggers, feeds comb captures, or interacts with the
    /// host.
    pub fn is_observable(&self) -> bool {
        match self {
            SIRInstruction::Store(_, _, _, _, triggers, comb_capture_sites) => {
                !triggers.is_empty() || !comb_capture_sites.is_empty()
            }
            SIRInstruction::Commit(_, _, _, _, triggers) => !triggers.is_empty(),
            _ => self.is_host_interaction(),
        }
    }

    /// Whether the instruction hands data or control to the host: it records
    /// a runtime event, updates comb-capture state outside the simulation
    /// state, or calls an external function. The host sees these in program
    /// order.
    pub fn is_host_interaction(&self) -> bool {
        match self {
            SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
            | SIRInstruction::ExternCall { .. } => true,
            SIRInstruction::Call { effects, .. } => {
                effects.host.interacts_with_host() || effects.kernel
            }
            SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Store(..)
            | SIRInstruction::Commit(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..) => false,
        }
    }

    /// What the instruction does outside the simulation state: the one
    /// classification that call summaries ([`crate::CallEffects::host`]) and
    /// the lane footprint both read, so they cannot disagree. A call answers
    /// for its callee; one that reaches a native kernel may use every
    /// resource. A native kernel's own state accesses are
    /// [`Self::state_reads`] and [`Self::state_writes`].
    pub fn host_effects(&self) -> crate::HostEffects {
        let none = crate::HostEffects::default();
        match self {
            SIRInstruction::Store(_, _, _, _, triggers, capture_sites) => crate::HostEffects {
                triggers: !triggers.is_empty(),
                capture_flags: !capture_sites.is_empty(),
                ..none
            },
            SIRInstruction::Commit(_, _, _, _, triggers) => crate::HostEffects {
                triggers: !triggers.is_empty(),
                ..none
            },
            SIRInstruction::RuntimeEvent { .. } => crate::HostEffects {
                runtime_events: true,
                ..none
            },
            SIRInstruction::CombCaptureEvent { .. } => crate::HostEffects {
                runtime_events: true,
                capture_flags: true,
                ..none
            },
            SIRInstruction::CombCaptureEnableIfChanged { .. } => crate::HostEffects {
                capture_flags: true,
                ..none
            },
            SIRInstruction::ExternCall { .. } => crate::HostEffects {
                extern_calls: true,
                ..none
            },
            SIRInstruction::Call { effects, .. } if effects.kernel => crate::HostEffects::ALL,
            SIRInstruction::Call { effects, .. } => effects.host,
            SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..) => none,
        }
    }

    /// What a call may do, the calls it makes included; `None` for any other
    /// instruction. Its state accesses are not among
    /// [`Self::memory_read`] and [`Self::memory_write`], which describe one
    /// access each.
    pub fn call_effects(&self) -> Option<&crate::CallEffects<A>> {
        match self {
            SIRInstruction::Call { effects, .. } => Some(effects),
            _ => None,
        }
    }

    /// Whether the instruction may read and write any simulation state: a
    /// native kernel call, or a call of a function that makes one. No state
    /// access may be moved across it, and no stored value is known after
    /// it. Any other call's state accesses are its [`Self::call_effects`].
    pub fn accesses_unknown_state(&self) -> bool {
        match self {
            SIRInstruction::Call { effects, .. } => effects.kernel,
            SIRInstruction::ExternCall { func, .. } => {
                crate::extern_abi::native_kernel_index(*func).is_some()
            }
            _ => false,
        }
    }

    /// Whether executing the instruction matters beyond the register it
    /// defines. Such an instruction is kept even when its result is unused,
    /// and is not duplicated, merged, or moved across other effects.
    pub fn has_side_effects(&self) -> bool {
        // A call can write state and end its caller with the callee's
        // status, whatever its effects.
        self.memory_write().is_some()
            || self.is_observable()
            || matches!(self, SIRInstruction::Call { .. })
    }

    /// Whether the instruction's result depends only on its operands: it
    /// neither reads nor writes state and has no observable effect.
    pub fn is_pure(&self) -> bool {
        self.memory_read().is_none() && !self.has_side_effects()
    }
}

impl crate::SIRTerminator {
    /// Visit every register the terminator uses, in operand order.
    pub fn for_each_use(&self, mut visit: impl FnMut(RegisterId)) {
        match self {
            crate::SIRTerminator::Jump(_, arguments) => {
                for &argument in arguments {
                    visit(argument);
                }
            }
            crate::SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                visit(*cond);
                for &argument in &true_block.1 {
                    visit(argument);
                }
                for &argument in &false_block.1 {
                    visit(argument);
                }
            }
            crate::SIRTerminator::Switch { selector, .. } => visit(*selector),
            crate::SIRTerminator::Return | crate::SIRTerminator::Error(_) => {}
        }
    }

    /// Visit every register the terminator uses, allowing it to be replaced.
    /// The order matches [`crate::SIRTerminator::for_each_use`].
    pub fn for_each_use_mut(&mut self, mut visit: impl FnMut(&mut RegisterId)) {
        match self {
            crate::SIRTerminator::Jump(_, arguments) => {
                for argument in arguments {
                    visit(argument);
                }
            }
            crate::SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                visit(cond);
                for argument in &mut true_block.1 {
                    visit(argument);
                }
                for argument in &mut false_block.1 {
                    visit(argument);
                }
            }
            crate::SIRTerminator::Switch { selector, .. } => visit(selector),
            crate::SIRTerminator::Return | crate::SIRTerminator::Error(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{FunctionId, RegisterId, SIRInstruction};

    #[test]
    fn a_call_uses_its_frame_and_arguments_and_defines_its_outputs() {
        let call = SIRInstruction::<u32>::Call {
            function: FunctionId(0),
            frame: RegisterId(1),
            args: vec![(0, RegisterId(2)), (2, RegisterId(3))],
            outputs: vec![1],
            dst: Some(RegisterId(4)),
            effects: std::sync::Arc::new(crate::CallEffects {
                writes: vec![7],
                host: crate::HostEffects {
                    extern_calls: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
        };
        let mut uses = Vec::new();
        call.for_each_use(|register| uses.push(register));
        assert_eq!(uses, [RegisterId(1), RegisterId(2), RegisterId(3)]);
        assert_eq!(call.defined_register(), Some(RegisterId(4)));
        // Its state accesses are its effects, not those of one access.
        assert!(call.memory_read().is_none() && call.memory_write().is_none());
        assert_eq!(call.call_effects().unwrap().writes, [7]);
        assert!(call.is_host_interaction());
        // Only a native kernel inside the callee makes them unknown.
        assert!(!call.accesses_unknown_state());
        // A call is kept whatever its effects: it can end its caller.
        assert!(call.has_side_effects() && !call.is_pure());
    }

    #[test]
    fn opaque_accesses_are_those_of_calls_and_native_kernels() {
        let call = |reads: Vec<u32>, writes: Vec<u32>| SIRInstruction::<u32>::Call {
            function: FunctionId(0),
            frame: RegisterId(0),
            args: Vec::new(),
            outputs: Vec::new(),
            dst: None,
            effects: std::sync::Arc::new(crate::CallEffects {
                reads,
                writes,
                ..Default::default()
            }),
        };
        let store = SIRInstruction::<u32>::Store(
            7,
            crate::SIROffset::Static(8),
            8,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        );
        // A Store's one access is exact, not opaque.
        assert!(!store.has_opaque_state_access());
        assert!(store.opaque_written_objects().unwrap().next().is_none());
        assert!(store.state_writes().overlaps(&7, 12, 8));
        assert!(!store.state_writes().overlaps(&7, 0, 8));
        // A call touching nothing is no barrier.
        assert!(!call(Vec::new(), Vec::new()).has_opaque_state_access());
        assert!(call(Vec::new(), Vec::new()).state_writes().is_none());
        let writer = call(vec![1], vec![2]);
        assert!(writer.has_opaque_state_access());
        assert_eq!(
            writer.opaque_written_objects().unwrap().collect::<Vec<_>>(),
            [&2]
        );
        assert_eq!(
            writer
                .opaque_accessed_objects()
                .unwrap()
                .collect::<Vec<_>>(),
            [&1, &2]
        );
        // A whole object overlaps any range of it.
        assert!(writer.state_writes().overlaps(&2, 100, 1));
        // A native kernel may touch anything.
        let kernel = SIRInstruction::<u32>::ExternCall {
            dst: None,
            func: crate::extern_abi::NATIVE_KERNEL_CALL_TAG,
            args: Vec::new(),
        };
        assert!(kernel.has_opaque_state_access());
        assert!(kernel.opaque_written_objects().is_none());
    }
}
