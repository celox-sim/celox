//! Bootstrap AArch64 backend.
//!
//! The initial pipeline deliberately covers a small, executable subset:
//! parameters, 64-bit immediates, integer addition, and return. It establishes
//! the target boundary and exercises the shared register allocator before SIR
//! lowering and spill support are added.

use celox_backend_common::regalloc::{
    Allocation, LinearScanError, LiveRange, MachineRegister, allocate_linear_scan,
};
use std::collections::HashMap;
use std::fmt;

/// Virtual general-purpose register in bootstrap ARM64 MIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VReg(u32);

/// AArch64 general-purpose register number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Arm64Reg(u8);

impl Arm64Reg {
    pub const X0: Self = Self(0);

    pub const fn number(self) -> u8 {
        self.0
    }
}

impl MachineRegister for Arm64Reg {
    fn index(self) -> u8 {
        self.0
    }
}

const ALLOCATABLE_REGS: [Arm64Reg; 7] = [
    Arm64Reg(9),
    Arm64Reg(10),
    Arm64Reg(11),
    Arm64Reg(12),
    Arm64Reg(13),
    Arm64Reg(14),
    Arm64Reg(15),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inst {
    Parameter { dst: VReg, index: u8 },
    LoadImm { dst: VReg, value: u64 },
    Add { dst: VReg, lhs: VReg, rhs: VReg },
    Return { value: VReg },
}

impl Inst {
    fn def(self) -> Option<VReg> {
        match self {
            Self::Parameter { dst, .. } | Self::LoadImm { dst, .. } | Self::Add { dst, .. } => {
                Some(dst)
            }
            Self::Return { .. } => None,
        }
    }

    fn uses(self) -> [Option<VReg>; 2] {
        match self {
            Self::Parameter { .. } | Self::LoadImm { .. } => [None, None],
            Self::Add { lhs, rhs, .. } => [Some(lhs), Some(rhs)],
            Self::Return { value } => [Some(value), None],
        }
    }
}

/// Verified bootstrap machine function.
#[derive(Debug, Clone)]
pub struct Function {
    instructions: Vec<Inst>,
    vreg_count: u32,
}

impl Function {
    pub fn builder() -> FunctionBuilder {
        FunctionBuilder::default()
    }
}

/// Builder which assigns every definition a fresh SSA identity.
#[derive(Debug, Default)]
pub struct FunctionBuilder {
    instructions: Vec<Inst>,
    next_vreg: u32,
}

impl FunctionBuilder {
    fn alloc(&mut self) -> VReg {
        let value = VReg(self.next_vreg);
        self.next_vreg = self
            .next_vreg
            .checked_add(1)
            .expect("bootstrap ARM64 VReg namespace exhausted");
        value
    }

    pub fn parameter(&mut self, index: u8) -> VReg {
        let dst = self.alloc();
        self.instructions.push(Inst::Parameter { dst, index });
        dst
    }

    pub fn load_imm(&mut self, value: u64) -> VReg {
        let dst = self.alloc();
        self.instructions.push(Inst::LoadImm { dst, value });
        dst
    }

    pub fn add(&mut self, lhs: VReg, rhs: VReg) -> VReg {
        let dst = self.alloc();
        self.instructions.push(Inst::Add { dst, lhs, rhs });
        dst
    }

    pub fn return_value(&mut self, value: VReg) {
        self.instructions.push(Inst::Return { value });
    }

    pub fn finish(self) -> Function {
        Function {
            instructions: self.instructions,
            vreg_count: self.next_vreg,
        }
    }
}

/// Failure from bootstrap MIR verification or allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    InvalidMir(String),
    Regalloc(LinearScanError<VReg>),
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMir(message) => write!(formatter, "invalid ARM64 MIR: {message}"),
            Self::Regalloc(error) => write!(formatter, "ARM64 register allocation failed: {error}"),
        }
    }
}

impl std::error::Error for CompileError {}

impl From<LinearScanError<VReg>> for CompileError {
    fn from(error: LinearScanError<VReg>) -> Self {
        Self::Regalloc(error)
    }
}

/// Compiled AArch64 function and its physical assignment.
#[derive(Debug, Clone)]
pub struct CompiledFunction {
    code: Vec<u8>,
    allocation: Allocation<VReg, Arm64Reg>,
}

impl CompiledFunction {
    pub fn code(&self) -> &[u8] {
        &self.code
    }

    pub fn assigned_register(&self, value: VReg) -> Option<Arm64Reg> {
        self.allocation.get(value)
    }
}

