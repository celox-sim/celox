//! Constant evaluation of stateless, effect-free calls in ordinary expressions.
use super::*;

fn lvalue(value: crate::ir::LValue) -> LValue {
    match value {
        crate::ir::LValue::Ident(name) => LValue::Ident(name),
        crate::ir::LValue::Select {
            name,
            msb,
            lsb,
            array_slice_width,
            array_slice_reversed,
        } => LValue::Select {
            name,
            msb: msb.into(),
            lsb: lsb.into(),
            array_slice_width: array_slice_width.map(Into::into),
            array_slice_reversed,
            signed: false,
            is_2state: false,
        },
    }
}

/// A module's stateless zero-argument functions, evaluated at most once each.
/// This reuses the constant interpreter only after runtime lifetime and effect
/// checks; functions with persistent storage or observable effects stay calls.
pub struct RuntimeConstantFunctions<'a> {
    state: State,
    constants: &'a HashMap<String, i128>,
    cache: RefCell<HashMap<String, Option<typecheck::IntegralLiteral>>>,
    two_state: bool,
}

pub(super) fn charge(state: &State) -> Option<()> {
    if let Some(fuel) = state.runtime_fuel.get() {
        state.runtime_fuel.set(Some(fuel.checked_sub(1)?));
    }
    Some(())
}

