use super::*;
use celox_design::{DomainKind, PortTypeKind};
use lydite_solver::finite::{Limits, SearchHint, Verdict, solve_with_hint};
use serde::de::DeserializeOwned;

fn var(name: &str, w: u32) -> Term {
    ir::var(name.into(), Sort::Bv(w))
}
fn prove(actual: Term, expected: Term) {
    let result = solve_with_hint(
        &not(eq(actual, expected)),
        &Env::new(),
        Limits::default(),
        SearchHint::Unsat,
    );
    assert_eq!(result.verdict, Verdict::Unsat, "{}", result.diagnostics());
}
fn witness(formula: Term) {
    let result = solve_with_hint(&formula, &Env::new(), Limits::default(), SearchHint::Sat);
    assert_eq!(result.verdict, Verdict::Sat, "{}", result.diagnostics());
    assert!(result.original_formula_validated);
}
fn address(id: u64, region: u64) -> Value {
    json!({"instance_id":0,"var_id":id,"region":region})
}
fn load(r: u64, id: u64, region: u64, w: u32) -> Value {
    json!({"Load":[r,address(id,region),{"Static":0},w]})
}
fn store(id: u64, region: u64, r: u64, w: u32) -> Value {
    json!({"Store":[address(id,region),{"Static":0},w,r,[],[]]})
}
fn commit(id: u64, src: u64, dst: u64, w: u32) -> Value {
    json!({"Commit":[address(id,src),address(id,dst),{"Static":0},w,[]]})
}
fn block(inst: Vec<Value>, term: Value) -> Value {
    json!({"params":[],"instructions":inst,"terminator":term})
}
fn typed<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}
fn unit(mut blocks: Value, widths: &[u32]) -> Unit {
    let types = widths
        .iter()
        .enumerate()
        .map(|(i, w)| (i.to_string(), json!({"Bit":{"width":w,"signed":false}})))
        .collect::<Map<_, _>>();
    for (id, block) in blocks.as_object_mut().unwrap() {
        block["id"] = json!(id.parse::<usize>().unwrap());
    }
    typed(json!({"entry_block_id":0,"blocks":blocks,"register_map":types}))
}
fn lift_json(code: &Value, config: &Value) -> Res<Transition> {
    lift(
        &serde_json::from_value(code.clone()).map_err(|e| e.to_string())?,
        config,
    )
}
fn lifter(values: &[(u64, u32, usize, &str)]) -> Lifter {
    let mut state = BTreeMap::new();
    for (id, w, count, prefix) in values {
        let mut s = Storage::new(*w, *count, false);
        for (i, c) in s.cells.iter_mut().enumerate() {
            c.value = Some(var(&format!("{prefix}{i}"), *w))
        }
        state.insert((0, *id, 0), s);
    }
    Lifter {
        frame: Frame {
            guard: b(true),
            state,
            regs: BTreeMap::new(),
        },
        stats: Statistics::default(),
    }
}
#[test]
fn arbitrary_branch_holds_and_nba_reads_old_state() {
    let mut l = lifter(&[
        (0, 1, 1, "en"),
        (1, 8, 1, "a"),
        (2, 8, 1, "q"),
        (3, 8, 1, "p"),
    ]);
    let u = unit(
        json!({
            "0":block(vec![commit(2,0,1,8),commit(3,0,1,8),load(0,0,0,1)],json!({"Branch":{"cond":0,"true_block":[1,[]],"false_block":[2,[]]}})),
            "1":block(vec![load(1,1,0,8),store(2,1,1,8),load(2,2,0,8),store(3,1,2,8)],json!({"Jump":[2,[]]})),
            "2":block(vec![commit(2,1,0,8),commit(3,1,0,8)],json!("Return"))
        }),
        &[1, 8, 8],
    );
    l.execute(&u).unwrap();
    let q = l.frame.state[&(0, 2, 0)].read(0, 8).unwrap();
    let p = l.frame.state[&(0, 3, 0)].read(0, 8).unwrap();
    let en = truth(var("en0", 1));
    prove(q.clone(), ite(en.clone(), var("a0", 8), var("q0", 8)));
    prove(p.clone(), ite(en.clone(), var("q0", 8), var("p0", 8)));
    witness(not(eq(p, ite(en.clone(), var("a0", 8), var("p0", 8)))));
    witness(and(en.clone(), not(eq(q.clone(), var("q0", 8)))));
    witness(and(not(en), eq(q, var("q0", 8))));
}
#[test]
fn block_parameters_and_switch_are_symbolic() {
    let mut l = lifter(&[(0, 2, 1, "selector"), (1, 8, 1, "a"), (2, 8, 1, "q")]);
    let mut join = block(vec![store(2, 0, 3, 8)], json!("Return"));
    join["params"] = json!([3]);
    let u = unit(
        json!({"0":block(vec![load(0,0,0,2),load(1,1,0,8),load(2,2,0,8)],json!({"Switch":{"selector":0,"cases":[{"value":[1],"target":1},{"value":[2],"target":1}],"default":2}})),"1":block(vec![],json!({"Jump":[3,[1]]})),"2":block(vec![],json!({"Jump":[3,[2]]})),"3":join}),
        &[2, 8, 8, 8],
    );
    l.execute(&u).unwrap();
    let condition = or(
        eq(var("selector0", 2), bv(2, 1)),
        eq(var("selector0", 2), bv(2, 2)),
    );
    prove(
        l.frame.state[&(0, 2, 0)].read(0, 8).unwrap(),
        ite(condition, var("a0", 8), var("q0", 8)),
    );
}
#[test]
fn symbolic_sparse_nba_aliasing_last_write_and_untouched_holds() {
    let mut l = lifter(&[
        (0, 3, 1, "i"),
        (1, 3, 1, "j"),
        (2, 8, 1, "a"),
        (3, 8, 1, "b"),
        (4, 8, 4, "m"),
    ]);
    let offset = |r: u64| json!({"Element":{"index":r,"element_width":8,"bit_offset":0,"dynamic_bit_offset":null}});
    let u = unit(
        json!({"0":block(vec![load(0,0,0,3),load(1,1,0,3),load(2,2,0,8),load(3,3,0,8),json!({"Store":[address(4,2),offset(0),8,2,[],[]]}),json!({"Store":[address(4,2),offset(1),8,3,[],[]]}),commit(4,2,0,32)],json!("Return"))}),
        &[3, 3, 8, 8],
    );
    l.execute(&u).unwrap();
    for n in 0..4 {
        let actual = l.frame.state[&(0, 4, 0)].read(n * 8, 8).unwrap();
        let expected = ite(
            eq(var("j0", 3), bv(3, n as u64)),
            var("b0", 8),
            ite(
                eq(var("i0", 3), bv(3, n as u64)),
                var("a0", 8),
                var(&format!("m{n}"), 8),
            ),
        );
        prove(actual, expected)
    }
    // All indices, including out-of-range, were quantified without a domain assumption.
    witness(and(
        eq(var("i0", 3), var("j0", 3)),
        not(eq(var("a0", 8), var("b0", 8))),
    ));
}
#[test]
fn dynamic_load_and_out_of_bounds_zero_no_address_selection() {
    let mut l = lifter(&[(0, 3, 1, "i"), (1, 8, 4, "m"), (2, 8, 1, "q")]);
    let u = unit(
        json!({"0":block(vec![load(0,0,0,3),json!({"Load":[1,address(1,0),{"Element":{"index":0,"element_width":8,"bit_offset":0,"dynamic_bit_offset":null}},8]}),store(2,0,1,8)],json!("Return"))}),
        &[3, 8],
    );
    l.execute(&u).unwrap();
    let mut expected = bv(8, 0);
    for n in 0..4 {
        expected = ite(
            eq(var("i0", 3), bv(3, n)),
            var(&format!("m{n}"), 8),
            expected,
        )
    }
    prove(l.frame.state[&(0, 2, 0)].read(0, 8).unwrap(), expected);
}
#[test]
fn partial_dynamic_write_preserves_other_bits_and_crosses_lanes() {
    let mut l = lifter(&[(0, 4, 1, "offset"), (1, 4, 1, "a"), (2, 8, 2, "q")]);
    let u = unit(
        json!({"0":block(vec![load(0,0,0,4),load(1,1,0,4),json!({"Store":[address(2,0),{"Dynamic":0},4,1,[],[]]})],json!("Return"))}),
        &[4, 4],
    );
    l.execute(&u).unwrap();
    let old = cat(var("q1", 8), var("q0", 8)).unwrap();
    let amount = resize(var("offset0", 4), 16, false);
    let mask = op("bvshl", 16, bv(16, 15), amount.clone());
    let inv = ir::node(Sort::Bv(16), "bvnot", vec![mask]);
    let expected = op(
        "bvor",
        16,
        op("bvand", 16, old, inv),
        op("bvshl", 16, resize(var("a0", 4), 16, false), amount),
    );
    prove(l.frame.state[&(0, 2, 0)].read(0, 16).unwrap(), expected);
}
#[test]
fn element_offset_arithmetic_cannot_wrap_large_indices_into_storage() {
    let offset = typed(
        json!({"Element":{"index":0,"element_width":8,"bit_offset":1,"dynamic_bit_offset":1}}),
    );
    let regs = BTreeMap::from([(0, var("index", 64)), (1, var("bit_offset", 64))]);
    let guard = Lifter::access_guard(&offset, &regs, 9).unwrap();
    let valid = or(
        and(
            eq(var("index", 64), bv(64, 0)),
            eq(var("bit_offset", 64), bv(64, 8)),
        ),
        and(
            eq(var("index", 64), bv(64, 1)),
            eq(var("bit_offset", 64), bv(64, 0)),
        ),
    );
    prove(boolword(guard, 1), boolword(valid, 1));
}
#[test]
fn symbolic_shifts_cover_oversized_counts_and_sign_extension() {
    let x = var("x", 4);
    let shift = var("shift", 8);
    for name in [BinaryOp::Shl, BinaryOp::Shr, BinaryOp::Sar] {
        let actual = binary(name, x.clone(), shift.clone(), 8, false, false).unwrap();
        let extended = resize(x.clone(), 8, name == BinaryOp::Sar);
        let expected = if name == BinaryOp::Shl {
            op("bvshl", 8, extended, shift.clone())
        } else if name == BinaryOp::Shr {
            op("bvlshr", 8, extended, shift.clone())
        } else {
            let neg = truth(extract(x.clone(), 3, 1));
            let inverted = ir::node(Sort::Bv(8), "bvnot", vec![extended.clone()]);
            ite(
                neg,
                ir::node(
                    Sort::Bv(8),
                    "bvnot",
                    vec![op("bvlshr", 8, inverted, shift.clone())],
                ),
                op("bvlshr", 8, extended, shift.clone()),
            )
        };
        prove(actual, expected)
    }
}
#[test]
fn cyclic_cfg_and_unbound_state_fail_closed() {
    let mut l = lifter(&[(0, 8, 1, "q")]);
    let cyclic = unit(json!({"0":block(vec![],json!({"Jump":[0,[]]}))}), &[]);
    assert!(l.execute(&cyclic).unwrap_err().contains("cyclic"));
    let mut l = lifter(&[(0, 8, 1, "q")]);
    let missing = unit(
        json!({"0":block(vec![load(0,0,1,8)],json!("Return"))}),
        &[8],
    );
    assert!(l.execute(&missing).unwrap_err().contains("unbound"));
}
#[test]
fn dag_export_roundtrips_through_existing_typed_ir() {
    let t = binary(BinaryOp::Add, var("i.a", 8), var("s.q", 8), 8, false, false).unwrap();
    let transition = Transition {
        next: Env::from([("q".into(), t.clone())]),
        outputs: Env::from([("sum".into(), t)]),
        statistics: Statistics::default(),
    };
    let out = transition.to_json(false).unwrap();
    assert_eq!(out["next"]["q"], out["outputs"]["sum"]);
    let inline = transition.to_json(true).unwrap();
    let mut lower = Lower::default();
    let env = Env::from([("i.a".into(), var("i.a", 8)), ("s.q".into(), var("s.q", 8))]);
    prove(
        lower.expr(&inline["next"]["q"], &env).unwrap(),
        transition.next["q"].clone(),
    );
}

