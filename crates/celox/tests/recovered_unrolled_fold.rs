use celox::SimulatorBuilder;
use num_bigint::BigUint;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

const MULTI_STATE_LOOP: &str = r#"
    module Top (
        en       : input  logic,
        bits     : input  logic<4>,
        out_data : output logic<3>,
    ) {
        var valid: logic;
        var age  : logic<3>;
        var data : logic<3>;

        always_comb {
            valid = 1'b0;
            age   = 3'd0;
            data  = 3'd0;
            if en {
                for i in 0..4 {
                    let ai: logic<3> = (i as 3);
                    if bits[i] && (!valid || ai >: age) {
                        valid = 1'b1;
                        age   = ai;
                        data  = ai;
                    }
                }
            }
            out_data  = data;
        }
    }
"#;

all_backends! {

// Independent reductions without feedback retain their shared loop and
// induction variable. Check both reductions across changing external inputs.
fn recovered_independent_reductions_share_input_correctly(sim) {
    @ignore_on(sv);
    @setup { let code = r#"
        module Top (
            values: input logic<32>,
            sum: output logic<8>,
            parity: output logic<8>,
        ) {
            always_comb {
                sum = 8'd0;
                parity = 8'd0;
                for h in 0..4 {
                    sum = sum + values[h * 8+:8];
                    parity = parity ^ values[h * 8+:8];
                }
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");
    let values = sim.signal("values");
    let sum = sim.signal("sum");
    let parity = sim.signal("parity");
    for value in [0u32, 0x04030201, 0xffffffff, 0x12345678, 0x800100ff, 0] {
        sim.modify(|io| io.set(values, value)).unwrap();
        let bytes = value.to_le_bytes();
        let expected_sum = bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        let expected_parity = bytes.iter().fold(0u8, |acc, byte| acc ^ *byte);
        assert_eq!(sim.get(sum), expected_sum.into());
        assert_eq!(sim.get(parity), expected_parity.into());
    }
}

// Read enables determine the write grant, but never read the granted writes.
// Recovering both reductions as one atomic fold must not invent that feedback.
fn recovered_mmio_read_write_dependencies_are_independent(sim) {
    @ignore_on(sv);
    @setup { let code = r#"
        module Top (
            reads : input logic<2>,
            writes: input logic<2>,
            read_enable : output logic,
            write_enable: output logic,
        ) {
            var grant: logic;
            var fired: logic<2>;
            assign grant = !read_enable;
            assign fired = if grant ? writes : 2'd0;
            always_comb {
                read_enable = 1'b0;
                write_enable = 1'b0;
                for h in 0..2 {
                    if reads[h] { read_enable = 1'b1; }
                    if fired[h] { write_enable = 1'b1; }
                }
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");
    let reads = sim.signal("reads");
    let writes = sim.signal("writes");
    let read_enable = sim.signal("read_enable");
    let write_enable = sim.signal("write_enable");
    for r in [0u8, 1, 2, 3, 0] {
        for w in [0u8, 1, 2, 3, 0] {
            sim.modify(|io| { io.set(reads, r); io.set(writes, w); }).unwrap();
            assert_eq!(sim.get(read_enable), ((r != 0) as u8).into());
            assert_eq!(sim.get(write_enable), ((r == 0 && w != 0) as u8).into());
        }
    }
}

fn recovered_unrolled_store_forward_selects_older_entry(sim) {
    @ignore_on(sv);
    @setup { let code = r#"
        module StoreShadow (
            store_idx : input  logic<5>,
            store_addr: input  logic<64>,
            store_data: input  logic<64>,
            addresses : output logic<64> [32],
            values    : output logic<64> [32],
            widths    : output logic<3>  [32],
            valid     : output logic     [32],
        ) {
            always_comb {
                for i in 0..32 {
                    addresses[i] = 64'd0;
                    values[i]    = 64'd0;
                    widths[i]    = 3'd0;
                    valid[i]     = 1'b0;
                }
                addresses[store_idx] = store_addr;
                values[store_idx]    = store_data;
                widths[store_idx]    = 3'b010;
                valid[store_idx]     = 1'b1;
            }
        }

        module Top (
            en        : input  logic,
            head      : input  logic<5>,
            load_idx  : input  logic<5>,
            store_idx : input  logic<5>,
            load_addr : input  logic<64>,
            store_addr: input  logic<64>,
            store_data: input  logic<64>,
            mask       : output logic,
            byte0      : output logic<8>,
        ) {
            var rob_addr: logic<64> [32];
            var rob_data: logic<64> [32];
            var rob_f3  : logic<3>  [32];
            var rob_fwd : logic     [32];
            inst shadow: StoreShadow (
                store_idx,
                store_addr,
                store_data,
                addresses: rob_addr,
                values   : rob_data,
                widths   : rob_f3,
                valid    : rob_fwd,
            );

            var ls_addr: logic<64> [32];
            var ls_data: logic<64> [32];
            var ls_f3  : logic<3>  [32];
            var ls_fwd : logic     [32];
            assign ls_addr = rob_addr;
            assign ls_data = rob_data;
            assign ls_f3   = rob_f3;
            assign ls_fwd  = rob_fwd;

            var found: logic;
            var found_age: logic<5>;
            var found_byte: logic<8>;
            always_comb {
                found      = 1'b0;
                found_age  = 5'd0;
                found_byte = 8'd0;
                if en {
                    for i in 0..32 {
                        let age_i: logic<5> = (i as 5) - head;
                        let load_age: logic<5> = load_idx - head;
                        let sa: logic<64> = ls_addr[i];
                        let la: logic<64> = load_addr;
                        let covered: logic = ls_fwd[i] && (age_i <: load_age)
                            && (la >= sa) && (la <: sa + ((5'd1 << ls_f3[i][1:0]) as 64));
                        let offset: logic<64> = la - sa;
                        let shifted: logic<64> = ls_data[i] >> {offset[2:0], 3'b000};
                        if covered && (!found || age_i >: found_age) {
                            found      = 1'b1;
                            found_age  = age_i;
                            found_byte = shifted[7:0];
                        }
                    }
                }
            }
            assign mask  = found;
            assign byte0 = found_byte;
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");

    let en = sim.signal("en");
    let head = sim.signal("head");
    let load_idx = sim.signal("load_idx");
    let store_idx = sim.signal("store_idx");
    let load_addr = sim.signal("load_addr");
    let store_addr = sim.signal("store_addr");
    let store_data = sim.signal("store_data");
    let mask = sim.signal("mask");
    let byte0 = sim.signal("byte0");

    for (head_value, store_value, load_value) in [(0u8, 3u8, 4u8), (29, 31, 1), (17, 18, 21)] {
        sim.modify(|io| {
            io.set(en, 1u8);
            io.set(head, head_value);
            io.set(store_idx, store_value);
            io.set(load_idx, load_value);
            io.set(store_addr, 0x8000_2004u64);
            io.set(load_addr, 0x8000_2004u64);
            io.set(store_data, 0x1234_5678u64);
        }).unwrap();
        assert_eq!(sim.get(mask), 1u8.into(), "head={head_value} store={store_value} load={load_value}");
        assert_eq!(sim.get(byte0), 0x78u8.into(), "head={head_value} store={store_value} load={load_value}");
    }
}

fn recovered_unrolled_multi_state_priority(sim) {
    @ignore_on(sv);
    @setup { let code = MULTI_STATE_LOOP; }
    @build SimulatorBuilder::new(code, "Top");

    let en = sim.signal("en");
    let bits = sim.signal("bits");
    let out_data = sim.signal("out_data");

    for enabled in [0u8, 1u8] {
        for input in 0u8..16 {
            sim.set(en, enabled);
            sim.set(bits, input);
            sim.eval_comb().unwrap();

            let expected_priority = if enabled == 0 || input == 0 {
                0
            } else {
                (7 - input.leading_zeros()) as u8
            };
            assert_eq!(
                sim.get(out_data),
                expected_priority.into(),
                "data for en={enabled} bits={input:04b}"
            );
        }
    }
}

fn recovered_unrolled_guard_uses_procedural_four_state_truth(sim) {
    @ignore_on(sv);
    @setup { let code = r#"
        module Top (
            cond : input  logic<8>,
            bits : input  logic<4>,
            value: output logic<4>,
        ) {
            var state: logic<4>;
            always_comb {
                state = 4'd0;
                if cond {
                    for i in 0..4 {
                        state[i] = bits[i];
                    }
                }
                value = state;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top").four_state(true);

    let cond = sim.signal("cond");
    let bits = sim.signal("bits");
    let value = sim.signal("value");
    let unknown = BigUint::from(1u8) << 2usize;

    sim.modify(|io| {
        io.set(bits, 0b1010u8);
        io.set_four_state(cond, unknown.clone(), unknown.clone());
    })
    .unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(
        sim.get_four_state(value),
        (BigUint::from(0u8), BigUint::from(0u8)),
        "an entirely unknown procedural guard must not enter the recovered loop",
    );

    let known_one = BigUint::from(1u8) << 7usize;
    sim.modify(|io| {
        io.set_four_state(cond, &known_one | &unknown, unknown.clone());
    })
    .unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(
        sim.get_four_state(value),
        (BigUint::from(0b1010u8), BigUint::from(0u8)),
        "a known one bit must make the procedural guard true despite another unknown bit",
    );
}

}
