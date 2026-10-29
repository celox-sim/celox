//! A binding to an expression compiled as an ordinary SIR kernel.
//! Machine code is owned by the simulator backend and its program image.
use crate::{ExprBytecode, ExprOpcode as Opcode, ExpressionKernel};

#[derive(Clone, Debug)]
pub struct CompiledExpr {
    bytecode: ExprBytecode,
    observes_hardware: bool,
}
impl CompiledExpr {
    pub fn new(bytecode: ExprBytecode) -> Self {
        Self {
            bytecode,
            observes_hardware: true,
        }
    }
    pub fn bytecode(&self) -> &ExprBytecode {
        &self.bytecode
    }
    pub fn kernel(&self) -> Option<&ExpressionKernel<usize>> {
        self.bytecode.kernel()
    }
    pub fn classify_hardware_access(mut self, mut hardware: impl FnMut(usize) -> bool) -> Self {
        self.observes_hardware = self.bytecode.ops().iter().any(|op| match op {
            Opcode::LoadU64 { location, .. }
            | Opcode::LoadWide { location, .. }
            | Opcode::LoadIndexed { location, .. }
            | Opcode::LoadBitSelect { location, .. }
            | Opcode::LoadString { location, .. }
            | Opcode::StoreString { location, .. }
            | Opcode::StoreU64 { location, .. } => hardware(*location),
            _ => false,
        });
        self
    }
    pub fn observes_hardware(&self) -> bool {
        self.observes_hardware
    }
    /// Literal folding for conservative binding-time overlap checks. General
    /// constant folding belongs to the shared SIR optimizer.
    pub fn constant_u64(&self) -> Option<u64> {
        self.kernel()
            .and_then(|kernel| kernel.constant)
            .or_else(|| self.bytecode.literal_u64())
    }
}