fn compiled_unit(u: Unit) -> (Value, Value) {
    let signals = [
        ("clk", 0, 1, "Input"),
        ("en", 1, 1, "Input"),
        ("a", 2, 8, "Input"),
        ("q", 3, 8, "Output"),
    ];
    let meta =
        |w| json!({"width":w,"array_dims":[],"is_4state":false,"kind":"Other","type_kind":"Bit"});
    let code = json!({"status":"compiled_only_not_verified","four_state":false,"allow_always_ff_function_effects":true,"allowed_diagnostics":[],"runtime_event_sites":[],"frontend_lookup":"","signals":signals.iter().map(|(name,id,w,kind)|json!({"path":[name],"instances":[],"address":address(*id,0),"kind":kind,"signed":false,"metadata":meta(*w)})).collect::<Vec<_>>(),"design":{"state_objects":signals.iter().map(|(_,id,w,_)|json!({"address":address(*id,0),"metadata":meta(*w)})).collect::<Vec<_>>(),"ordered_events":[address(0,0)],"cascaded_events":[],"event_aliases":[],"reset_clocks":[],"initial_state":[]},"sir":{"eval_comb":[],"eval_apply_ffs":[{"event":address(0,0),"units":[u]}],"eval_comb_apply_ffs":[],"eval_only_ffs":[],"apply_ffs":[]}});
    let config = json!({"event":"clk","inputs":{"en":{"type":"bool"},"a":{"type":{"bv":8}}},"state":{"q":{"type":{"bv":8}}},"outputs":{"q":{"signal":"q","type":{"bv":8}}}});
    (code, config)
}
#[test]
fn public_bindings_preserve_arbitrary_prestate_and_preedge_outputs() {
    let u = unit(
        json!({"0":block(vec![load(0,1,0,1)],json!({"Branch":{"cond":0,"true_block":[1,[]],"false_block":[2,[]]}})),"1":block(vec![load(1,2,0,8),store(3,0,1,8)],json!({"Jump":[2,[]]})),"2":block(vec![],json!("Return"))}),
        &[1, 8],
    );
    let (code, mut config) = compiled_unit(u);
    let t = lift_json(&code, &config).unwrap();
    prove(t.outputs["q"].clone(), var("s.q", 8));
    prove(
        t.next["q"].clone(),
        ite(
            ir::var("i.en".into(), Sort::Bool),
            var("i.a", 8),
            var("s.q", 8),
        ),
    );
    config["overrides"] = json!({"en":false});
    let hold = lift_json(&code, &config).unwrap();
    assert_eq!(hold.to_json(true).unwrap()["next"]["q"], "s.q");
    config["overrides"] = json!({"en":true});
    let reset = lift_json(&code, &config).unwrap();
    assert_eq!(reset.to_json(true).unwrap()["next"]["q"], "i.a");
    // A malformed/partial reset remains state-dependent; it is never zero-filled.
    witness(not(eq(hold.next["q"].clone(), bv(8, 0))));
}
#[test]
fn bad_bindings_and_malformed_sir_fail_closed_without_panics() {
    let u = unit(json!({"0":block(vec![],json!("Return"))}), &[]);
    let (code, config) = compiled_unit(u);
    for change in [0, 1, 2, 3] {
        let mut config = config.clone();
        match change {
            0 => config["inputs"]["alias"] = json!({"signal":"q","type":{"bv":8}}),
            1 => config["inputs"]["a"]["name"] = json!("en"),
            2 => config["state"]["q"]["expr"] = json!(["bv", 8, 0]),
            _ => config["state"]["q"]["type"] = json!({"bv":4}),
        };
        assert!(lift_json(&code, &config).is_err());
    }
    let mut malformed = code.clone();
    malformed["sir"]["eval_apply_ffs"][0]["units"][0]["blocks"]["0"]["instructions"] =
        json!([{"Store":[]}]);
    assert!(lift_json(&malformed, &config).is_err());
    let mut malformed = code.clone();
    malformed["sir"]["eval_apply_ffs"][0]["units"][0]["blocks"]["0"]["terminator"] =
        json!({"Jump":[99,[]]});
    assert!(lift_json(&malformed, &config).is_err());
    let mut malformed = code.clone();
    malformed["design"]["state_objects"]
        .as_array_mut()
        .unwrap()
        .push(code["design"]["state_objects"][0].clone());
    assert!(lift_json(&malformed, &config).is_err());
}
#[test]
fn typed_width_boundaries_and_signed_promotion_are_explicit() {
    assert!(unary(UnaryOp::Minus, var("a", 4), 8, false).is_err());
    assert!(binary(BinaryOp::Eq, var("a", 4), var("b", 8), 1, true, true).is_err());
    assert!(binary(BinaryOp::LtS, var("a", 4), var("b", 4), 8, true, true).is_err());
    // SIR arithmetic promotes a signed lhs but treats the rhs as unsigned;
    // source context casts are already separate typed SIR instructions.
    prove(
        binary(BinaryOp::Add, var("a", 4), var("b", 4), 8, true, true).unwrap(),
        op(
            "bvadd",
            8,
            resize(var("a", 4), 8, true),
            resize(var("b", 4), 8, false),
        ),
    );
    // A signed declaration cannot change the logical right-shift's zero fill.
    prove(
        binary(BinaryOp::Shr, var("a", 4), var("b", 8), 8, true, false).unwrap(),
        op("bvlshr", 8, resize(var("a", 4), 8, false), var("b", 8)),
    );
    let overflow = unit(
        json!({"0":block(vec![json!({"Imm":[0,{"payload":[16],"mask":[0]}]})],json!("Return"))}),
        &[4],
    );
    let mut l = lifter(&[]);
    assert!(l.execute(&overflow).unwrap_err().contains("exceeds"));
}
#[test]
fn symbolic_sparse_branch_mask_and_commit_clear_preserve_holds() {
    let mut l = lifter(&[(0, 1, 1, "en"), (1, 8, 1, "a"), (2, 8, 1, "q")]);
    let u = unit(
        json!({"0":block(vec![load(0,0,0,1)],json!({"Branch":{"cond":0,"true_block":[1,[]],"false_block":[2,[]]}})),"1":block(vec![load(1,1,0,8),store(2,2,1,8)],json!({"Jump":[2,[]]})),"2":block(vec![commit(2,2,0,8),commit(2,2,0,8)],json!("Return"))}),
        &[1, 8],
    );
    l.execute(&u).unwrap();
    prove(
        l.frame.state[&(0, 2, 0)].read(0, 8).unwrap(),
        ite(truth(var("en0", 1)), var("a0", 8), var("q0", 8)),
    );
    assert_eq!(l.frame.state[&(0, 2, 2)].cells[0].mask, bv(8, 0));
}
#[test]
fn four_state_unknown_initializers_and_event_ambiguity_reject() {
    let u = unit(json!({"0":block(vec![],json!("Return"))}), &[]);
    let (code, config) = compiled_unit(u);
    for mode in [0, 1, 2, 3, 4, 5] {
        let mut bad = code.clone();
        match mode {
            0 => bad["four_state"] = json!(true),
            1 => {
                bad.as_object_mut().unwrap().remove("four_state");
            }
            2 => {
                bad["design"]["initial_state"] = json!([{"address":address(3,0),"data":{"Writes":[{"bit_offset":0,"bit_width":8,"value_bytes":[255],"mask_bytes":[1]}]}}])
            }
            3 => bad["design"]["cascaded_events"] = json!([address(0, 0)]),
            4 => {
                bad["design"]["event_aliases"] = json!([
                    [address(0, 0), address(1, 0)],
                    [address(1, 0), address(0, 0)]
                ])
            }
            _ => bad["sir"]["eval_apply_ffs"]
                .as_array_mut()
                .unwrap()
                .push(code["sir"]["eval_apply_ffs"][0].clone()),
        };
        assert!(lift_json(&bad, &config).is_err(), "mode {mode}");
    }
}
#[test]
fn input_binding_cannot_hide_state_or_unbound_storage() {
    let u = unit(
        json!({"0":block(vec![load(0,2,0,8),store(3,0,0,8)],json!("Return"))}),
        &[8],
    );
    let (code, mut config) = compiled_unit(u);
    config["inputs"]["a"]["expr"] = json!("s.q");
    assert!(lift_json(&code, &config).is_err());
    config["inputs"].as_object_mut().unwrap().remove("a");
    assert!(lift_json(&code, &config).unwrap_err().contains("unbound"));
}

