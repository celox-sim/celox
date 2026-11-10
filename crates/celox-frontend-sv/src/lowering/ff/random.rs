//! The random number streams of a process: `$urandom` and what draws from
//! it (IEEE 1800-2023 18.13), `$random` and the distribution functions
//! (20.14, Annex N).

use super::*;

impl Ff<'_, '_> {
    /// The seed of this process's `$urandom` stream before it is seeded: a
    /// function unit has the calling process's in its frame.
    pub(super) fn process_seed(&self) -> sv::ir::Expr {
        match self.frame.as_ref().and_then(|frame| frame.random.as_ref()) {
            Some((_, seed)) => sv::ir::Expr::Ident(seed.clone()),
            None => sv::ir::Expr::Literal(format!("32'd{}", self.random_seed)),
        }
    }

    /// The state of this process's `$urandom` stream.
    pub(super) fn urandom_state(&mut self) -> (SourceVarId, String) {
        if self.random.is_none() {
            let (id, name) = self.m.temp("urandom_state", 33, false, false);
            self.working.insert(id);
            self.targets.push((id, BitAccess::new(0, 32)));
            self.random = Some((id, name));
        }
        self.random.clone().expect("a process RNG")
    }

    /// The seed of this process's `$random` calls without a seed argument.
    /// It starts at 0, as the seed variable of `$random(seed)` typically
    /// does, so a design whose one process calls `$random` gets the same
    /// numbers as other simulators that use one seed for the whole design.
    pub(super) fn dist_seed(&mut self) -> (SourceVarId, String) {
        if self.dist_seed.is_none() {
            let (id, name) = self.m.temp("random_seed", 32, false, false);
            self.working.insert(id);
            self.targets.push((id, BitAccess::new(0, 31)));
            self.dist_seed = Some((id, name));
        }
        self.dist_seed.clone().expect("a process seed")
    }

    /// Evaluate `expr` into a new hidden variable.
    pub(super) fn hidden(
        &mut self,
        purpose: &str,
        expr: &sv::ir::Expr,
        signed: bool,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let value = self.eval(expr, Some((32, signed)))?;
        let (id, name) = self.m.temp(purpose, 32, signed, false);
        self.store(id, SIROffset::Static(0), 32, value);
        Ok(sv::ir::Expr::Ident(name))
    }

    /// IEEE 1800-2023 18.13.1/18.14: a deterministic, process-local stream.
    /// This full-period 32-bit LCG is implementation-defined; tests compare
    /// repeatability rather than assuming another simulator's sequence.
    pub(super) fn urandom(
        &mut self,
        seed: Option<sv::ir::Expr>,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let (id, name) = self.urandom_state();
        let initial = match seed {
            Some(seed) => seed,
            None => Expr::Mux {
                condition: Box::new(Expr::Binary {
                    left: Box::new(Expr::Ident(name.clone())),
                    op: BinaryOp::Eq,
                    right: Box::new(Expr::Literal("33'd0".into())),
                }),
                then_expr: Box::new(self.process_seed()),
                else_expr: Box::new(Expr::Ident(name.clone())),
            },
        };
        // The int seed formal preserves a narrow signed operand's sign bits
        // before its 32-bit representation becomes unsigned RNG state.
        let next = Expr::Resize {
            width: 32,
            signed: false,
            expr: Box::new(Expr::Binary {
                left: Box::new(Expr::Binary {
                    left: Box::new(self.int_formal(initial)),
                    op: BinaryOp::Mul,
                    right: Box::new(Expr::Literal("32'd1664525".into())),
                }),
                op: BinaryOp::Add,
                right: Box::new(Expr::Literal("32'd1013904223".into())),
            }),
        };
        let result = self.hidden("urandom_result", &next, false)?;
        let state = Expr::Concat(vec![Expr::Literal("1'b1".into()), result.clone()]);
        let state = self.eval(&state, Some((33, false)))?;
        self.store(id, SIROffset::Static(0), 33, state);
        Ok(result)
    }

