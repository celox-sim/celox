use super::*;

sv_backends! {
    fn structure_elements_have_no_packed_dimensions(sim) {
        // IEEE 1800-2023 20.7: an unpacked structure is not an array, so a
        // queue of them has its one unpacked dimension, though the structure
        // is held in a record like a wide integral element.
        @setup {
            let source = r#"module Top(output int value);
                typedef struct { bit [99:0] v; } s_t;
                s_t records[$];
                logic [129:0] wide[$];
                initial value = $dimensions(records) * 10 + $dimensions(wide);
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("dimensions.sv"))], "Top");
        sim.run_until(0).unwrap();
        let value = sim.signal("value");
        assert_eq!(sim.get(value), BigUint::from(12u8));
    }

    // A member of a container element of packed structure type selects the
    // bits of the element that the structure gives the member (IEEE
    // 1800-2023 7.2.1), and a select of an element selects its bits
    // (11.5.1), whether the element is held by value or, wider than 64
    // bits, in a record.
    fn members_and_selects_of_packed_elements(sim) {
        @setup {
            let source = r#"module Top(
                output logic [7:0] id, last_id, inner, byte_of, part, wide_id, bits,
                output logic [99:0] data,
                output logic [63:0] cycle,
                output logic last, top_bit, wide_bit);
                typedef struct packed { logic [3:0] hi; logic [3:0] lo; } pair_t;
                typedef struct packed { logic last; logic [7:0] id; pair_t pair; } narrow_t;
                typedef struct packed {
                    logic last; logic [7:0] id; logic [99:0] data; logic [63:0] cycle;
                } wide_t;
                narrow_t narrow[$];
                wide_t wide[$];
                wide_t keyed[int];
                logic [99:0] vectors[];
                narrow_t n;
                wide_t w;
                int i = 1, j = 2;
                initial begin
                    n = '{last: 1'b0, id: 8'h12, pair: '{hi: 4'ha, lo: 4'h5}};
                    narrow.push_back(n);
                    n.id = 8'h34;
                    n.pair.hi = 4'hc;
                    narrow.push_back(n);
                    w = '{last: 1'b1, id: 8'h9c, data: {4'h7, 96'h0123_4567_89ab_cdef_0f1e_2d3c}, cycle: 64'hfeed_face_cafe_beef};
                    wide.push_back(w);
                    keyed[7] = w;
                    vectors = new[2];
                    vectors[1] = {8'ha5, 92'h0} | 100'h66_0000;
                    id = narrow[0].id;
                    last_id = narrow[$].id;
                    inner = {narrow[i].pair.hi, narrow[0].pair.lo};
                    part = narrow[i].id[5:2];
                    bits = narrow[1][11:4];
                    data = wide[0].data;
                    cycle = wide[0].cycle;
                    last = wide[0].last;
                    wide_id = keyed[7].id;
                    byte_of = wide[0].data[23:16];
                    top_bit = wide[0][172];
                    wide_bit = vectors[i][99];
                    if (vectors[1][8 * j +: 8] !== 8'h66 || vectors[i][99:92] !== 8'ha5)
                        wide_bit = 1'b0;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("element_members.sv"))], "Top");
        sim.run_until(0).unwrap();
        for (name, value) in [
            ("id", 0x12u64),
            ("last_id", 0x34),
            ("inner", 0xc5),
            ("part", 0xd),
            ("bits", 0x4c),
            ("cycle", 0xfeed_face_cafe_beef),
            ("last", 1),
            ("wide_id", 0x9c),
            ("byte_of", 0x1e),
            ("top_bit", 1),
            ("wide_bit", 1),
        ] {
            assert_eq!(sim.get(sim.signal(name)), BigUint::from(value), "{name}");
        }
        assert_eq!(
            sim.get(sim.signal("data")),
            BigUint::parse_bytes(b"70123456789abcdef0f1e2d3c", 16).unwrap()
        );
    }
}