#[test]
fn exported_array_width_is_total_not_per_element() {
    let shape = |width, array_dims: &[usize]| {
        storage_shape(&VariableMetadata {
            width,
            is_4state: false,
            kind: DomainKind::Other,
            type_kind: PortTypeKind::Bit,
            array_dims: array_dims.to_vec(),
        })
    };
    assert_eq!(shape(64, &[2]).unwrap(), (32, 2));
    assert_eq!(shape(512, &[4, 4]).unwrap(), (32, 16));
    for (width, array_dims) in [(65, &[2]), (64, &[0]), (131072, &[4096])] {
        assert!(shape(width, array_dims).is_err());
    }
}

#[test]
fn typed_export_round_trips_the_actual_wire_format() {
    let raw = include_str!("../tests/fixtures/array_read_write2.json");
    let code: Compiled = serde_json::from_str(raw).unwrap();
    let wire: Value = serde_json::from_str(&code.to_json().unwrap()).unwrap();
    assert_eq!(wire, serde_json::from_str::<Value>(raw).unwrap());
}
#[test]
fn actual_veryl_array_layout_read_old_write_and_reset() {
    let code: Compiled =
        serde_json::from_str(include_str!("../tests/fixtures/array_read_write2.json")).unwrap();
    let mut config = json!({"event":"clk","inputs":{"rst":{"type":"bool"},"we":{"type":"bool"},"wa":{"type":{"bv":1}},"wd":{"type":{"bv":32}},"ra0":{"type":{"bv":1}}},"state":{"m0":{"signal":"mem","element":0,"type":{"bv":32}},"m1":{"signal":"mem","element":1,"type":{"bv":32}},"q0":{"type":{"bv":32}}},"outputs":{},"overrides":{"rst":false}});
    let t = lift(&code, &config).unwrap();
    for index in 0..2 {
        prove(
            t.next[&format!("m{index}")].clone(),
            ite(
                and(
                    ir::var("i.we".into(), Sort::Bool),
                    eq(var("i.wa", 1), bv(1, index)),
                ),
                var("i.wd", 32),
                var(&format!("s.m{index}"), 32),
            ),
        );
    }
    prove(
        t.next["q0"].clone(),
        ite(
            eq(var("i.ra0", 1), bv(1, 0)),
            var("s.m0", 32),
            var("s.m1", 32),
        ),
    );
    config["overrides"] = json!({"rst":true});
    let reset = lift(&code, &config).unwrap();
    for term in reset.next.values() {
        assert_eq!(term, &bv(32, 0));
    }
}

