mod arithmetic;
mod control_flow;
mod features;
mod memory;
mod registers;
mod shifts;
mod sparse;

use super::*;
use crate::native::features::{
    IMAGE_FEATURE_AVX, IMAGE_FEATURE_BMI2, IMAGE_FEATURE_POPCNT, StateBaseStrategy, X86Features,
};
use crate::native::jit_mem::JitCode;
use crate::native::{mir_legalize, mir_opt, regalloc};
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, Register};
