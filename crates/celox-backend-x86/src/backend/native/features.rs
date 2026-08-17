//! Host x86-64 feature facts shared by MIR optimization, allocation, and emission.

/// Machine encoding selected for register-register shifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VariableShiftEncoding {
    /// BMI2 three-operand shifts accept the count in any general-purpose register.
    Bmi2,
    /// Baseline x86-64 shifts require the count in CL (the low byte of RCX).
    LegacyCl,
}

/// State/arena base selected for one native function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateBaseStrategy {
    /// Reserve R15 and use ordinary base+displacement addressing.
    R15,
    /// Borrow the FS base while generated code is running.
    #[cfg_attr(
        not(any(all(target_os = "windows", target_arch = "x86_64"), test)),
        expect(dead_code, reason = "constructed by Windows x86-64 target detection")
    )]
    Fs,
    /// Borrow the GS base while generated code is running.
    #[cfg_attr(
        not(any(all(target_os = "linux", target_arch = "x86_64"), test)),
        expect(
            dead_code,
            reason = "constructed by Linux x86-64 target detection and emitter tests"
        )
    )]
    Gs,
}

/// Immutable target facts captured once when an MIR function is created.
///
/// Register constraints and emission must consult the same value: allocating a
/// free shift-count operand and later emitting a CL shift would be a silent
/// miscompile, while constraining BMI2 shifts to RCX creates unnecessary Perm
/// boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct X86Features {
    bmi2: bool,
    avx: bool,
    state_base: StateBaseStrategy,
}

impl X86Features {
    pub(crate) fn detect() -> Self {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let bmi2 = std::arch::is_x86_feature_detected!("bmi2");
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        let bmi2 = false;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let avx = std::arch::is_x86_feature_detected!("avx");
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        let avx = false;

        Self {
            bmi2,
            avx,
            state_base: detect_state_base_strategy(),
        }
    }

    pub(crate) const fn bmi2(self) -> bool {
        self.bmi2
    }

    pub(crate) const fn avx(self) -> bool {
        self.avx
    }

    pub(crate) const fn variable_shift_encoding(self) -> VariableShiftEncoding {
        if self.bmi2 {
            VariableShiftEncoding::Bmi2
        } else {
            VariableShiftEncoding::LegacyCl
        }
    }

    /// Whether user-mode RDFSBASE/RDGSBASE/WRFSBASE/WRGSBASE instructions are
    /// both implemented by the CPU and enabled by the operating system.
    pub(crate) const fn state_base(self) -> StateBaseStrategy {
        self.state_base
    }

    /// R15 is reserved as the state base on hosts where GS-base instructions
    /// cannot be executed.
    pub(crate) const fn allocatable_register_count(self) -> usize {
        match self.state_base {
            StateBaseStrategy::Fs | StateBaseStrategy::Gs => 15,
            StateBaseStrategy::R15 => 14,
        }
    }

    #[cfg(test)]
    pub(crate) const fn for_test(bmi2: bool) -> Self {
        Self {
            bmi2,
            avx: false,
            state_base: StateBaseStrategy::R15,
        }
    }

    #[cfg(test)]
    pub(crate) const fn for_test_with_state_base(
        bmi2: bool,
        state_base: StateBaseStrategy,
    ) -> Self {
        Self {
            bmi2,
            avx: false,
            state_base,
        }
    }
}

/// Feature bits required by machine code emitted on this host.
pub fn detected_image_feature_bits() -> u8 {
    image_feature_bits(X86Features::detect())
}

fn image_feature_bits(features: X86Features) -> u8 {
    const BMI2: u8 = 1 << 0;
    const AVX: u8 = 1 << 1;
    const FS_STATE_BASE: u8 = 1 << 2;
    const GS_STATE_BASE: u8 = 1 << 3;

    let mut bits = 0;
    if features.bmi2() {
        bits |= BMI2;
    }
    if features.avx() {
        bits |= AVX;
    }
    match features.state_base() {
        StateBaseStrategy::Fs => bits |= FS_STATE_BASE,
        StateBaseStrategy::Gs => bits |= GS_STATE_BASE,
        StateBaseStrategy::R15 => {}
    }
    bits
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn detect_state_base_strategy() -> StateBaseStrategy {
    // CPUID alone is insufficient: older kernels can leave CR4.FSGSBASE
    // disabled even when the processor implements the instructions. Linux
    // advertises user-mode availability through AT_HWCAP2.
    const HWCAP2_FSGSBASE: libc::c_ulong = 1 << 1;
    let cpu_supports_fsgsbase = std::arch::x86_64::__cpuid_count(7, 0).ebx & 1 != 0;
    if cpu_supports_fsgsbase
        // SAFETY: getauxval reads the immutable ELF auxiliary vector and has
        // no pointer preconditions.
        && unsafe { libc::getauxval(libc::AT_HWCAP2) } & HWCAP2_FSGSBASE != 0
    {
        StateBaseStrategy::Gs
    } else {
        StateBaseStrategy::R15
    }
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn detect_state_base_strategy() -> StateBaseStrategy {
    const PF_RDWRFSGSBASE_AVAILABLE: u32 = 22;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn IsProcessorFeaturePresent(processor_feature: u32) -> i32;
    }

    // Windows x64 owns GS for the TEB. Keep it intact and borrow FS only when
    // the OS explicitly reports the base instructions as executable.
    // SAFETY: IsProcessorFeaturePresent accepts every u32 feature identifier
    // and has no pointer arguments.
    if unsafe { IsProcessorFeaturePresent(PF_RDWRFSGSBASE_AVAILABLE) } != 0 {
        StateBaseStrategy::Fs
    } else {
        StateBaseStrategy::R15
    }
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64")
)))]
const fn detect_state_base_strategy() -> StateBaseStrategy {
    StateBaseStrategy::R15
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_features_distinguish_fs_and_gs_state_bases() {
        let fs = image_feature_bits(X86Features::for_test_with_state_base(
            false,
            StateBaseStrategy::Fs,
        ));
        let gs = image_feature_bits(X86Features::for_test_with_state_base(
            false,
            StateBaseStrategy::Gs,
        ));
        let r15 = image_feature_bits(X86Features::for_test_with_state_base(
            false,
            StateBaseStrategy::R15,
        ));

        assert_ne!(fs, gs);
        assert_ne!(fs, r15);
        assert_ne!(gs, r15);
    }
}