#[test]
fn guarded_word_updates_match_the_original_bit_mask_encoding() {
    for w in [2, 4, 8, 16, 32, 64] {
        let g = ir::var("g".into(), Sort::Bool);
        let h = ir::var("h".into(), Sort::Bool);
        let mut a = Storage::new(w, 1, true);
        a.write(0, w, var("a", w), g.clone()).unwrap();
        a.write(0, w, var("b", w), h.clone()).unwrap();
        let mut c = Storage::new(w, 1, true);
        c.write(0, w, var("c", w), h).unwrap();
        let joined = Storage::merge(g, &a, &c).unwrap();
        for storage in [&a, &joined] {
            let cell = &storage.cells[0];
            let old = var("old", w);
            let masked = op(
                "bvor",
                w,
                op("bvand", w, old.clone(), bitnot(cell.mask.clone())),
                op("bvand", w, cell.value.clone().unwrap(), cell.mask.clone()),
            );
            let word = apply_word_update(
                cell.whole_word.as_ref().unwrap(),
                &old,
                &mut Default::default(),
            );
            prove(word, masked);
        }
    }
}
#[test]
fn partial_writes_drop_word_fast_path_until_unconditional_overwrite() {
    let mut storage = Storage::new(8, 1, true);
    storage.write(0, 8, var("a", 8), b(true)).unwrap();
    storage.write(2, 3, var("part", 3), b(true)).unwrap();
    assert!(storage.cells[0].whole_word.is_none());
    storage
        .write(0, 8, var("b", 8), ir::var("g".into(), Sort::Bool))
        .unwrap();
    assert!(storage.cells[0].whole_word.is_none());
    storage.write(0, 8, var("c", 8), b(true)).unwrap();
    assert!(storage.cells[0].whole_word.is_some());
    prove(
        apply_word_update(
            storage.cells[0].whole_word.as_ref().unwrap(),
            &var("old", 8),
            &mut Default::default(),
        ),
        var("c", 8),
    );
}