/// Verify, allocate, and emit one bootstrap AArch64 function.
pub fn compile(function: &Function) -> Result<CompiledFunction, CompileError> {
    verify(function)?;
    let ranges = live_ranges(function)?;
    let allocation = allocate_linear_scan(&ranges, &ALLOCATABLE_REGS)?;
    let code = emit(function, &allocation);
    Ok(CompiledFunction { code, allocation })
}

fn verify(function: &Function) -> Result<(), CompileError> {
    let mut defined = vec![false; function.vreg_count as usize];
    let mut saw_return = false;
    for (index, instruction) in function.instructions.iter().copied().enumerate() {
        if saw_return {
            return Err(CompileError::InvalidMir(format!(
                "instruction {index} follows return"
            )));
        }
        if let Inst::Parameter {
            index: parameter, ..
        } = instruction
            && parameter >= 8
        {
            return Err(CompileError::InvalidMir(format!(
                "parameter {parameter} is outside the AAPCS64 register argument set"
            )));
        }
        for used in instruction.uses().into_iter().flatten() {
            let Some(is_defined) = defined.get(used.0 as usize) else {
                return Err(CompileError::InvalidMir(format!(
                    "instruction {index} uses unallocated {used:?}"
                )));
            };
            if !is_defined {
                return Err(CompileError::InvalidMir(format!(
                    "instruction {index} uses {used:?} before its definition"
                )));
            }
        }
        if let Some(dst) = instruction.def() {
            let Some(slot) = defined.get_mut(dst.0 as usize) else {
                return Err(CompileError::InvalidMir(format!(
                    "instruction {index} defines unallocated {dst:?}"
                )));
            };
            if std::mem::replace(slot, true) {
                return Err(CompileError::InvalidMir(format!(
                    "instruction {index} redefines {dst:?}"
                )));
            }
        }
        saw_return = matches!(instruction, Inst::Return { .. });
    }
    if !saw_return {
        return Err(CompileError::InvalidMir("function has no return".into()));
    }
    if defined.into_iter().any(|is_defined| !is_defined) {
        return Err(CompileError::InvalidMir(
            "allocated virtual register has no definition".into(),
        ));
    }
    Ok(())
}

fn live_ranges(function: &Function) -> Result<Vec<LiveRange<VReg>>, CompileError> {
    let mut ranges = HashMap::<VReg, LiveRange<VReg>>::new();
    for (index, instruction) in function.instructions.iter().copied().enumerate() {
        let index = u32::try_from(index)
            .map_err(|_| CompileError::InvalidMir("too many instructions".into()))?;
        let use_point = index
            .checked_mul(2)
            .ok_or_else(|| CompileError::InvalidMir("program point overflow".into()))?;
        let def_point = use_point
            .checked_add(1)
            .ok_or_else(|| CompileError::InvalidMir("program point overflow".into()))?;
        for used in instruction.uses().into_iter().flatten() {
            let range = ranges.get_mut(&used).ok_or_else(|| {
                CompileError::InvalidMir(format!("missing live range for {used:?}"))
            })?;
            range.end = use_point;
        }
        if let Some(dst) = instruction.def() {
            ranges.insert(
                dst,
                LiveRange {
                    value: dst,
                    start: def_point,
                    end: def_point,
                },
            );
        }
    }
    let mut result = ranges.into_values().collect::<Vec<_>>();
    result.sort_unstable_by_key(|range| range.value);
    Ok(result)
}

fn emit(function: &Function, allocation: &Allocation<VReg, Arm64Reg>) -> Vec<u8> {
    let mut code = Vec::new();
    for instruction in &function.instructions {
        match *instruction {
            Inst::Parameter { dst, index } => {
                let destination = assigned(allocation, dst);
                push_mov_reg(&mut code, destination, Arm64Reg(index));
            }
            Inst::LoadImm { dst, value } => {
                push_load_imm(&mut code, assigned(allocation, dst), value);
            }
            Inst::Add { dst, lhs, rhs } => push_word(
                &mut code,
                encode_add(
                    assigned(allocation, dst),
                    assigned(allocation, lhs),
                    assigned(allocation, rhs),
                ),
            ),
            Inst::Return { value } => {
                push_mov_reg(&mut code, Arm64Reg::X0, assigned(allocation, value));
                push_word(&mut code, 0xd65f_03c0);
            }
        }
    }
    code
}

fn assigned(allocation: &Allocation<VReg, Arm64Reg>, value: VReg) -> Arm64Reg {
    allocation
        .get(value)
        .expect("verified ARM64 MIR value must have an assignment")
}

fn push_mov_reg(code: &mut Vec<u8>, destination: Arm64Reg, source: Arm64Reg) {
    if destination != source {
        push_word(code, encode_mov_reg(destination, source));
    }
}

