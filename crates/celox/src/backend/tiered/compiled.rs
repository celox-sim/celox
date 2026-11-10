//! The compiled code of a tier and how the tiered backend runs it.

use super::*;

/// The adopted compiled tier.
pub(super) enum CompiledTier {
    #[cfg(feature = "cranelift")]
    Jit(Box<JitBackend>),
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    Native(Box<crate::backend::native::NativeBackend>),
}

impl CompiledCode {
    /// Image size in `u64` words the compiled tier requires its host
    /// allocation to cover.
    pub(super) fn required_image_words(&self) -> usize {
        match self {
            // A MemorySpilled Cranelift plan extends the layout with backend
            // scratch beyond the semantic state, and adoption resizes the
            // transferred image to the compiled layout's total size; report
            // that requirement so promotion stays inside the reservation.
            #[cfg(feature = "cranelift")]
            CompiledCode::Cranelift(shared) => shared.layout.merged_total_size.div_ceil(8),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            CompiledCode::Native(shared) => shared.native_memory_size.div_ceil(8) + 1,
        }
    }
}

impl CompiledTier {
    /// Bind compiled code to a live simulation state handed over by the
    /// interpreter during promotion.
    ///
    /// Callers must have verified [`CompiledCode::required_image_words`]
    /// against the image's spare capacity first: growth stays within the
    /// existing allocation so promotion never moves the live memory image,
    /// which external views (zero-copy host buffers) may still reference.
    pub(super) fn adopt(
        code: CompiledCode,
        mut memory: MemoryImage,
        runtime_event_buffer: Arc<RuntimeEventBuffer>,
        comb_capture_enabled: Vec<u8>,
        heap: Box<celox_heap::Heap>,
    ) -> Self {
        debug_assert!(memory.capacity_words() >= code.required_image_words());
        let semantic_words = match &code {
            #[cfg(feature = "cranelift")]
            CompiledCode::Cranelift(shared) => shared.layout.merged_total_size.div_ceil(8),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            CompiledCode::Native(shared) => shared.layout().merged_total_size.div_ceil(8),
        };
        if memory.len_words() > semantic_words {
            memory.as_mut_slice()[semantic_words..].fill(0);
        }
        memory.resize_zeroed_within_capacity(code.required_image_words());
        match code {
            #[cfg(feature = "cranelift")]
            CompiledCode::Cranelift(shared) => {
                Self::Jit(Box::new(JitBackend::adopt_shared_with_state(
                    shared,
                    memory,
                    runtime_event_buffer,
                    comb_capture_enabled,
                    heap,
                )))
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            CompiledCode::Native(shared) => Self::Native(Box::new(
                crate::backend::native::NativeBackend::adopt_shared_with_state(
                    shared,
                    memory,
                    runtime_event_buffer,
                    comb_capture_enabled,
                    heap,
                ),
            )),
        }
    }

    pub(super) fn layout(&self) -> &MemoryLayout {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => jit.layout(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => native.layout(),
        }
    }

    pub(super) fn memory_base_mut(&mut self) -> *mut u8 {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => jit.memory_as_ptr().0.cast_mut(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => native.memory_as_ptr().0.cast_mut(),
        }
    }

    /// Logical capacity of the live image in `u64` words.
    pub(super) fn memory_word_capacity(&self) -> usize {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => {
                let (_, size_bytes) = jit.memory_as_ptr();
                size_bytes.div_ceil(8)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => native.memory_word_capacity(),
        }
    }

    /// Replace a native tier's shared code in place, keeping the live image.
    /// Returns false when the tier is not native; the caller keeps the
    /// previous code in that case.
    pub(super) fn replace_native_code(&mut self, code: CompiledCode) -> bool {
        #[cfg(any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        ))]
        #[allow(irrefutable_let_patterns)]
        if let CompiledCode::Native(shared) = code {
            return match self {
                Self::Native(native) => {
                    native.replace_shared_code(shared);
                    true
                }
                #[cfg(feature = "cranelift")]
                Self::Jit(_) => false,
            };
        }
        #[cfg(not(any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        )))]
        {
            let _ = code;
        }
        false
    }

    pub(super) fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => jit.eval_comb(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => native.eval_comb(),
        }
    }

    /// Resolve the tier's own event handle for a shared trigger id. Every
    /// backend indexes its event table by the same deterministic id space,
    /// so translation is an array lookup.
    pub(super) fn eval_apply_ff_at(&mut self, id: usize) -> Result<(), SimulatorErrorCode> {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => {
                let event = jit.id_to_event_slice()[id];
                jit.eval_apply_ff_at(event)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                let event = native.id_to_event_slice()[id];
                native.eval_apply_ff_at(event)
            }
        }
    }

    pub(super) fn eval_comb_apply_ff_at(&mut self, id: usize) -> Result<(), SimulatorErrorCode> {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => {
                let event = jit.id_to_event_slice()[id];
                jit.eval_comb_apply_ff_at(event)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                let event = native.id_to_event_slice()[id];
                native.eval_comb_apply_ff_at(event)
            }
        }
    }

    /// Execute up to `count` fused ticks. The Cranelift tier has no
    /// in-generated-code batch loop, so it completes one tick per call and
    /// lets the caller re-poll; the native tier runs the whole batch inside
    /// generated code when `native_tick_loop` is enabled.
    pub(super) fn eval_comb_apply_ff_many_at(
        &mut self,
        id: usize,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => (1, {
                let event = jit.id_to_event_slice()[id];
                jit.eval_comb_apply_ff_at(event)
            }),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                native.eval_comb_apply_ff_many_at(native.id_to_event_slice()[id], count)
            }
        }
    }

    /// Resolve the phase-specific evaluate-only event for a shared trigger
    /// address and execute it.
    ///
    /// Evaluate-only and apply functions live in their own per-phase id
    /// spaces, so the combined `id_to_event_slice` entry must not be used
    /// here; the address resolves through each tier's phase-specific map.
    pub(super) fn eval_only_ff_at_addr(
        &mut self,
        addr: &AbsoluteAddr,
    ) -> Result<(), SimulatorErrorCode> {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => {
                let event = jit
                    .resolve_eval_only_event(addr)
                    .unwrap_or_else(|| panic!("eval-only event not found for {addr:?}"));
                jit.eval_only_ff_at(event)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                let event = native
                    .resolve_eval_only_event(addr)
                    .unwrap_or_else(|| panic!("eval-only event not found for {addr:?}"));
                native.eval_only_ff_at(event)
            }
        }
    }

    pub(super) fn run_process(&mut self, index: usize) -> Result<(), SimulatorErrorCode> {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => jit.run_process(index),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => native.run_process(index),
        }
    }

    /// Resolve the phase-specific apply event for a shared trigger address
    /// and execute it.
    pub(super) fn apply_ff_at_addr(
        &mut self,
        addr: &AbsoluteAddr,
    ) -> Result<(), SimulatorErrorCode> {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Jit(jit) => {
                let event = jit
                    .resolve_apply_event(addr)
                    .unwrap_or_else(|| panic!("apply event not found for {addr:?}"));
                jit.apply_ff_at(event)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                let event = native
                    .resolve_apply_event(addr)
                    .unwrap_or_else(|| panic!("apply event not found for {addr:?}"));
                native.apply_ff_at(event)
            }
        }
    }
}

/// Compiled code produced by the background worker, before it is bound to a
/// live simulation state at promotion time.
pub(crate) enum CompiledCode {
    #[cfg(feature = "cranelift")]
    Cranelift(Arc<SharedJitCode>),
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    Native(Arc<SharedNativeCode>),
}
