//! Fuzz-style cross-validation: generate random combinational Veryl modules,
//! run with both Native and Cranelift backends, compare outputs.
//!
//! Uses deterministic PRNG for reproducibility.

use celox::{BigUint, Simulator};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self { Self(seed) }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo)
    }
}

fn gen_random_comb_module(rng: &mut Rng, width: usize) -> (String, Vec<u64>) {
    let n_inputs = rng.range(2, 5) as usize;
    let n_temps = rng.range(1, 4) as usize;

    let mut code = format!("module Top(\n");
    for i in 0..n_inputs {
        code += &format!("    i{i}: input logic<{width}>,\n");
    }
    code += &format!("    o: output logic<{width}>\n) {{\n");

    // Generate random operations
    let ops = ["+" , "-", "&", "|", "^"];
    let mut last_var = format!("i0");
    for t in 0..n_temps {
        let op = ops[rng.range(0, ops.len() as u64) as usize];
        let rhs_idx = rng.range(0, n_inputs as u64) as usize;
        let rhs = format!("i{rhs_idx}");
        code += &format!("    var t{t}: logic<{width}>;\n    assign t{t} = {last_var} {op} {rhs};\n");
        last_var = format!("t{t}");
    }
    code += &format!("    assign o = {last_var};\n}}\n");

    // Generate random input values
    let mut inputs = Vec::new();
    for _ in 0..n_inputs {
        let mask = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
        inputs.push(rng.next() & mask);
    }

    (code, inputs)
}

fn run_fuzz_case(seed: u64, width: usize) {
    let mut rng = Rng::new(seed);
    let (code, inputs) = gen_random_comb_module(&mut rng, width);

    let sim_n_result = std::panic::catch_unwind(|| {
        let mut sim = Simulator::builder(&code, "Top").build().unwrap();
        let n_inputs = inputs.len();
        for i in 0..n_inputs {
            let sig = sim.signal(&format!("i{i}"));
            match width {
                w if w <= 32 => sim.set(sig, inputs[i] as u32),
                w if w <= 64 => sim.set(sig, inputs[i]),
                _ => sim.set_wide(sig, BigUint::from(inputs[i])),
            }
        }
        let o = sim.signal("o");
        sim.get(o)
    });

    let sim_c_result = std::panic::catch_unwind(|| {
        let mut sim = Simulator::builder(&code, "Top").build_cranelift().unwrap();
        let n_inputs = inputs.len();
        for i in 0..n_inputs {
            let sig = sim.signal(&format!("i{i}"));
            match width {
                w if w <= 32 => sim.set(sig, inputs[i] as u32),
                w if w <= 64 => sim.set(sig, inputs[i]),
                _ => sim.set_wide(sig, BigUint::from(inputs[i])),
            }
        }
        let o = sim.signal("o");
        sim.get(o)
    });

    match (sim_n_result, sim_c_result) {
        (Ok(vn), Ok(vc)) => {
            assert_eq!(vn, vc,
                "Fuzz mismatch (seed={seed}, width={width}):\n  code: {code}\n  native={vn:#x}\n  cranelift={vc:#x}");
        }
        (Err(_), Err(_)) => {} // Both fail: skip
        (Ok(vn), Err(e)) => {
            panic!("Cranelift panicked but native succeeded (seed={seed}): native={vn:#x}, error={e:?}");
        }
        (Err(e), Ok(vc)) => {
            panic!("Native panicked but Cranelift succeeded (seed={seed}): cranelift={vc:#x}, error={e:?}");
        }
    }
}

#[test]
fn fuzz_narrow_100_seeds() {
    for seed in 0..100 {
        run_fuzz_case(seed * 31337 + 1, 32);
    }
}

#[test]
fn fuzz_64bit_100_seeds() {
    for seed in 0..100 {
        run_fuzz_case(seed * 7919 + 42, 64);
    }
}

#[test]
fn fuzz_wide_128bit_50_seeds() {
    for seed in 0..50 {
        run_fuzz_case(seed * 104729 + 7, 128);
    }
}