#[test]
fn split_initialization_requires_every_bit_before_reading() {
    for w in [2, 8, 32, 64] {
        let mut storage = Storage::new(w, 1, false);
        let input = var("input", w);
        storage
            .write(
                (w - 1) as usize,
                1,
                extract(input.clone(), w - 1, 1),
                b(true),
            )
            .unwrap();
        assert!(storage.read(0, w).unwrap_err().contains("unbound"));
        storage
            .write(0, w - 1, extract(input.clone(), 0, w - 1), b(true))
            .unwrap();
        prove(storage.read(0, w).unwrap(), input);
    }
}

#[test]
fn incomplete_initialization_rejects_reads_conditional_writes_and_joins() {
    let mut storage = Storage::new(8, 1, false);
    storage.write(0, 4, bv(4, 10), b(true)).unwrap();
    // Overlapping writes do not count twice toward coverage.
    storage.write(2, 4, bv(4, 3), b(true)).unwrap();
    assert!(storage.read(0, 8).is_err());
    // Even a defined slice stays unreadable until the entire lane is initialized.
    assert!(storage.read(0, 2).is_err());
    assert!(storage.read(6, 2).is_err());
    assert!(
        storage
            .write(6, 2, bv(2, 1), ir::var("g".into(), Sort::Bool))
            .is_err()
    );
    assert!(Storage::merge(b(true), &storage, &storage).is_err());
    storage.write(6, 2, bv(2, 2), b(true)).unwrap();
    prove(storage.read(0, 8).unwrap(), bv(8, 142));
}

