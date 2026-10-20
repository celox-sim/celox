//! Integer probabilistic distributions (IEEE 1800-2023 20.14.2, Annex N).
//!
//! The seed transition wraps at 32 bits. Floating point is only an internal
//! implementation detail; all SystemVerilog formals and results are signed int.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distribution {
    Normal,
    Exponential,
    Poisson,
    ChiSquare,
    T,
    Erlang,
}

impl Distribution {
    pub fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            0 => Self::Normal,
            1 => Self::Exponential,
            2 => Self::Poisson,
            3 => Self::ChiSquare,
            4 => Self::T,
            5 => Self::Erlang,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "$dist_normal",
            Self::Exponential => "$dist_exponential",
            Self::Poisson => "$dist_poisson",
            Self::ChiSquare => "$dist_chi_square",
            Self::T => "$dist_t",
            Self::Erlang => "$dist_erlang",
        }
    }

    /// Draw one value, updating `seed` only after validating the int formals.
    pub fn sample(self, seed: &mut i32, args: &[i32]) -> Result<i32, String> {
        let arity = if matches!(self, Self::Normal | Self::Erlang) {
            2
        } else {
            1
        };
        if args.len() != arity {
            return Err(format!(
                "{} requires {arity} distribution parameters",
                self.name()
            ));
        }
        if self != Self::Normal && args.iter().any(|value| *value <= 0) {
            return Err(format!(
                "{} requires positive distribution parameters",
                self.name()
            ));
        }
        let sample = match self {
            Self::Normal => normal(seed) * f64::from(args[1]) + f64::from(args[0]),
            Self::Exponential => exponential(seed) * f64::from(args[0]),
            Self::Poisson => return Ok(poisson(seed, args[0])),
            Self::ChiSquare => chi_square(seed, args[0]),
            Self::T => {
                let chi = chi_square(seed, args[0]);
                normal(seed) / (chi / f64::from(args[0])).sqrt()
            }
            Self::Erlang => {
                let mut product = 1.0;
                for _ in 0..args[0] {
                    product *= uniform(seed, 0.0, 1.0);
                }
                -f64::from(args[1]) * product.ln() / f64::from(args[0])
            }
        };
        // Annex N rounds symmetrically, halfway away from zero. Its 32-bit
        // C conversion produces INT_MIN on an out-of-range result on the
        // reference targets; make that behavior explicit instead of relying
        // on Rust's saturating float-to-int conversion.
        let rounded = sample.round();
        Ok(
            if !rounded.is_finite()
                || rounded < f64::from(i32::MIN)
                || rounded > f64::from(i32::MAX)
            {
                i32::MIN
            } else {
                rounded as i32
            },
        )
    }
}

fn uniform(seed: &mut i32, start: f64, end: f64) -> f64 {
    if *seed == 0 {
        *seed = 259_341_593;
    }
    *seed = seed.wrapping_mul(69_069).wrapping_add(1);
    let base = f64::from(f32::from_bits(((*seed as u32) >> 9) | 0x3f80_0000));
    let adjusted = base + base * (1.0 / 8_388_608.0);
    (end - start) * (adjusted - 1.0) + start
}

fn normal(seed: &mut i32) -> f64 {
    loop {
        let first = uniform(seed, -1.0, 1.0);
        let second = uniform(seed, -1.0, 1.0);
        let radius = first * first + second * second;
        if radius < 1.0 && radius != 0.0 {
            return first * (-2.0 * radius.ln() / radius).sqrt();
        }
    }
}

fn exponential(seed: &mut i32) -> f64 {
    let value = uniform(seed, 0.0, 1.0);
    if value == 0.0 { 0.0 } else { -value.ln() }
}

fn poisson(seed: &mut i32, mean: i32) -> i32 {
    let threshold = (-f64::from(mean)).exp();
    let mut product = uniform(seed, 0.0, 1.0);
    let mut count = 0;
    while threshold < product {
        count += 1;
        product *= uniform(seed, 0.0, 1.0);
    }
    count
}

fn chi_square(seed: &mut i32, degrees: i32) -> f64 {
    let mut result = if degrees % 2 != 0 {
        normal(seed).powi(2)
    } else {
        0.0
    };
    for _ in 0..degrees / 2 {
        result += 2.0 * exponential(seed);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::Distribution::*;

    #[test]
    fn annex_n_fixed_seed_vectors() {
        for (distribution, args, expected, next) in [
            (Normal, vec![0, 10], -4, -642_763_923),
            (Exponential, vec![10], 82, 1_174_174),
            (Poisson, vec![10], 3, -642_763_923),
            (ChiSquare, vec![5], 2, -1_369_077_693),
            (T, vec![5], 0, -434_453_879),
            (Erlang, vec![3, 10], 33, 861_173_020),
        ] {
            let mut seed = 17;
            assert_eq!(distribution.sample(&mut seed, &args).unwrap(), expected);
            assert_eq!(seed, next);
        }
    }

    #[test]
    fn zero_negative_seeds_and_small_shape_parameters() {
        for (distribution, initial, args, expected, next) in [
            (Normal, 0, vec![-3, 5], -1, 1_082_744_015),
            (Exponential, -1, vec![10], 0, -69_068),
            (Poisson, -1, vec![1], 3, 1_486_841_147),
            (ChiSquare, 1, vec![2], 22, 69_070),
            (T, 1, vec![1], -4, 2_044_158_073),
            (Erlang, 0, vec![1, 3], 2, -1_844_104_698),
        ] {
            let mut seed = initial;
            assert_eq!(distribution.sample(&mut seed, &args).unwrap(), expected);
            assert_eq!(seed, next);
        }
    }

    #[test]
    fn invalid_parameters_do_not_advance_seed() {
        for (distribution, args) in [
            (Exponential, vec![0]),
            (Poisson, vec![-1]),
            (ChiSquare, vec![0]),
            (T, vec![-2]),
            (Erlang, vec![0, 10]),
            (Erlang, vec![2, 0]),
        ] {
            let mut seed = 17;
            assert!(
                distribution
                    .sample(&mut seed, &args)
                    .unwrap_err()
                    .contains("positive")
            );
            assert_eq!(seed, 17);
        }
    }
}