    /// Randomize one scalar enum from the process stream. Arguments after the
    /// writable destination are its declared values, supplied by the analyzer.
    pub(super) fn randomize_enum(
        &mut self,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let Some(Some(Expr::Ident(name))) = args.first() else {
            return Err(unsupported("scope enum randomize variable"));
        };
        let values = args[1..]
            .iter()
            .map(Option::as_ref)
            .collect::<Option<Vec<_>>>()
            .filter(|values| !values.is_empty())
            .ok_or_else(|| unsupported("scope enum randomize domain"))?;
        let count = u32::try_from(values.len())
            .map_err(|_| unsupported("scope enum randomize domain size"))?;
        let threshold = count.wrapping_neg() % count;
        let draw_block = self.b.new_block();
        let accepted = self.b.new_block();
        self.jump(draw_block);
        self.b.switch_to_block(draw_block);
        let draw = self.randomize_word()?;
        let rejected = Expr::Binary {
            left: Box::new(draw.clone()),
            op: BinaryOp::Lt,
            right: Box::new(Expr::Literal(format!("32'd{threshold}"))),
        };
        let cond = self.eval_truth(&rejected)?.0;
        self.b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (draw_block, Vec::new()),
            false_block: (accepted, Vec::new()),
        });
        self.b.switch_to_block(accepted);
        let index = self.hidden(
            "enum_index",
            &Expr::Binary {
                left: Box::new(draw),
                op: BinaryOp::Mod,
                right: Box::new(Expr::Literal(format!("32'd{count}"))),
            },
            false,
        )?;
        let mut value = (*values.last().unwrap()).clone();
        for (position, member) in values.iter().enumerate().rev().skip(1) {
            value = Expr::Mux {
                condition: Box::new(Expr::Binary {
                    left: Box::new(index.clone()),
                    op: BinaryOp::Eq,
                    right: Box::new(Expr::Literal(format!("32'd{position}"))),
                }),
                then_expr: Box::new((*member).clone()),
                else_expr: Box::new(value),
            };
        }
        self.assign(&sv::ir::LValue::Ident(name.clone()), &value)?;
        Ok(Expr::Literal("32'sd1".into()))
    }

    /// A bijective 32-bit output permutation of the process RNG. Mixing
    /// high bits into low bits prevents the LCG's short low-bit periods from
    /// freezing bits when a wide variable consumes an even number of words.
    pub(super) fn randomize_word(&mut self) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let mut word = self.urandom(None)?;
        for (shift, multiplier) in [(16, 0x7feb352du32), (15, 0x846ca68bu32)] {
            let mixed = Expr::Binary {
                left: Box::new(word.clone()),
                op: BinaryOp::BitXor,
                right: Box::new(Expr::Binary {
                    left: Box::new(word),
                    op: BinaryOp::Shr,
                    right: Box::new(Expr::Literal(shift.to_string())),
                }),
            };
            word = self.hidden(
                "randomize_mix",
                &Expr::Binary {
                    left: Box::new(mixed),
                    op: BinaryOp::Mul,
                    right: Box::new(Expr::Literal(format!("32'd{multiplier}"))),
                },
                false,
            )?;
        }
        let mixed = Expr::Binary {
            left: Box::new(word.clone()),
            op: BinaryOp::BitXor,
            right: Box::new(Expr::Binary {
                left: Box::new(word),
                op: BinaryOp::Shr,
                right: Box::new(Expr::Literal("16".into())),
            }),
        };
        self.hidden("randomize_word", &mixed, false)
    }

    /// IEEE 1800-2023 18.13.2: a number of the `$urandom` stream between the
    /// two bounds, in either order, inclusive.
    pub(super) fn urandom_range(
        &mut self,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let bound = |this: &mut Self, arg: Option<&Expr>| match arg {
            Some(arg) => {
                let arg = this.hoist(arg)?;
                let arg = this.int_formal(arg);
                this.hidden("urandom_range_bound", &arg, false)
            }
            None => Ok(Expr::Literal("32'd0".into())),
        };
        let max = bound(self, args.first().and_then(Option::as_ref))?;
        let min = bound(self, args.get(1).and_then(Option::as_ref))?;
        let value = self.urandom(None)?;
        let widen = |expr: Expr, width| Expr::Resize {
            expr: Box::new(expr),
            width,
            signed: false,
        };
        let binary = |left, op, right| Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let reversed = binary(max.clone(), BinaryOp::Lt, min.clone());
        let pick = |when_reversed: &Expr, otherwise: &Expr| Expr::Mux {
            condition: Box::new(reversed.clone()),
            then_expr: Box::new(when_reversed.clone()),
            else_expr: Box::new(otherwise.clone()),
        };
        let (high, low) = (pick(&min, &max), pick(&max, &min));
        // The span is 2**32 for the full range, so it takes 33 bits. Scaling
        // takes the offset from the high bits of the number: the low bits of
        // the LCG have short periods.
        let span = binary(
            binary(widen(high, 33), BinaryOp::Sub, widen(low.clone(), 33)),
            BinaryOp::Add,
            Expr::Literal("33'd1".into()),
        );
        let offset = binary(
            binary(widen(value, 65), BinaryOp::Mul, widen(span, 65)),
            BinaryOp::Shr,
            Expr::Literal("65'd32".into()),
        );
        let result = Expr::Resize {
            expr: Box::new(binary(widen(low, 65), BinaryOp::Add, offset)),
            width: 32,
            signed: false,
        };
        self.hidden("urandom_range_result", &result, false)
    }

    /// IEEE 1800-2023 20.14.1 and Annex N: `$random` is
    /// `rtl_dist_uniform(seed, LONG_MIN, LONG_MAX)` with a 32-bit `long`.
    ///
    /// Annex N computes the number in floating point from bits 31:9 of the
    /// next seed, `m`; the result is exactly `512 * (m + 1) + m / 16384 -
    /// 2**31`, one less where that is negative and `m` is a multiple of
    /// 16384 (Annex N takes `r - 1` of a negative integral `r`), and
    /// `LONG_MIN` where `m` is all ones and the conversion overflows, as the
    /// reference code returns on x86 and Icarus Verilog does.
    pub(super) fn random(
        &mut self,
        seed: Option<&sv::ir::Expr>,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let (current, store_back) = match seed {
            Some(seed) => {
                let (lvalues, read, guards) = self.capture_system_actual(seed)?;
                (self.hoist(&read)?, Some((lvalues, guards)))
            }
            None => (Expr::Ident(self.dist_seed().1), None),
        };
        let literal = |value: u32| Expr::Literal(format!("32'd{value}"));
        let binary = |left, op, right| Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let current = self.int_formal(current);
        let current = self.hidden("random_seed", &current, false)?;
        let seeded = Expr::Mux {
            condition: Box::new(binary(current.clone(), BinaryOp::Eq, literal(0))),
            then_expr: Box::new(literal(259_341_593)),
            else_expr: Box::new(current),
        };
        let next = binary(
            binary(seeded, BinaryOp::Mul, literal(69_069)),
            BinaryOp::Add,
            literal(1),
        );
        let next = self.hidden("random_seed", &next, false)?;
        // `{m, 9'b0} + 512 + m / 16384`, which wraps only for the all-ones `m`.
        let scaled = binary(
            binary(
                binary(next.clone(), BinaryOp::BitAnd, literal(0xffff_fe00)),
                BinaryOp::Add,
                literal(512),
            ),
            BinaryOp::Add,
            binary(next.clone(), BinaryOp::Shr, literal(23)),
        );
        let scaled = self.hidden("random_scaled", &scaled, false)?;
        let integral_negative = binary(
            binary(scaled.clone(), BinaryOp::Lt, literal(0x8000_0000)),
            BinaryOp::LogicAnd,
            binary(
                binary(next.clone(), BinaryOp::BitAnd, literal(0x007f_fe00)),
                BinaryOp::Eq,
                literal(0),
            ),
        );
        let value = Expr::Mux {
            condition: Box::new(binary(
                binary(next.clone(), BinaryOp::Shr, literal(9)),
                BinaryOp::Eq,
                literal(0x007f_ffff),
            )),
            then_expr: Box::new(literal(0x8000_0000)),
            else_expr: Box::new(binary(
                binary(scaled, BinaryOp::BitXor, literal(0x8000_0000)),
                BinaryOp::Sub,
                Expr::Mux {
                    condition: Box::new(integral_negative),
                    then_expr: Box::new(literal(1)),
                    else_expr: Box::new(literal(0)),
                },
            )),
        };
        let value = Expr::Resize {
            expr: Box::new(value),
            width: 32,
            signed: true,
        };
        let result = self.hidden("random_result", &value, true)?;
        match store_back {
            Some((lvalues, guards)) => {
                self.check_tagged_guards(&guards)?;
                self.write_system_actual(&lvalues, &next)?;
            }
            None => {
                let (id, _) = self.dist_seed();
                let next = self.eval(&next, Some((32, false)))?;
                self.store(id, SIROffset::Static(0), 32, next);
            }
        }
        Ok(result)
    }

    /// Annex N's uniform transform, represented as an exact rational. The
    /// largest product has 79 bits, so 96-bit temporaries avoid overflow.
    pub(super) fn dist_uniform(
        &mut self,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp as B, Expr as E};
        let seed = args
            .first()
            .and_then(Option::as_ref)
            .ok_or_else(|| unsupported("seed of `$dist_uniform`"))?;
        let (lvalues, seed_read, guards) = self.capture_system_actual(seed)?;
        let seed_width = self
            .m
            .comparison_width(&seed_read, &seed_read)
            .ok_or_else(|| unsupported("integral seed of `$dist_uniform`"))?;
        let seed_signed = self.expr_signed(seed);
        let original = self
            .host_hidden(
                "dist_original_seed",
                &seed_read,
                seed_width,
                seed_signed,
                true,
            )?
            .1;
        let mut bound = |index| -> Result<E, sv::AnalyzerError> {
            let value = self.hoist(
                args.get(index)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| unsupported("bounds of `$dist_uniform`"))?,
            )?;
            let value = E::Resize {
                expr: Box::new(value),
                width: 32,
                signed: true,
            };
            self.hidden("dist_bound", &value, true)
        };
        let start = bound(1)?;
        let end = bound(2)?;
        let current = self.int_formal(original.clone());
        let current = self.hidden("dist_old_seed", &current, false)?;
        // The distribution's inout formal is int. Only the final copyout
        // writes the caller's actual, whose selector was captured once.
        let formal = self.hidden("dist_seed_formal", &current, true)?;
        let full = self.random(Some(&formal))?;
        let next = E::Binary {
            left: Box::new(E::Binary {
                left: Box::new(E::Mux {
                    condition: Box::new(E::Binary {
                        left: Box::new(current.clone()),
                        op: B::Eq,
                        right: Box::new(E::Literal("32'd0".into())),
                    }),
                    then_expr: Box::new(E::Literal("32'd259341593".into())),
                    else_expr: Box::new(current.clone()),
                }),
                op: B::Mul,
                right: Box::new(E::Literal("32'd69069".into())),
            }),
            op: B::Add,
            right: Box::new(E::Literal("32'd1".into())),
        };
        let next = self.hidden("dist_next_seed", &next, false)?;
        let lit = |n: i128| {
            if n < 0 {
                E::Unary {
                    op: sv::ir::UnaryOp::Minus,
                    expr: Box::new(E::Literal(format!("96'sd{}", -n))),
                }
            } else {
                E::Literal(format!("96'sd{n}"))
            }
        };
        let bin = |left, op, right| E::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let wide = |value| E::Resize {
            expr: Box::new(value),
            width: 96,
            signed: true,
        };
        let mux = |condition, then_expr, else_expr| E::Mux {
            condition: Box::new(condition),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        };
        let start = wide(start);
        let end = wide(end);
        let invalid = bin(start.clone(), B::Ge, end.clone());
        // If end is INT_MAX, Annex N shifts the lower bound down by one,
        // and adds one after the uniform transform.
        let shifted = bin(end.clone(), B::Eq, lit(i32::MAX as i128));
        let shifted_value = mux(shifted.clone(), lit(1), lit(0));
        let a = bin(start.clone(), B::Sub, shifted_value.clone());
        let span = bin(bin(end.clone(), B::Sub, start.clone()), B::Add, lit(1));
        // Extract the 23-bit mantissa before converting to signed wide
        // arithmetic. Widening the raw seed as signed would sign-extend its
        // high bit, and a logical shift would then retain those high ones.
        let m = wide(E::Resize {
            expr: Box::new(bin(next, B::Shr, E::Literal("32'd9".into()))),
            width: 32,
            signed: false,
        });
        let fraction = bin(
            bin(m.clone(), B::Mul, lit(8_388_608)),
            B::Add,
            bin(lit(8_388_608), B::Add, m),
        );
        let denominator = lit(1i128 << 46);
        let numerator = bin(
            bin(a, B::Mul, denominator.clone()),
            B::Add,
            bin(span, B::Mul, fraction),
        );
        let numerator = bin(
            numerator,
            B::Add,
            bin(shifted_value, B::Mul, denominator.clone()),
        );
        let numerator = self
            .host_hidden("dist_numerator", &numerator, 96, true, false)?
            .1;
        let negative_integral = bin(
            bin(numerator.clone(), B::Lt, lit(0)),
            B::LogicAnd,
            bin(
                bin(numerator.clone(), B::BitAnd, lit((1i128 << 46) - 1)),
                B::Eq,
                lit(0),
            ),
        );
        let value = bin(
            bin(numerator, B::Sar, lit(46)),
            B::Sub,
            mux(negative_integral, lit(1), lit(0)),
        );
        let value = self.host_hidden("dist_value", &value, 96, true, false)?.1;
        let value = mux(
            bin(value.clone(), B::Lt, start.clone()),
            start.clone(),
            mux(bin(value.clone(), B::Gt, end.clone()), end.clone(), value),
        );
        let is_full = bin(
            bin(start.clone(), B::Eq, lit(i32::MIN as i128)),
            B::LogicAnd,
            bin(end, B::Eq, lit(i32::MAX as i128)),
        );
        let value = mux(invalid.clone(), start, mux(is_full, wide(full), value));
        // A degenerate/reversed interval returns start without consuming seed.
        let next = E::Resize {
            expr: Box::new(formal),
            width: seed_width,
            signed: true,
        };
        let restored = mux(invalid, original, next);
        self.check_tagged_guards(&guards)?;
        self.write_system_actual(&lvalues, &restored)?;
        Ok(E::Resize {
            expr: Box::new(value),
            width: 32,
            signed: true,
        })
    }

    pub(super) fn distribution(
        &mut self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue};
        use sv::ir::{Expr, SystemFunction as F};
        if !self.can_request() {
            return Err(unsupported(format!(
                "{} outside a timed process",
                function.name()
            )));
        }
        let operation = match function {
            F::DistNormal => 0,
            F::DistExponential => 1,
            F::DistPoisson => 2,
            F::DistChiSquare => 3,
            F::DistT => 4,
            F::DistErlang => 5,
            _ => unreachable!(),
        };
        let seed = args
            .first()
            .and_then(Option::as_ref)
            .ok_or_else(|| unsupported("distribution seed"))?;
        // Capture selectors once before evaluating the other actuals, and
        // suspend with an int formal rather than writing the caller's storage.
        let (lvalues, captured, guards) = self.capture_system_actual(seed)?;
        let formal = self.int_formal(captured);
        let (seed_id, seed_result) =
            self.host_hidden("distribution_seed", &formal, 32, true, true)?;
        let value = |id| HostRequestValue {
            value_kind: celox_design::ValueKind::Bits,
            float_precision: None,
            id,
            width: 32,
            signed: true,
            is_string: false,
        };
        let mut operands = vec![value(seed_id)];
        for arg in args.iter().skip(1) {
            let arg = self.hoist(
                arg.as_ref()
                    .ok_or_else(|| unsupported("distribution argument"))?,
            )?;
            let formal = self.int_formal(arg);
            let (id, _) = self.host_hidden("distribution_arg", &formal, 32, true, true)?;
            operands.push(value(id));
        }
        let (ret_id, ret_name) = self.m.temp("distribution_result", 32, true, false);
        let site = self
            .host_request_sites
            .iter()
            .filter(|site| site.kind == HostRequestKind::Distribution)
            .count() as u32;
        self.host_request_sites.push(HostRequestSite {
            process: celox_design::SiteProcess::own(0),
            kind: HostRequestKind::Distribution,
            operation,
            args: operands,
            results: vec![value(ret_id), value(seed_id)],
            strings: Vec::new(),
        });
        self.request_host(HostRequestKind::Distribution, site)?;
        self.check_tagged_guards(&guards)?;
        self.write_system_actual(&lvalues, &seed_result)?;
        Ok(Expr::Ident(ret_name))
    }
}