#[test]
fn whole_write_replaces_pending_initialization() {
    let mut storage = Storage::new(8, 1, false);
    storage.write(3, 2, bv(2, 3), b(true)).unwrap();
    storage.write(0, 8, bv(8, 42), b(true)).unwrap();
    assert!(storage.cells[0].pending_bits.is_empty());
    prove(storage.read(0, 8).unwrap(), bv(8, 42));
}

#[test]
fn split_initialization_is_checked_at_public_outputs() {
    let empty = unit(json!({"0":block(vec![],json!("Return"))}), &[]);
    let (mut code, mut config) = compiled_unit(empty);
    config["state"] = json!({});
    let first = json!({"Store":[address(3,0),{"Static":4},4,0,[],[]]});
    let second = json!({"Store":[address(3,0),{"Static":0},4,0,[],[]]});
    let comb = unit(
        json!({"0":block(vec![load(0,2,0,4),first.clone(),second],json!("Return"))}),
        &[4],
    );
    code["sir"]["eval_comb"] = json!([comb]);
    let lifted = lift_json(&code, &config).unwrap();
    let nibble = extract(var("i.a", 8), 0, 4);
    prove(
        lifted.outputs["q"].clone(),
        cat(nibble.clone(), nibble).unwrap(),
    );
    code["sir"]["eval_comb"] = json!([unit(
        json!({"0":block(vec![load(0,2,0,4),first],json!("Return"))}),
        &[4]
    )]);
    assert!(lift_json(&code, &config).unwrap_err().contains("unbound"));
}

#[test]
fn unbound_cross_lane_fragments_require_complete_individual_lanes() {
    let mut storage = Storage::new(8, 2, false);
    let middle = var("middle", 8);
    let low = var("low", 4);
    let high = var("high", 4);
    storage.write(4, 8, middle.clone(), b(true)).unwrap();
    assert!(storage.read(0, 8).is_err());
    assert!(storage.read(8, 8).is_err());
    assert!(storage.read(0, 16).is_err());
    assert!(
        storage
            .write(
                0,
                8,
                var("replacement", 8),
                ir::var("guard".into(), Sort::Bool)
            )
            .is_err()
    );
    storage.write(0, 4, low.clone(), b(true)).unwrap();
    prove(
        storage.read(0, 8).unwrap(),
        cat(extract(middle.clone(), 0, 4), low.clone()).unwrap(),
    );
    assert!(storage.read(8, 8).is_err());
    assert!(storage.read(0, 16).is_err());
    storage.write(12, 4, high.clone(), b(true)).unwrap();
    prove(
        storage.read(0, 16).unwrap(),
        cat(high, cat(middle, low).unwrap()).unwrap(),
    );
}
