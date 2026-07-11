//! Host x86-64 feature facts shared by MIR optimization, allocation, and emission.

/// Machine encoding selected for register-register shifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VariableShiftEncoding {
    /// BMI2 three-operand shifts accept the count in any general-purpose register.
    Bmi2,
    /// Baseline x86-64 shifts require the count in CL (the low byte of RCX).
    LegacyCl,
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
}

impl X86Features {
    pub(crate) fn detect() -> Self {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let bmi2 = std::arch::is_x86_feature_detected!("bmi2");
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        let bmi2 = false;

        Self { bmi2 }
    }

    pub(crate) const fn bmi2(self) -> bool {
        self.bmi2
    }

    pub(crate) const fn variable_shift_encoding(self) -> VariableShiftEncoding {
        if self.bmi2 {
            VariableShiftEncoding::Bmi2
        } else {
            VariableShiftEncoding::LegacyCl
        }
    }

    #[cfg(test)]
    pub(crate) const fn for_test(bmi2: bool) -> Self {
        Self { bmi2 }
    }
}