impl<'a> RuntimeConstantFunctions<'a> {
    pub fn new(
        module: &crate::ir::Module,
        constants: &'a HashMap<String, i128>,
        four_state: bool,
    ) -> Self {
        let subroutines = module
            .subroutines()
            .iter()
            .cloned()
            .map(|subroutine| {
                subroutine.map(&mut Into::into, &mut lvalue, &mut std::convert::identity)
            })
            .collect::<Vec<Subroutine>>();
        let readers = simulation_state_readers(&subroutines);
        let mut functions = ConstantFunctionTable::default();
        for subroutine in &subroutines {
            if subroutine.is_task
                || !subroutine.persistent_locals.is_empty()
                || readers.contains_key(&subroutine.name)
                || subroutine
                    .params
                    .iter()
                    .any(|param| param.direction != crate::procedural::ParamDirection::Input)
            {
                continue;
            }
            let Some(return_var) = subroutine
                .return_var
                .clone()
                .zip(subroutine.return_type.clone())
            else {
                continue;
            };
            let mut owned = subroutine
                .params
                .iter()
                .map(|param| param.name.clone())
                .collect::<HashSet<_>>();
            owned.insert(return_var.0.clone());
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| {
                    if let Stmt::Local { name, .. } = stmt {
                        owned.insert(name.clone());
                    }
                });
            }
            let mut impure = false;
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| match stmt {
                    Stmt::Assign { lhs, .. } => impure |= !owned.contains(lhs.name()),
                    Stmt::AssignConcat { parts, .. } => {
                        impure |= parts.iter().any(|lhs| !owned.contains(lhs.name()))
                    }
                    Stmt::SystemTask { .. }
                    | Stmt::Call { .. }
                    | Stmt::Delay { .. }
                    | Stmt::WaitEvent(_)
                    | Stmt::WaitImplicit(_)
                    | Stmt::Wait(_)
                    | Stmt::Fork { .. }
                    | Stmt::WaitFork
                    | Stmt::DisableFork => impure = true,
                    _ => {}
                });
            }
            if impure {
                continue;
            }
            functions.insert(
                subroutine.name.clone(),
                ConstantFunction {
                    params: subroutine
                        .params
                        .iter()
                        .map(|param| (param.name.clone(), param.r#type.clone()))
                        .collect(),
                    argument_names: subroutine
                        .params
                        .iter()
                        .map(|param| param.source_name.clone())
                        .collect(),
                    argument_defaults: subroutine
                        .params
                        .iter()
                        .map(|param| param.default.clone())
                        .collect(),
                    return_var: Some(return_var),
                    body: subroutine.body.clone(),
                },
            );
        }
        // A rejected callee cannot become pure merely because its caller has
        // no direct effect. Include default expressions and discarded branches.
        loop {
            let mut rejected = Vec::new();
            for (name, function) in &functions {
                let mut missing = false;
                let mut check = |callee: &Callee, _: CallArguments<'_>| {
                    if let Callee::Subroutine(name) = callee {
                        missing |= !functions.contains_key(name);
                    }
                };
                for expression in function.argument_defaults.iter().flatten() {
                    visit_expr_calls(expression, &mut check);
                }
                for stmt in &function.body {
                    stmt.walk(&mut |stmt| {
                        for expression in lifetimes::stmt_exprs(stmt) {
                            visit_expr_calls(expression, &mut check);
                        }
                    });
                }
                if missing {
                    rejected.push(name.clone());
                }
            }
            if rejected.is_empty() {
                break;
            }
            for name in rejected {
                functions.remove(&name);
            }
        }
        let state = State::default();
        *state.functions.borrow_mut() = ConstantFunctions {
            functions: Arc::new(functions),
            locals: Arc::new(
                module
                    .locals()
                    .iter()
                    .map(|local| (local.name.clone(), local.r#type.clone()))
                    .collect(),
            ),
            errors: Arc::default(),
        };
        Self {
            state,
            constants,
            cache: RefCell::default(),
            two_state: !four_state,
        }
    }

    /// A call with no actual or formal arguments, or `None` when it cannot be
    /// evaluated without runtime state, effects, or exceeding the shared budget.
    pub fn value(&self, name: &str) -> Option<typecheck::IntegralLiteral> {
        if let Some(value) = self.cache.borrow().get(name) {
            return value.clone();
        }
        if !self
            .state
            .functions
            .borrow()
            .functions
            .get(name)?
            .params
            .is_empty()
        {
            return None;
        }
        self.state.runtime_fuel.set(Some(MAX_ITERATIONS));
        self.state.runtime_two_state.set(Some(self.two_state));
        let env = Environment {
            constants: self.constants,
            functions: &self.state,
        };
        let value = call_value(name, &[], &env).map(Value::into_literal);
        self.state.runtime_fuel.set(None);
        self.state.runtime_two_state.set(None);
        self.cache.borrow_mut().insert(name.into(), value.clone());
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(source: &str) -> crate::ir::Module {
        let parsed =
            crate::ParsedSource::parse(source, std::path::Path::new("constant-call.sv")).unwrap();
        let packages = crate::ParsedSource::analyze_packages(&[&parsed]).unwrap();
        parsed
            .analyze_module(
                "Top",
                &HashMap::default(),
                &[],
                &[],
                &crate::ModuleInterfaces::default(),
                &packages,
            )
            .unwrap()
            .modules()[0]
            .clone()
    }

    #[test]
    fn case_uses_the_types_of_all_labels() {
        let module = module(
            r#"module Top(output int q);
            function automatic int mixed();
                logic signed [7:0] x = 8'hff;
                case (x)
                    16'shffff: return 1;
                    16'h0000: return 2;
                    default: return 0;
                endcase
            endfunction
            assign q = mixed();
        endmodule"#,
        );
        let constants = HashMap::default();
        for four_state in [false, true] {
            let evaluator = RuntimeConstantFunctions::new(&module, &constants, four_state);
            assert_eq!(evaluator.value("mixed").unwrap().value, BigUint::default());
        }
    }

    #[test]
    fn inside_preserves_unknown_comparisons() {
        let module = module(
            r#"module Top(output logic q);
            function automatic logic unknown_match();
                logic x;
                return x inside {1'b1};
            endfunction
            assign q = unknown_match();
        endmodule"#,
        );
        let constants = HashMap::default();
        let evaluator = RuntimeConstantFunctions::new(&module, &constants, true);
        let value = evaluator.value("unknown_match").unwrap();
        assert_eq!(value.mask, BigUint::from(1u8));
        assert_eq!(value.value, BigUint::from(1u8));
    }

    #[test]
    fn case_context_reaches_arithmetic_and_skips_unselected_labels() {
        let module = module(
            r#"module Top(output int q);
            function automatic logic [15:0] unreachable_label();
                while (1) begin end
                return 0;
            endfunction
            function automatic int arithmetic();
                case (8'hff + 8'h01)
                    8'h00: return 0;
                    16'h100: return 1;
                    default: return 2;
                endcase
            endfunction
            function automatic int signed_case();
                logic signed [7:0] x = -1;
                case (x)
                    16'shffff: return 1;
                    16'sh0000: return 2;
                    default: return 0;
                endcase
            endfunction
            function automatic int skip_label();
                case (8'hff + 8'h01)
                    16'h100: return 1;
                    unreachable_label(): return 2;
                    default: return 0;
                endcase
            endfunction
            function automatic logic fill_case();
                case ('1)
                    8'hff: return 1;
                    default: return 0;
                endcase
            endfunction
            function automatic int exact_unknown();
                case (1'bx)
                    1'bz: return 0;
                    1'bx: return 1;
                    default: return 2;
                endcase
            endfunction
            assign q=0;
        endmodule"#,
        );
        let constants = HashMap::default();
        for four_state in [false, true] {
            let evaluator = RuntimeConstantFunctions::new(&module, &constants, four_state);
            for name in ["arithmetic", "signed_case", "skip_label", "fill_case"] {
                assert_eq!(
                    evaluator.value(name).unwrap().value,
                    BigUint::from(1u8),
                    "{name}"
                );
            }
            if four_state {
                assert_eq!(
                    evaluator.value("exact_unknown").unwrap().value,
                    BigUint::from(1u8)
                );
            }
        }
    }

    #[test]
    fn inside_combines_unknown_matches_and_ranges() {
        let module = module(
            r#"module Top(output logic q);
            function automatic logic definite_match(); return 1'bx inside {1'b0, 1'b?}; endfunction
            function automatic logic definite_mismatch(); return 2'bx0 inside {2'b11}; endfunction
            function automatic logic unknown_range(); return 1'bx inside {[1'b0:1'b1]}; endfunction
            function automatic logic empty_range(); return 2'b11 inside {[2'bxx:2'b01]}; endfunction
            assign q=0;
        endmodule"#,
        );
        let constants = HashMap::default();
        let evaluator = RuntimeConstantFunctions::new(&module, &constants, true);
        for (name, expected, mask) in [
            ("definite_match", 1u8, 0u8),
            ("definite_mismatch", 0, 0),
            ("unknown_range", 1, 1),
            ("empty_range", 0, 0),
        ] {
            let value = evaluator.value(name).unwrap();
            assert_eq!(value.value, BigUint::from(expected), "{name}");
            assert_eq!(value.mask, BigUint::from(mask), "{name}");
        }
    }

    #[test]
    fn stateless_matrix_function_with_unpacked_scratch_is_a_constant() {
        let module = module(
            r#"module Top(output wire [15:0] q);
            function [3:0][3:0] pattern();
                logic [3:0] words [3:0];
                for (int i=0; i<4; i++) words[i] = 4'b1 << i;
                for (int i=0; i<4; i++) for (int j=0; j<4; j++) pattern[i][j] = words[i][j];
            endfunction
            assign q = pattern();
        endmodule"#,
        );
        let constants = HashMap::default();
        let evaluator = RuntimeConstantFunctions::new(&module, &constants, true);
        let result = evaluator.value("pattern").unwrap();
        assert_eq!(result.width, 16);
        assert_eq!(result.value, BigUint::from(0x8421u32));
        assert_eq!(result.mask, BigUint::default());
        assert_eq!(evaluator.value("pattern"), Some(result));
    }

    #[test]
    fn wide_matrix_value_is_evaluated_once() {
        let module = module(
            r#"module Top(output wire [8099:0] q);
            function [89:0][89:0] pattern();
                logic [89:0] words [89:0];
                for (int i=0; i<90; i++) words[i] = 90'b1 << i;
                for (int i=0; i<90; i++) pattern[i] = words[i];
            endfunction
            assign q = pattern();
        endmodule"#,
        );
        assert!(module.subroutines()[0].persistent_locals.is_empty());
        let constants = HashMap::default();
        let evaluator = RuntimeConstantFunctions::new(&module, &constants, true);
        FUNCTION_CALLS.with(|calls| calls.set(0));
        let value = evaluator.value("pattern").unwrap();
        let mut expected = BigUint::default();
        for i in 0..90 {
            expected.set_bit(i * 91, true);
        }
        assert_eq!(value.width, 8100);
        assert_eq!(value.value, expected);
        assert_eq!(value.mask, BigUint::default());
        assert_eq!(evaluator.value("pattern"), Some(value));
        assert_eq!(FUNCTION_CALLS.with(Cell::get), 1);
    }

    #[test]
    fn runtime_state_and_observable_effects_remain_calls() {
        let module = module(
            r#"module Top(output int q);
            int global_value;
            function int stateful(); static int count=0; count++; return count; endfunction
            function int writes_global(); global_value=7; return 7; endfunction
            function int reads_global(); return global_value; endfunction
            function int message(); $display("keep this effect"); return 3; endfunction
            function int indirect(); return message(); endfunction
            assign q = 0;
        endmodule"#,
        );
        let constants = HashMap::default();
        let evaluator = RuntimeConstantFunctions::new(&module, &constants, true);
        for name in [
            "stateful",
            "writes_global",
            "reads_global",
            "message",
            "indirect",
        ] {
            assert!(
                evaluator.value(name).is_none(),
                "{name} was treated as a constant"
            );
        }
    }

    #[test]
    fn unknown_intermediates_in_two_state_mode_stay_runtime_calls() {
        let module = module(
            r#"module Top(output int q);
            function automatic int explicit_unknown(); return 1'bx === 1'b0; endfunction
            function automatic int outside_select(); logic [3:0] x=0; return x[8] === 1'b0; endfunction
            function automatic int non_integral_local(); real x=1.5; return x * 2; endfunction
            assign q=0;
        endmodule"#,
        );
        let constants = HashMap::default();
        let two_state = RuntimeConstantFunctions::new(&module, &constants, false);
        for name in ["explicit_unknown", "outside_select", "non_integral_local"] {
            assert!(
                two_state.value(name).is_none(),
                "{name} must retain runtime semantics"
            );
        }
        let four_state = RuntimeConstantFunctions::new(&module, &constants, true);
        for name in ["explicit_unknown", "outside_select"] {
            assert_eq!(four_state.value(name).unwrap().value, BigUint::default());
        }
        assert!(four_state.value("non_integral_local").is_none());
    }

    #[test]
    fn aggregate_slice_assignment_stays_with_runtime_lowering() {
        let module = module(
            r#"module Top(output wire [31:0] q);
            function automatic logic [31:0] slices();
                logic [7:0] words [0:3];
                words[0]=8'h11; words[1]=8'h22; words[2]=8'h33; words[3]=8'h44;
                words[0+:2] = words[2+:2];
                return {words[0], words[1], words[2], words[3]};
            endfunction
            assign q = slices();
        endmodule"#,
        );
        let constants = HashMap::default();
        let evaluator = RuntimeConstantFunctions::new(&module, &constants, true);
        assert!(
            evaluator
                .state
                .functions
                .borrow()
                .functions
                .contains_key("slices")
        );
        assert!(evaluator.value("slices").is_none());
    }

    #[test]
    fn automatic_initial_values_follow_the_runtime_state_mode() {
        let module = module(
            r#"module Top(output int q);
            function automatic int unknown_local(); logic [7:0] x; return x === 8'b0; endfunction
            function automatic int bit_local(); bit [7:0] x; return x == 8'b0; endfunction
            assign q = 0;
        endmodule"#,
        );
        let constants = HashMap::default();
        for four_state in [false, true] {
            let evaluator = RuntimeConstantFunctions::new(&module, &constants, four_state);
            assert_eq!(
                evaluator.value("unknown_local").unwrap().value,
                BigUint::from(u8::from(!four_state))
            );
            assert_eq!(
                evaluator.value("bit_local").unwrap().value,
                BigUint::from(1u8)
            );
        }
    }
    #[test]
    fn conditional_functions_keep_both_arm_types() {
        let module = module(
            r#"module Top(output int q);
            function automatic int mixed_sign();
                return 1'b1 ? 8'shff : 8'h00;
            endfunction
            function automatic logic [31:0] concat_width();
                return {8'hab, (1'b1 ? 8'hff : 16'h0), 8'hcd};
            endfunction
            assign q = mixed_sign();
        endmodule"#,
        );
        let constants = HashMap::default();
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for four_state in [false, true] {
            let evaluator = RuntimeConstantFunctions::new(&module, &constants, four_state);
            for (name, value) in [("mixed_sign", 255u32), ("concat_width", 0xab00ffcdu32)] {
                let result = evaluator.value(name).unwrap();
                actual.push((four_state, name, result.value, result.mask));
                expected.push((four_state, name, BigUint::from(value), BigUint::default()));
            }
        }
        assert_eq!(actual, expected);
    }
    #[test]
    fn conditional_functions_skip_values_but_keep_types() {
        let module = module(
            r#"module Top(output int q);
            function automatic logic [15:0] never_called();
                return 16'h1234;
            endfunction
            function automatic int skipped_function();
                return 1'b1 ? 8'shff : never_called();
            endfunction
            function automatic int skipped_function_false();
                return 1'b0 ? never_called() : 8'shff;
            endfunction
            function automatic int skipped_unknown();
                return 1'b1 ? 8'hff : 32'bx;
            endfunction
            function automatic int skipped_index();
                logic [31:0] word;
                return 1'b1 ? 8'hff : word[never_called() +: 16];
            endfunction
            function automatic logic [31:0] power_type();
                return {8'hab, (1'b1 ? 8'hff : (8'h1 ** 32'd3)), 8'hcd};
            endfunction
            function automatic int signed_power_type();
                return 1'b1 ? 8'shff : (8'sh1 ** 32'd3);
            endfunction
            function automatic logic [31:0] arithmetic_context();
                return {(1'b1 ? (8'hff + 8'h01) : 16'h0)};
            endfunction
            function automatic int assignment_context();
                return 1'b1 ? (8'hff + 8'h01) : 8'h0;
            endfunction
            function automatic int unsigned_shift_context();
                return 1'b1 ? (8'sh80 >>> 1) : 8'h00;
            endfunction
            function automatic logic [15:0] power_context();
                return {(1'b1 ? (8'h2 ** 32'd65536) : 16'h0)};
            endfunction
            assign q = 0;
        endmodule"#,
        );
        let constants = HashMap::default();
        for four_state in [false, true] {
            let evaluator = RuntimeConstantFunctions::new(&module, &constants, four_state);
            for (name, expected) in [
                ("skipped_function", 255u32),
                ("skipped_function_false", 255),
                ("skipped_unknown", 255),
                ("skipped_index", 255),
                ("power_type", 0x00abffcd),
                ("signed_power_type", 0xffffffff),
                ("arithmetic_context", 256),
                ("assignment_context", 256),
                ("unsigned_shift_context", 64),
                ("power_context", 0),
            ] {
                FUNCTION_CALLS.with(|count| count.set(0));
                let result = evaluator.value(name).unwrap();
                assert_eq!(
                    result.value,
                    BigUint::from(expected),
                    "{name}, four_state={four_state}"
                );
                assert_eq!(result.mask, BigUint::default(), "{name}");
                assert_eq!(
                    FUNCTION_CALLS.with(Cell::get),
                    1,
                    "{name} evaluated an unselected call"
                );
            }
        }
    }

    #[test]
    fn unknown_conditional_functions_evaluate_condition_once() {
        let module = module(
            r#"module Top(output logic [15:0] q);
            function automatic logic predicate(); return 1'bx; endfunction
            function automatic logic [7:0] left_value(); return 8'ha5; endfunction
            function automatic logic [15:0] right_value(); return 16'h00a5; endfunction
            function automatic logic [15:0] merged();
                return predicate() ? left_value() : right_value();
            endfunction
            assign q = 0;
        endmodule"#,
        );
        let constants = HashMap::default();
        let evaluator = RuntimeConstantFunctions::new(&module, &constants, true);
        FUNCTION_CALLS.with(|count| count.set(0));
        let result = evaluator.value("merged").unwrap();
        assert_eq!(result.value, BigUint::from(0xa5u8));
        assert_eq!(result.mask, BigUint::default());
        assert_eq!(FUNCTION_CALLS.with(Cell::get), 4);
        assert_eq!(evaluator.value("merged"), Some(result));
        assert_eq!(FUNCTION_CALLS.with(Cell::get), 4);
    }
}