fn push_load_imm(code: &mut Vec<u8>, destination: Arm64Reg, value: u64) {
    let halves = [
        value as u16,
        (value >> 16) as u16,
        (value >> 32) as u16,
        (value >> 48) as u16,
    ];
    let first = halves.iter().position(|half| *half != 0).unwrap_or(0);
    push_word(code, encode_movz(destination, halves[first], first as u8));
    for (index, half) in halves.into_iter().enumerate() {
        if index != first && half != 0 {
            push_word(code, encode_movk(destination, half, index as u8));
        }
    }
}

fn push_word(code: &mut Vec<u8>, instruction: u32) {
    code.extend_from_slice(&instruction.to_le_bytes());
}

fn encode_mov_reg(destination: Arm64Reg, source: Arm64Reg) -> u32 {
    0xaa00_03e0 | (u32::from(source.0) << 16) | u32::from(destination.0)
}

fn encode_add(destination: Arm64Reg, lhs: Arm64Reg, rhs: Arm64Reg) -> u32 {
    0x8b00_0000 | (u32::from(rhs.0) << 16) | (u32::from(lhs.0) << 5) | u32::from(destination.0)
}

fn encode_movz(destination: Arm64Reg, immediate: u16, halfword: u8) -> u32 {
    0xd280_0000
        | (u32::from(halfword) << 21)
        | (u32::from(immediate) << 5)
        | u32::from(destination.0)
}

fn encode_movk(destination: Arm64Reg, immediate: u16, halfword: u8) -> u32 {
    0xf280_0000
        | (u32::from(halfword) << 21)
        | (u32::from(immediate) << 5)
        | u32::from(destination.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(code: &[u8]) -> Vec<u32> {
        code.chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
            .collect()
    }

    fn add_function() -> Function {
        let mut builder = Function::builder();
        let lhs = builder.parameter(0);
        let rhs = builder.parameter(1);
        let sum = builder.add(lhs, rhs);
        builder.return_value(sum);
        builder.finish()
    }

    #[test]
    fn emits_aapcs64_add_leaf() {
        let compiled = compile(&add_function()).unwrap();
        assert_eq!(
            words(compiled.code()),
            vec![
                0xaa00_03e9, // mov x9, x0
                0xaa01_03ea, // mov x10, x1
                0x8b0a_0129, // add x9, x9, x10
                0xaa09_03e0, // mov x0, x9
                0xd65f_03c0, // ret
            ]
        );
    }

    #[test]
    fn materializes_every_nonzero_immediate_halfword() {
        let mut builder = Function::builder();
        let value = builder.load_imm(0x1234_5678_9abc_def0);
        builder.return_value(value);
        let compiled = compile(&builder.finish()).unwrap();
        assert_eq!(compiled.code().len(), 6 * 4);
        assert_eq!(words(compiled.code()).last(), Some(&0xd65f_03c0));
    }

    #[test]
    fn reports_bootstrap_register_pressure() {
        let mut builder = Function::builder();
        let parameters = (0..8)
            .map(|index| builder.parameter(index))
            .collect::<Vec<_>>();
        let mut sum = parameters[0];
        for parameter in &parameters[1..] {
            sum = builder.add(sum, *parameter);
        }
        builder.return_value(sum);
        assert!(matches!(
            compile(&builder.finish()),
            Err(CompileError::Regalloc(
                LinearScanError::RegisterPressure { .. }
            ))
        ));
    }

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    unsafe fn execute_binary(function: &CompiledFunction, lhs: u64, rhs: u64) -> u64 {
        let length = function.code().len();
        let memory = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(memory, libc::MAP_FAILED);
        unsafe {
            std::ptr::copy_nonoverlapping(function.code().as_ptr(), memory.cast(), length);
            clear_cache(memory.cast(), memory.cast::<u8>().add(length));
        }
        assert_eq!(
            unsafe { libc::mprotect(memory, length, libc::PROT_READ | libc::PROT_EXEC) },
            0
        );
        let entry: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(memory) };
        let result = entry(lhs, rhs);
        assert_eq!(unsafe { libc::munmap(memory, length) }, 0);
        result
    }

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    unsafe extern "C" {
        #[link_name = "__clear_cache"]
        fn clear_cache(begin: *mut u8, end: *mut u8);
    }

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    #[test]
    fn executes_generated_add_under_aapcs64() {
        let compiled = compile(&add_function()).unwrap();
        assert_eq!(unsafe { execute_binary(&compiled, 19, 23) }, 42);
    }
}
