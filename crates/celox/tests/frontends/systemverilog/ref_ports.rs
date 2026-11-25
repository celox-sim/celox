use super::*;

sv_backends! {
    fn ref_ports_support_ansi_non_ansi_chaining_and_always_ff_clk(sim) {
        @setup {
            let source = r#"
                module Leaf(const ref logic clk, ref logic [7:0] acc, input logic [7:0] inc);
                    always_ff @(posedge clk) acc <= acc + inc;
                endmodule
                module Mid(clk, acc, ro, mirrored);
                    const ref logic clk;
                    ref logic [7:0] acc;
                    const ref logic [7:0] ro;
                    output logic [7:0] mirrored;
                    Leaf u_leaf(.clk(clk), .acc(acc), .inc(8'd3));
                    assign mirrored = ro;
                endmodule
                module Top(
                    input var logic clk,
                    output logic [7:0] acc_out,
                    output logic [7:0] mirrored_out
                );
                    logic [7:0] acc = 8'd10;
                    Mid u_mid(.clk(clk), .acc(acc), .ro(acc), .mirrored(mirrored_out));
                    assign acc_out = acc;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_chain.sv"))], "Top");
        let clk = sim.signal("clk");
        let acc_out = sim.signal("acc_out");
        let mirrored_out = sim.signal("mirrored_out");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(acc_out), 10u8.into());
        assert_eq!(sim.get(mirrored_out), 10u8.into());

        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(acc_out), 13u8.into());
        assert_eq!(sim.get(mirrored_out), 13u8.into());
    }

    fn ref_ports_support_classes_real_containers_and_programs(sim) {
        @setup {
            let source = r#"
                package p;
                    timeunit 1ns / 1ns;
                    class Box;
                        int v;
                        function new(int init); v = init; endfunction
                    endclass
                    typedef enum logic [1:0] { E0 = 2'd0, E1 = 2'd1, E2 = 2'd2 } mode_e;
                endpackage
                program Driver(
                    const ref logic clk,
                    ref p::Box b,
                    ref real r,
                    ref int dyn[],
                    ref int aa[string],
                    ref p::mode_e m
                );
                    timeunit 1ns / 1ns;
                    initial begin
                        @(posedge clk);
                        b = new(99);
                        r = 2.5;
                        dyn = new[2];
                        dyn[0] = 11;
                        dyn[1] = 22;
                        aa["k"] = 7;
                        m = p::E2;
                        #5;
                    end
                endprogram
                module Top(
                    input var logic clk,
                    output int box_v,
                    output logic [63:0] r_bits,
                    output int dyn_sum,
                    output int aa_v,
                    output logic [1:0] mode_v
                );
                    timeunit 1ns / 1ns;
                    p::Box box_h;
                    real r_val = 1.0;
                    int dyn[];
                    int aa[string];
                    p::mode_e mode = p::E1;
                    Driver drv(.clk(clk), .b(box_h), .r(r_val), .dyn(dyn), .aa(aa), .m(mode));
                    initial begin
                        box_h = new(4);
                        @(posedge clk);
                        #1;
                        box_v = box_h.v;
                        r_bits = $realtobits(r_val);
                        dyn_sum = dyn[0] + dyn[1];
                        aa_v = aa["k"];
                        mode_v = mode;
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_types.sv"))], "Top");
        let clk = sim.signal("clk");
        sim.run_until(0).unwrap();
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("box_v")), 99u32.into());
        assert_eq!(
            sim.get(sim.signal("r_bits")),
            BigUint::from(2.5f64.to_bits())
        );
        assert_eq!(sim.get(sim.signal("dyn_sum")), 33u32.into());
        assert_eq!(sim.get(sim.signal("aa_v")), 7u32.into());
        assert_eq!(sim.get(sim.signal("mode_v")), 2u8.into());
    }

    fn ref_ports_support_instance_array_broadcast_and_plain_always(sim) {
        @setup {
            let source = r#"
                module Obs(const ref int v, output int out_v);
                    assign out_v = v + 1;
                endmodule
                module PlainWriter(ref int v, const ref logic trig);
                    always @(posedge trig) v = v + 5;
                endmodule
                module Top(input var logic trig, output int y0, output int y1, output int cur);
                    int shared = 10;
                    int obs[2];
                    Obs u_obs[2](.v(shared), .out_v(obs));
                    PlainWriter w(.v(shared), .trig(trig));
                    always @(posedge trig) shared = shared + 2;
                    assign y0 = obs[0];
                    assign y1 = obs[1];
                    assign cur = shared;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_bcast.sv"))], "Top");
        let trig = sim.signal("trig");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("cur")), 10u32.into());
        assert_eq!(sim.get(sim.signal("y0")), 11u32.into());
        assert_eq!(sim.get(sim.signal("y1")), 11u32.into());
        sim.modify(|io| io.set(trig, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("cur")), 17u32.into());
        assert_eq!(sim.get(sim.signal("y0")), 18u32.into());
        assert_eq!(sim.get(sim.signal("y1")), 18u32.into());
    }

    // Verilator 5.054 rejects IEEE 1800-2023 6.22.2 equivalent types on `ref`
    // ports (`bit signed [31:0]` <-> `int`, anonymous/differing-offset fixed
    // unpacked arrays) in favor of strict matching/typedef identity.
    fn ref_ports_accept_ieee_equivalent_packed_and_unpacked_array_types(sim) {
        @setup {
            let source = r#"
                module Flip(ref int v, ref int a[4], ref int b[1:4]);
                    timeunit 1ns / 1ns;
                    initial begin
                        #1;
                        v = -v;
                        a[0] = a[0] + 10;
                        a[3] = a[3] + 20;
                        b[1] = b[1] + 5;
                        b[4] = b[4] + 6;
                    end
                endmodule
                module Top(output int v_out, output int a0, output int a3, output int b0, output int b3);
                    timeunit 1ns / 1ns;
                    bit signed [31:0] v;
                    int arr[0:3];
                    int arr2[0:3];
                    initial begin
                        v = 42;
                        arr[0] = 1;
                        arr[3] = 5;
                        arr2[0] = 100;
                        arr2[3] = 200;
                    end
                    Flip u(.v(v), .a(arr), .b(arr2));
                    always_comb begin
                        v_out = v;
                        a0 = arr[0];
                        a3 = arr[3];
                        b0 = arr2[0];
                        b3 = arr2[3];
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_equiv.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("v_out")), 42u32.into());
        assert_eq!(sim.get(sim.signal("a0")), 1u32.into());
        assert_eq!(sim.get(sim.signal("a3")), 5u32.into());
        assert_eq!(sim.get(sim.signal("b0")), 100u32.into());
        assert_eq!(sim.get(sim.signal("b3")), 200u32.into());
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("v_out")), (-42i32 as u32).into());
        assert_eq!(sim.get(sim.signal("a0")), 11u32.into());
        assert_eq!(sim.get(sim.signal("a3")), 25u32.into());
        assert_eq!(sim.get(sim.signal("b0")), 105u32.into());
        assert_eq!(sim.get(sim.signal("b3")), 206u32.into());
    }

    // Verilator 5.054 miscompiles `ref` bindings to unpacked-struct leaf
    // members into combinational copies that clobber parent initializers.
    fn ref_ports_alias_unpacked_struct_and_leaf_member(sim) {
        @setup {
            let source = r#"
                package p;
                    timeunit 1ns / 1ns;
                    typedef struct { int a; bit [7:0] b; } pair_t;
                endpackage
                module BumpField(ref int f);
                    timeunit 1ns / 1ns;
                    initial begin #1 f = f + 30; end
                endmodule
                module SumPair(const ref p::pair_t s, output int total);
                    timeunit 1ns / 1ns;
                    assign total = s.a + int'(s.b);
                endmodule
                module Top(output int sum_out, output int a_out);
                    timeunit 1ns / 1ns;
                    p::pair_t s;
                    initial begin s.a = 10; s.b = 8'd5; end
                    BumpField bf(.f(s.a));
                    SumPair sp(.s(s), .total(sum_out));
                    assign a_out = s.a;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_struct_leaf.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("a_out")), 10u32.into());
        assert_eq!(sim.get(sim.signal("sum_out")), 15u32.into());
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("a_out")), 40u32.into());
        assert_eq!(sim.get(sim.signal("sum_out")), 45u32.into());
    }

    fn ref_ports_support_nested_unpacked_structs(sim) {
        @setup {
            let source = r#"
                package p;
                    timeunit 1ns / 1ns;
                    typedef struct { int x; bit [7:0] y; } inner_t;
                    typedef struct { inner_t inner; int z; } outer_t;
                endpackage
                module InnerMut(ref p::inner_t inner);
                    timeunit 1ns / 1ns;
                    initial begin #1 inner.y = inner.y + 8'd7; end
                endmodule
                module OuterMut(ref p::outer_t s);
                    timeunit 1ns / 1ns;
                    InnerMut u_in(.inner(s.inner));
                    always_comb s.z = s.inner.x + int'(s.inner.y);
                endmodule
                module Top(output int x_out, output bit [7:0] y_out, output int z_out);
                    timeunit 1ns / 1ns;
                    p::outer_t s;
                    initial begin s.inner.x = 10; s.inner.y = 8'd5; end
                    OuterMut u(.s(s));
                    assign x_out = s.inner.x;
                    assign y_out = s.inner.y;
                    assign z_out = s.z;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_nested_struct.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("x_out")), 10u32.into());
        assert_eq!(sim.get(sim.signal("y_out")), 5u8.into());
        assert_eq!(sim.get(sim.signal("z_out")), 15u32.into());
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("x_out")), 10u32.into());
        assert_eq!(sim.get(sim.signal("y_out")), 12u8.into());
        assert_eq!(sim.get(sim.signal("z_out")), 22u32.into());
    }

    fn ref_ports_allow_same_instance_aliased_comb_and_ff_processes(sim) {
        @setup {
            let source = r#"
                module ChildComb(input int x, ref int a, ref int b);
                    timeunit 1ns / 1ns;
                    always_comb begin
                        a = x;
                        b = a + 5;
                    end
                endmodule
                module ChildFf(input var logic clk, input int x, ref int a, ref int b);
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a <= x;
                        b <= x + 10;
                    end
                endmodule
                module ChildFfSlices(
                    input var logic clk,
                    input logic [7:0] lo,
                    input logic [7:0] hi,
                    ref int a,
                    ref int b
                );
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a[7:0] <= lo;
                        b[15:8] <= hi;
                    end
                endmodule
                module Top(
                    input var logic clk,
                    input int x,
                    output int comb_aliased_out,
                    output int comb_unaliased_a,
                    output int comb_unaliased_b,
                    output int ff_aliased_out,
                    output int ff_slice_out
                );
                    timeunit 1ns / 1ns;
                    int vc = 0;
                    int vc2 = 0, vc3 = 0;
                    int vf = 0;
                    int vs = 0;
                    ChildComb u_comb(.x(x), .a(vc), .b(vc));
                    ChildComb u_comb_unaliased(.x(x), .a(vc2), .b(vc3));
                    ChildFf u_ff(.clk(clk), .x(x), .a(vf), .b(vf));
                    ChildFfSlices u_slices(.clk(clk), .lo(8'h34), .hi(8'h12), .a(vs), .b(vs));
                    assign comb_aliased_out = vc;
                    assign comb_unaliased_a = vc2;
                    assign comb_unaliased_b = vc3;
                    assign ff_aliased_out = vf;
                    assign ff_slice_out = vs;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_same_inst_alias.sv"))], "Top");
        let clk = sim.signal("clk");
        let x = sim.signal("x");
        let comb_aliased_out = sim.signal("comb_aliased_out");
        let comb_unaliased_a = sim.signal("comb_unaliased_a");
        let comb_unaliased_b = sim.signal("comb_unaliased_b");
        let ff_aliased_out = sim.signal("ff_aliased_out");
        let ff_slice_out = sim.signal("ff_slice_out");

        sim.modify(|io| {
            io.set(clk, 0u8);
            io.set(x, 10u32);
        })
        .unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(comb_aliased_out), 15u32.into());
        assert_eq!(sim.get(comb_unaliased_a), 10u32.into());
        assert_eq!(sim.get(comb_unaliased_b), 15u32.into());
        assert_eq!(sim.get(ff_aliased_out), 0u32.into());
        assert_eq!(sim.get(ff_slice_out), 0u32.into());

        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(ff_aliased_out), 20u32.into());
        assert_eq!(sim.get(ff_slice_out), 0x1234u32.into());

        sim.modify(|io| io.set(x, 25u32)).unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(comb_aliased_out), 30u32.into());
        assert_eq!(sim.get(comb_unaliased_a), 25u32.into());
        assert_eq!(sim.get(comb_unaliased_b), 30u32.into());
    }

    fn ref_ports_allow_disjoint_slice_drivers_and_const_ref_observer(sim) {
        @setup {
            let source = r#"
                module Obs(const ref logic [7:0] r, output logic [7:0] y);
                    timeunit 1ns / 1ns;
                    assign y = r;
                endmodule
                module DriveHi(ref logic [7:0] r);
                    timeunit 1ns / 1ns;
                    assign r[7:4] = 4'hc;
                endmodule
                module DriveHiFf(const ref logic clk, ref logic [7:0] r);
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) r[7:4] <= 4'h6;
                endmodule
                module DriveHiShifted(ref logic [7:0] r);
                    timeunit 1ns / 1ns;
                    assign r[7:4] = 4'h7;
                endmodule
                module Top(
                    input var logic clk,
                    output logic [7:0] obs_out,
                    output logic [7:0] comb_out,
                    output logic [7:0] ff_out,
                    output logic [7:0] shifted_out
                );
                    timeunit 1ns / 1ns;
                    logic [7:0] v;
                    assign v[3:0] = 4'h5;
                    assign v[7:4] = 4'ha;
                    Obs u_obs(.r(v), .y(obs_out));

                    logic [7:0] w;
                    assign w[3:0] = 4'h3;
                    DriveHi u_comb(.r(w));
                    assign comb_out = w;

                    logic [7:0] f = 8'd0;
                    always_ff @(posedge clk) f[3:0] <= 4'h9;
                    DriveHiFf u_ff(.clk(clk), .r(f));
                    assign ff_out = f;

                    logic [15:8] s;
                    assign s[11:8] = 4'h2;
                    DriveHiShifted u_shift(.r(s));
                    assign shifted_out = s;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_disjoint_slices.sv"))], "Top");
        let clk = sim.signal("clk");
        let obs_out = sim.signal("obs_out");
        let comb_out = sim.signal("comb_out");
        let ff_out = sim.signal("ff_out");
        let shifted_out = sim.signal("shifted_out");

        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(obs_out), 0xa5u8.into());
        assert_eq!(sim.get(comb_out), 0xc3u8.into());
        assert_eq!(sim.get(ff_out), 0x00u8.into());
        assert_eq!(sim.get(shifted_out), 0x72u8.into());

        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(ff_out), 0x69u8.into());
    }

    fn ref_ports_preserve_sibling_reads_after_blocking_always_ff(sim) {
        @setup {
            let source = r#"
                module ChildBlocking(input var logic clk, ref int a, const ref int b);
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a = 10;
                        a = b + 1;
                    end
                endmodule
                module ChildPartialBlocking(input var logic clk, ref int a, const ref int b);
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a[7:0] = 8'haa;
                        a[15:8] = b[7:0];
                    end
                endmodule
                module ChildNbaRead(
                    input var logic clk,
                    ref int a,
                    const ref int b,
                    output int y
                );
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a <= 10;
                        y <= b + 1;
                    end
                endmodule
                module ChildMixed(input var logic clk, ref int a, ref int b);
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a = 10;
                        b <= b + 5;
                    end
                endmodule
                module Top(
                    input var logic clk,
                    output int aliased_out,
                    output int unaliased_a,
                    output int unaliased_b,
                    output int partial_aliased_out,
                    output int partial_unaliased_a,
                    output int partial_unaliased_b,
                    output int nba_v_out,
                    output int nba_y_out,
                    output int mixed_aliased_out,
                    output int mixed_unaliased_a,
                    output int mixed_unaliased_b
                );
                    timeunit 1ns / 1ns;
                    int v1 = 3;
                    int v2 = 3, v3 = 50;
                    int p1 = 32'h1234_5678;
                    int p2 = 32'h1234_5678, p3 = 32'hdead_beef;
                    int vn = 3;
                    int m1 = 3;
                    int m2 = 3, m3 = 50;

                    ChildBlocking u_blk_alias(.clk(clk), .a(v1), .b(v1));
                    ChildBlocking u_blk_sep(.clk(clk), .a(v2), .b(v3));
                    ChildPartialBlocking u_part_alias(.clk(clk), .a(p1), .b(p1));
                    ChildPartialBlocking u_part_sep(.clk(clk), .a(p2), .b(p3));
                    ChildNbaRead u_nba(.clk(clk), .a(vn), .b(vn), .y(nba_y_out));
                    ChildMixed u_mix_alias(.clk(clk), .a(m1), .b(m1));
                    ChildMixed u_mix_sep(.clk(clk), .a(m2), .b(m3));

                    assign aliased_out = v1;
                    assign unaliased_a = v2;
                    assign unaliased_b = v3;
                    assign partial_aliased_out = p1;
                    assign partial_unaliased_a = p2;
                    assign partial_unaliased_b = p3;
                    assign nba_v_out = vn;
                    assign mixed_aliased_out = m1;
                    assign mixed_unaliased_a = m2;
                    assign mixed_unaliased_b = m3;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_sibling_blocking.sv"))], "Top");
        let clk = sim.signal("clk");
        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(0).unwrap();

        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(1).unwrap();

        assert_eq!(sim.get(sim.signal("aliased_out")), 11u32.into());
        assert_eq!(sim.get(sim.signal("unaliased_a")), 51u32.into());
        assert_eq!(sim.get(sim.signal("unaliased_b")), 50u32.into());

        assert_eq!(
            sim.get(sim.signal("partial_aliased_out")),
            0x1234_aaaau32.into()
        );
        assert_eq!(
            sim.get(sim.signal("partial_unaliased_a")),
            0x1234_efaau32.into()
        );
        assert_eq!(
            sim.get(sim.signal("partial_unaliased_b")),
            0xdead_beefu32.into()
        );

        assert_eq!(sim.get(sim.signal("nba_v_out")), 10u32.into());
        assert_eq!(sim.get(sim.signal("nba_y_out")), 4u32.into());

        assert_eq!(sim.get(sim.signal("mixed_aliased_out")), 15u32.into());
        assert_eq!(sim.get(sim.signal("mixed_unaliased_a")), 10u32.into());
        assert_eq!(sim.get(sim.signal("mixed_unaliased_b")), 55u32.into());
    }

    fn ref_ports_preserve_nba_order_across_aliased_ports(sim) {
        @setup {
            let source = r#"
                module ChildOrder(input var logic clk, ref int a, ref int b);
                    always_ff @(posedge clk) begin
                        a = 0;
                        b <= 1;
                        a <= 2;
                    end
                endmodule
                module ChildBranchOrder(input var logic clk, input var logic sel, ref int a, ref int b);
                    always_ff @(posedge clk) begin
                        a = 0;
                        if (sel) begin
                            b <= 1;
                            a <= 2;
                        end else begin
                            a <= 3;
                            b <= 4;
                        end
                    end
                endmodule
                module ChildPartialOrder(input var logic clk, ref int a, ref int b);
                    always_ff @(posedge clk) begin
                        a = 0;
                        b[7:0] <= 8'hff;
                        a[3:0] <= 4'h0;
                        b[1:0] <= 2'b11;
                    end
                endmodule
                module Top(
                    input var logic clk,
                    input var logic sel,
                    output int alias_ord,
                    output int sep_a,
                    output int sep_b,
                    output int alias_br,
                    output int sep_br_a,
                    output int sep_br_b,
                    output int alias_part,
                    output int sep_part_a,
                    output int sep_part_b
                );
                    int x = 9;
                    int y = 9, z = 9;
                    int bx = 9;
                    int by = 9, bz = 9;
                    int px = 32'h1234_5678;
                    int py = 32'h1234_5678, pz = 32'h1234_5600;

                    ChildOrder u_alias(.clk(clk), .a(x), .b(x));
                    ChildOrder u_sep(.clk(clk), .a(y), .b(z));
                    ChildBranchOrder u_br_alias(.clk(clk), .sel(sel), .a(bx), .b(bx));
                    ChildBranchOrder u_br_sep(.clk(clk), .sel(sel), .a(by), .b(bz));
                    ChildPartialOrder u_part_alias(.clk(clk), .a(px), .b(px));
                    ChildPartialOrder u_part_sep(.clk(clk), .a(py), .b(pz));

                    assign alias_ord = x;
                    assign sep_a = y;
                    assign sep_b = z;
                    assign alias_br = bx;
                    assign sep_br_a = by;
                    assign sep_br_b = bz;
                    assign alias_part = px;
                    assign sep_part_a = py;
                    assign sep_part_b = pz;
                endmodule
            "#;
        }
        @build celox::Simulator::from_sv_sources(vec![(source, Path::new("ref_nba_order.sv"))], "Top");
        let clk = sim.event("clk");
        let sel = sim.signal("sel");

        sim.modify(|io| io.set(sel, 1u8)).unwrap();
        sim.tick(clk).unwrap();

        assert_eq!(sim.get(sim.signal("alias_ord")), 2u32.into());
        assert_eq!(sim.get(sim.signal("sep_a")), 2u32.into());
        assert_eq!(sim.get(sim.signal("sep_b")), 1u32.into());

        assert_eq!(sim.get(sim.signal("alias_br")), 2u32.into());
        assert_eq!(sim.get(sim.signal("sep_br_a")), 2u32.into());
        assert_eq!(sim.get(sim.signal("sep_br_b")), 1u32.into());

        assert_eq!(sim.get(sim.signal("alias_part")), 0xf3u32.into());
        assert_eq!(sim.get(sim.signal("sep_part_a")), 0u32.into());
        assert_eq!(sim.get(sim.signal("sep_part_b")), 0x1234_56ffu32.into());

        sim.modify(|io| io.set(sel, 0u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("alias_br")), 4u32.into());
        assert_eq!(sim.get(sim.signal("sep_br_a")), 3u32.into());
        assert_eq!(sim.get(sim.signal("sep_br_b")), 4u32.into());
    }

    fn ref_ports_alias_package_and_hierarchical_variables_in_same_instance(sim) {
        @setup {
            let source = r#"
                package p;
                    int v_blk = 3;
                    int v_mix1 = 3;
                    int v_mix2 = 3;
                endpackage
                module ChildPkgBlk(input var logic clk, ref int a);
                    always_ff @(posedge clk) begin
                        a = 10;
                        a = p::v_blk + 1;
                    end
                endmodule
                module ChildPkgMix1(input var logic clk, ref int a);
                    always_ff @(posedge clk) begin
                        a = 0;
                        p::v_mix1 <= 1;
                        a <= 2;
                    end
                endmodule
                module ChildPkgMix2(input var logic clk, ref int a);
                    always_ff @(posedge clk) begin
                        a = 0;
                        a <= 1;
                        p::v_mix2 <= 2;
                    end
                endmodule
                module ChildHier(input var logic clk, ref int a);
                    always_ff @(posedge clk) begin
                        a = 10;
                        a = Top.hv + 1;
                    end
                endmodule
                module Top(
                    input var logic clk,
                    output int blk_out,
                    output int mix1_out,
                    output int mix2_out,
                    output int hier_out
                );
                    int hv = 3;
                    ChildPkgBlk u_blk(.clk(clk), .a(p::v_blk));
                    ChildPkgMix1 u_m1(.clk(clk), .a(p::v_mix1));
                    ChildPkgMix2 u_m2(.clk(clk), .a(p::v_mix2));
                    ChildHier u_h(.clk(clk), .a(hv));
                    assign blk_out = p::v_blk;
                    assign mix1_out = p::v_mix1;
                    assign mix2_out = p::v_mix2;
                    assign hier_out = hv;
                endmodule
            "#;
        }
        @build celox::Simulator::from_sv_sources(vec![(source, Path::new("ref_pkg_hier_alias.sv"))], "Top");
        let clk = sim.event("clk");
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("blk_out")), 11u32.into());
        assert_eq!(sim.get(sim.signal("mix1_out")), 2u32.into());
        assert_eq!(sim.get(sim.signal("mix2_out")), 2u32.into());
        assert_eq!(sim.get(sim.signal("hier_out")), 11u32.into());
    }

    fn ref_ports_allow_always_star_writers_with_const_ref_observer(sim) {
        @setup {
            let source = r#"
                module Obs(const ref logic [7:0] r, output logic [7:0] y);
                    timeunit 1ns / 1ns;
                    assign y = r;
                endmodule
                module StarWriter(
                    input var logic sel,
                    input var logic [7:0] d,
                    ref logic [7:0] r
                );
                    timeunit 1ns / 1ns;
                    always @* if (sel) r = d;
                endmodule
                module Top(
                    input var logic sel,
                    input var logic [7:0] a,
                    input var logic [7:0] b,
                    output logic [7:0] parent_obs,
                    output logic [7:0] cross_obs
                );
                    timeunit 1ns / 1ns;
                    logic [7:0] v;
                    always @* if (sel) v = a;
                    always @* if (!sel) v = b;
                    Obs u_parent_obs(.r(v), .y(parent_obs));

                    logic [7:0] w;
                    always @* if (!sel) w = a;
                    StarWriter u_child_star(.sel(sel), .d(b), .r(w));
                    Obs u_cross_obs(.r(w), .y(cross_obs));
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_always_star.sv"))], "Top");
        let sel = sim.signal("sel");
        let a = sim.signal("a");
        let b = sim.signal("b");
        let parent_obs = sim.signal("parent_obs");
        let cross_obs = sim.signal("cross_obs");

        sim.modify(|io| {
            io.set(sel, 1u8);
            io.set(a, 0x11u8);
            io.set(b, 0x22u8);
        })
        .unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(parent_obs), 0x11u8.into());
        assert_eq!(sim.get(cross_obs), 0x22u8.into());

        sim.modify(|io| io.set(sel, 0u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(parent_obs), 0x22u8.into());
        assert_eq!(sim.get(cross_obs), 0x11u8.into());

        sim.modify(|io| {
            io.set(a, 0x33u8);
            io.set(b, 0x44u8);
        })
        .unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(parent_obs), 0x44u8.into());
        assert_eq!(sim.get(cross_obs), 0x33u8.into());
    }

    fn ref_ports_track_repeated_unstaged_ff_and_initial_pulses_across_aliases(sim) {
        @setup {
            let source = r#"
                module ChildNbaPulse(input var logic clk, ref logic a, ref logic b);
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a <= 1'b1;
                        b <= 1'b0;
                    end
                endmodule
                module ChildBlkPulse(input var logic clk, ref logic a, ref logic b);
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a = 1'b1;
                        b = 1'b0;
                    end
                endmodule
                module ChildInitPulse(ref logic a, ref logic b);
                    timeunit 1ns / 1ns;
                    initial begin
                        #5;
                        a = 1'b1;
                        b = 1'b0;
                    end
                endmodule
                module Top(
                    input var logic clk,
                    output logic nba_val,
                    output int nba_hits,
                    output logic sep_a,
                    output logic sep_b,
                    output logic blk_val,
                    output int blk_hits,
                    output logic init_val,
                    output int init_hits
                );
                    timeunit 1ns / 1ns;
                    logic v_nba = 1'b0;
                    logic s_a = 1'b0;
                    logic s_b = 1'b1;
                    logic v_blk = 1'b0;
                    logic v_init = 1'b0;

                    ChildNbaPulse u_nba_alias(.clk(clk), .a(v_nba), .b(v_nba));
                    ChildNbaPulse u_nba_sep(.clk(clk), .a(s_a), .b(s_b));
                    ChildBlkPulse u_blk_alias(.clk(clk), .a(v_blk), .b(v_blk));
                    ChildInitPulse u_init_alias(.a(v_init), .b(v_init));

                    always @(posedge v_nba) nba_hits <= nba_hits + 1;
                    always @(posedge v_blk) blk_hits <= blk_hits + 1;
                    always @(posedge v_init) init_hits <= init_hits + 1;

                    initial begin
                        nba_hits = 0;
                        blk_hits = 0;
                        init_hits = 0;
                    end

                    assign nba_val = v_nba;
                    assign sep_a = s_a;
                    assign sep_b = s_b;
                    assign blk_val = v_blk;
                    assign init_val = v_init;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_unstaged_pulse.sv"))], "Top");
        let clk = sim.signal("clk");
        let nba_val = sim.signal("nba_val");
        let nba_hits = sim.signal("nba_hits");
        let sep_a = sim.signal("sep_a");
        let sep_b = sim.signal("sep_b");
        let blk_val = sim.signal("blk_val");
        let blk_hits = sim.signal("blk_hits");
        let init_val = sim.signal("init_val");
        let init_hits = sim.signal("init_hits");

        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(nba_hits), 0u32.into());
        assert_eq!(sim.get(blk_hits), 0u32.into());
        assert_eq!(sim.get(init_hits), 0u32.into());

        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(nba_val), 0u8.into());
        assert_eq!(sim.get(nba_hits), 1u32.into());
        assert_eq!(sim.get(sep_a), 1u8.into());
        assert_eq!(sim.get(sep_b), 0u8.into());
        assert_eq!(sim.get(blk_val), 0u8.into());
        assert_eq!(sim.get(blk_hits), 1u32.into());

        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(4).unwrap();
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(6).unwrap();
        assert_eq!(sim.get(nba_val), 0u8.into());
        assert_eq!(sim.get(nba_hits), 2u32.into());
        assert_eq!(sim.get(blk_val), 0u8.into());
        assert_eq!(sim.get(blk_hits), 2u32.into());
        assert_eq!(sim.get(init_val), 0u8.into());
        assert_eq!(sim.get(init_hits), 1u32.into());
    }

    fn ref_ports_track_repeated_unstaged_ff_pulses_on_derived_clock_without_wait(sim) {
        @setup {
            let source = r#"
                module ChildNbaPulse(input var logic clk, ref logic a, ref logic b);
                    always_ff @(posedge clk) begin
                        a <= 1'b1;
                        b <= 1'b0;
                    end
                endmodule
                module Top(
                    input var logic clk,
                    output logic v_out,
                    output int hits
                );
                    logic v = 1'b0;
                    ChildNbaPulse u_alias(.clk(clk), .a(v), .b(v));
                    always_ff @(posedge v) hits <= hits + 1;
                    initial hits = 0;
                    assign v_out = v;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_derived_clk_pulse.sv"))], "Top");
        let clk = sim.signal("clk");
        let v_out = sim.signal("v_out");
        let hits = sim.signal("hits");

        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(v_out), 0u8.into());
        assert_eq!(sim.get(hits), 0u32.into());

        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(v_out), 0u8.into());
        assert_eq!(sim.get(hits), 1u32.into());

        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(2).unwrap();
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(v_out), 0u8.into());
        assert_eq!(sim.get(hits), 2u32.into());
    }

    fn ref_ports_exclude_written_aliases_from_always_comb_and_always_star(sim) {
        @setup {
            let source = r#"
                package p_comb;
                    timeunit 1ns / 1ns;
                    int v_pkg;
                endpackage
                module ChildComb(ref int a, const ref int b, input int x);
                    timeunit 1ns / 1ns;
                    always_comb a = b + x;
                endmodule
                module ChildGrouped(
                    ref int a,
                    const ref int b,
                    input int x1,
                    input int x2,
                    input int x3
                );
                    timeunit 1ns / 1ns;
                    always_comb a = b + x1 + x2 + x3;
                endmodule
                module ChildPartialLit(
                    ref logic [7:0] a,
                    const ref logic [7:0] b,
                    input logic [3:0] x
                );
                    timeunit 1ns / 1ns;
                    always_comb begin
                        if (^b[3:0] === 1'bx) a[3:0] = x;
                        else a[3:0] = b[7:4] + b[3:0] + x;
                    end
                endmodule
                module ChildPartialWhole(
                    ref logic [7:0] a,
                    const ref logic [7:0] b,
                    input logic [3:0] x
                );
                    timeunit 1ns / 1ns;
                    always_comb begin
                        if (^b === 1'bx) a[3:0] = x;
                        else a[3:0] = b + {4'd0, x} + (b >> 4);
                    end
                endmodule
                module ChildPkgComb(ref int a, input int x);
                    timeunit 1ns / 1ns;
                    always_comb a = p_comb::v_pkg + x;
                endmodule
                module ChildStarComb(
                    ref int a,
                    const ref int b,
                    input var logic ext_en,
                    input int x
                );
                    timeunit 1ns / 1ns;
                    always @* if (!ext_en) a = b + x;
                endmodule
                module Top(
                    input int x,
                    input int ext_v2,
                    input int dummy,
                    input logic [3:0] hi,
                    input var logic star_ext,
                    input int star_val,
                    output int alias_out,
                    output int sep_a_out,
                    output int sep_b_out,
                    output int grp_alias_out,
                    output int grp_sep_out,
                    output logic [7:0] part_lit_out,
                    output logic [7:0] part_whole_out,
                    output int pkg_out,
                    output int star_out,
                    output int dummy_out
                );
                    timeunit 1ns / 1ns;
                    int v;
                    int v1;
                    int v2;
                    assign v2 = ext_v2;
                    ChildComb u_alias(.a(v), .b(v), .x(x));
                    ChildComb u_sep(.a(v1), .b(v2), .x(x));

                    int vg;
                    int vg1;
                    ChildGrouped u_grp_alias(.a(vg), .b(vg), .x1(x), .x2(1), .x3(2));
                    ChildGrouped u_grp_sep(.a(vg1), .b(v2), .x1(x), .x2(1), .x3(2));

                    logic [7:0] vp_lit;
                    logic [7:0] vp_whole;
                    assign vp_lit[7:4] = hi;
                    assign vp_whole[7:4] = hi;
                    ChildPartialLit u_plit(.a(vp_lit), .b(vp_lit), .x(x[3:0]));
                    ChildPartialWhole u_pwhole(.a(vp_whole), .b(vp_whole), .x(x[3:0]));

                    ChildPkgComb u_pkg(.a(p_comb::v_pkg), .x(x));

                    int v_star;
                    ChildStarComb u_star(.a(v_star), .b(v_star), .ext_en(star_ext), .x(x));
                    always @* if (star_ext) v_star = star_val;

                    assign alias_out = v;
                    assign sep_a_out = v1;
                    assign sep_b_out = v2;
                    assign grp_alias_out = vg;
                    assign grp_sep_out = vg1;
                    assign part_lit_out = vp_lit;
                    assign part_whole_out = vp_whole;
                    assign pkg_out = p_comb::v_pkg;
                    assign star_out = v_star;
                    assign dummy_out = dummy + 1;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_comb_alias_sens.sv"))], "Top");
        let x = sim.signal("x");
        let ext_v2 = sim.signal("ext_v2");
        let dummy = sim.signal("dummy");
        let hi = sim.signal("hi");
        let star_ext = sim.signal("star_ext");
        let star_val = sim.signal("star_val");

        let alias_out = sim.signal("alias_out");
        let sep_a_out = sim.signal("sep_a_out");
        let sep_b_out = sim.signal("sep_b_out");
        let grp_alias_out = sim.signal("grp_alias_out");
        let grp_sep_out = sim.signal("grp_sep_out");
        let part_lit_out = sim.signal("part_lit_out");
        let part_whole_out = sim.signal("part_whole_out");
        let pkg_out = sim.signal("pkg_out");
        let star_out = sim.signal("star_out");
        let dummy_out = sim.signal("dummy_out");

        sim.modify(|io| {
            io.set(x, 3u32);
            io.set(ext_v2, 10u32);
            io.set(dummy, 0u32);
            io.set(hi, 2u8);
            io.set(star_ext, 1u8);
            io.set(star_val, 10u32);
        })
        .unwrap();
        sim.run_until(0).unwrap();
        // Time 0 runs once: v (0) + 3 = 3, v1 = 10 + 3 = 13, vg = 0 + 3 + 3 = 6, vg1 = 10 + 3 + 3 = 16.
        assert_eq!(sim.get(alias_out), 3u32.into());
        assert_eq!(sim.get(sep_a_out), 13u32.into());
        assert_eq!(sim.get(sep_b_out), 10u32.into());
        assert_eq!(sim.get(grp_alias_out), 6u32.into());
        assert_eq!(sim.get(grp_sep_out), 16u32.into());
        assert_eq!(sim.get(pkg_out), 3u32.into());
        assert_eq!(sim.get(star_out), 10u32.into());
        assert_eq!(sim.get(dummy_out), 1u32.into());
        let init_plit: u8 = sim.get(part_lit_out).try_into().unwrap();
        let init_pwhole: u8 = sim.get(part_whole_out).try_into().unwrap();
        assert_eq!(init_plit >> 4, 2);
        assert_eq!(init_pwhole >> 4, 2);

        // Changing an unrelated input must NOT spuriously retrigger aliased always_comb blocks.
        sim.modify(|io| io.set(dummy, 7u32)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(dummy_out), 8u32.into());
        assert_eq!(sim.get(alias_out), 3u32.into());
        assert_eq!(sim.get(grp_alias_out), 6u32.into());
        assert_eq!(sim.get(pkg_out), 3u32.into());
        assert_eq!(sim.get(star_out), 10u32.into());
        assert_eq!(sim.get(part_lit_out), init_plit.into());
        assert_eq!(sim.get(part_whole_out), init_pwhole.into());

        // Changing ext_v2 (v2) triggers unaliased sep instances, not aliased instances;
        // deasserting star_ext triggers ChildStarComb once (10 + 3 = 13).
        sim.modify(|io| {
            io.set(ext_v2, 20u32);
            io.set(star_ext, 0u8);
        })
        .unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sep_a_out), 23u32.into());
        assert_eq!(sim.get(grp_sep_out), 26u32.into());
        assert_eq!(sim.get(alias_out), 3u32.into());
        assert_eq!(sim.get(grp_alias_out), 6u32.into());
        assert_eq!(sim.get(star_out), 13u32.into());

        // Changing x retriggers all aliased and unaliased combinational blocks once.
        sim.modify(|io| io.set(x, 4u32)).unwrap();
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(alias_out), 7u32.into()); // 3 + 4
        assert_eq!(sim.get(sep_a_out), 24u32.into()); // 20 + 4
        assert_eq!(sim.get(grp_alias_out), 13u32.into()); // 6 + 4 + 1 + 2
        assert_eq!(sim.get(grp_sep_out), 27u32.into()); // 20 + 4 + 1 + 2
        assert_eq!(sim.get(pkg_out), 7u32.into()); // 3 + 4
        assert_eq!(sim.get(star_out), 17u32.into()); // 13 + 4

        // Changing the unwritten upper slice hi (`b[7:4]`) must retrigger ChildPartialLit and ChildPartialWhole!
        let before_plit_lo = u8::try_from(sim.get(part_lit_out)).unwrap() & 0x0f;
        let before_pwhole_lo = u8::try_from(sim.get(part_whole_out)).unwrap() & 0x0f;
        sim.modify(|io| io.set(hi, 5u8)).unwrap();
        sim.run_until(4).unwrap();
        assert_eq!(
            sim.get(part_lit_out),
            ((5u8 << 4) | ((5 + before_plit_lo + 4) & 0x0f)).into()
        );
        assert_eq!(
            sim.get(part_whole_out),
            ((5u8 << 4) | ((before_pwhole_lo + 4 + 5) & 0x0f)).into()
        );

        // External write to v_star still updates always @* properly without loop.
        sim.modify(|io| {
            io.set(star_ext, 1u8);
            io.set(star_val, 100u32);
        })
        .unwrap();
        sim.run_until(5).unwrap();
        assert_eq!(sim.get(star_out), 100u32.into());
        sim.modify(|io| io.set(star_ext, 0u8)).unwrap();
        sim.run_until(6).unwrap();
        assert_eq!(sim.get(star_out), 104u32.into());
    }

    fn ref_ports_stage_nonblocking_writes_to_aliased_unpacked_arrays(sim) {
        @setup {
            let source = r#"
                module ChildArrMix(
                    input var logic clk,
                    ref int a[1],
                    ref int b[1],
                    output int y
                );
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a[0] = 10;
                        b[0] <= 20;
                        y <= a[0];
                    end
                endmodule
                module ChildArrOrder(
                    input var logic clk,
                    ref int a[2],
                    ref int b[2],
                    output int y
                );
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a[0] = 10;
                        b[0] <= 20;
                        a[0] <= 30;
                        y <= a[0];
                    end
                endmodule
                module ChildArrSubElem(
                    input var logic clk,
                    ref logic [31:0] a[1],
                    ref logic [31:0] b[1],
                    output logic [31:0] y
                );
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a[0] = 32'h1122_3344;
                        b[0][7:0] <= 8'haa;
                        a[0][15:8] <= 8'hbb;
                        y <= a[0];
                    end
                endmodule
                module ChildArrLoop(
                    input var logic clk,
                    ref int a[2],
                    ref int b[2],
                    output int y
                );
                    timeunit 1ns / 1ns;
                    always_ff @(posedge clk) begin
                        a[0] = 10;
                        for (int i = 0; i < 2; i++) begin
                            b[i] <= 20 + i;
                        end
                        y <= a[0];
                    end
                endmodule
                module Top(
                    input var logic clk,
                    output int y_alias,
                    output int arr_alias_0,
                    output int y_sep,
                    output int arr_sep_a_0,
                    output int arr_sep_b_0,
                    output int y_ord_alias,
                    output int arr_ord_alias_0,
                    output int y_ord_sep,
                    output int arr_ord_sep_a_0,
                    output int arr_ord_sep_b_0,
                    output logic [31:0] y_sub,
                    output logic [31:0] arr_sub_0,
                    output int y_loop,
                    output int arr_loop_0,
                    output int arr_loop_1
                );
                    timeunit 1ns / 1ns;
                    int arr_alias[1];
                    int arr_sep_a[1], arr_sep_b[1];
                    ChildArrMix u_mix_alias(.clk(clk), .a(arr_alias), .b(arr_alias), .y(y_alias));
                    ChildArrMix u_mix_sep(.clk(clk), .a(arr_sep_a), .b(arr_sep_b), .y(y_sep));

                    int arr_ord_alias[2];
                    int arr_ord_sep_a[2], arr_ord_sep_b[2];
                    ChildArrOrder u_ord_alias(.clk(clk), .a(arr_ord_alias), .b(arr_ord_alias), .y(y_ord_alias));
                    ChildArrOrder u_ord_sep(.clk(clk), .a(arr_ord_sep_a), .b(arr_ord_sep_b), .y(y_ord_sep));

                    logic [31:0] arr_sub[1];
                    ChildArrSubElem u_sub(.clk(clk), .a(arr_sub), .b(arr_sub), .y(y_sub));

                    int arr_loop[2];
                    ChildArrLoop u_loop(.clk(clk), .a(arr_loop), .b(arr_loop), .y(y_loop));

                    assign arr_alias_0 = arr_alias[0];
                    assign arr_sep_a_0 = arr_sep_a[0];
                    assign arr_sep_b_0 = arr_sep_b[0];
                    assign arr_ord_alias_0 = arr_ord_alias[0];
                    assign arr_ord_sep_a_0 = arr_ord_sep_a[0];
                    assign arr_ord_sep_b_0 = arr_ord_sep_b[0];
                    assign arr_sub_0 = arr_sub[0];
                    assign arr_loop_0 = arr_loop[0];
                    assign arr_loop_1 = arr_loop[1];
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_arr_nba_alias.sv"))], "Top");
        let clk = sim.signal("clk");
        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(0).unwrap();
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(1).unwrap();

        assert_eq!(sim.get(sim.signal("y_alias")), 10u32.into());
        assert_eq!(sim.get(sim.signal("arr_alias_0")), 20u32.into());
        assert_eq!(sim.get(sim.signal("y_sep")), 10u32.into());
        assert_eq!(sim.get(sim.signal("arr_sep_a_0")), 10u32.into());
        assert_eq!(sim.get(sim.signal("arr_sep_b_0")), 20u32.into());

        assert_eq!(sim.get(sim.signal("y_ord_alias")), 10u32.into());
        assert_eq!(sim.get(sim.signal("arr_ord_alias_0")), 30u32.into());
        assert_eq!(sim.get(sim.signal("y_ord_sep")), 10u32.into());
        assert_eq!(sim.get(sim.signal("arr_ord_sep_a_0")), 30u32.into());
        assert_eq!(sim.get(sim.signal("arr_ord_sep_b_0")), 20u32.into());

        assert_eq!(sim.get(sim.signal("y_sub")), 0x1122_3344u32.into());
        assert_eq!(sim.get(sim.signal("arr_sub_0")), 0x1122_bbaau32.into());

        assert_eq!(sim.get(sim.signal("y_loop")), 10u32.into());
        assert_eq!(sim.get(sim.signal("arr_loop_0")), 20u32.into());
        assert_eq!(sim.get(sim.signal("arr_loop_1")), 21u32.into());
    }

    fn ref_ports_correlate_foreign_overlap_and_driver_kind_per_instance(sim) {
        @setup {
            let source = r#"
                module ChildInit(ref logic [7:0] a);
                    timeunit 1ns / 1ns;
                    initial begin
                        #1;
                        a[7:4] = 4'ha;
                    end
                endmodule
                module Obs(const ref logic [7:0] r, output logic [7:0] y);
                    timeunit 1ns / 1ns;
                    assign y = r;
                endmodule
                module ChildStarSlice(ref logic [7:0] a, input logic [3:0] d);
                    timeunit 1ns / 1ns;
                    always @* a[7:4] = d;
                endmodule
                module ChildMixedLocal(ref logic [7:0] a, input logic [3:0] d);
                    timeunit 1ns / 1ns;
                    always_comb a[7:4] = d;
                    initial begin
                        #1;
                        a[3:0] = 4'hc;
                    end
                endmodule
                module Top(
                    input var logic sel,
                    input logic [3:0] d,
                    output logic [7:0] v1_out,
                    output logic [7:0] v2_out,
                    output logic [7:0] v3_out,
                    output logic [7:0] w1_out,
                    output logic [7:0] w2_out,
                    output logic [7:0] s1_out,
                    output logic [7:0] s2_out,
                    output logic [7:0] m_out
                );
                    timeunit 1ns / 1ns;
                    logic [7:0] v1;
                    logic [7:0] v2;
                    logic [7:0] v3;
                    assign v1[3:0] = 4'h5;
                    initial v2[7:4] = 4'hb;
                    assign v3[3:0] = 4'h5;
                    initial v3[7:4] = 4'hb;
                    ChildInit u1(.a(v1));
                    ChildInit u2(.a(v2));
                    ChildInit u3(.a(v3));

                    logic [7:0] w1;
                    logic [7:0] w2;
                    assign w1 = {d, 4'h2};
                    always @* if (sel) w2 = 8'hca;
                    always @* if (!sel) w2 = {d, 4'h1};
                    Obs u_obs1(.r(w1), .y(w1_out));
                    Obs u_obs2(.r(w2), .y(w2_out));

                    logic [7:0] s1;
                    logic [7:0] s2;
                    assign s1[3:0] = 4'h5;
                    always @* if (sel) s2[7:4] = 4'he;
                    ChildStarSlice u_s1(.a(s1), .d(d));
                    ChildStarSlice u_s2(.a(s2), .d(d));

                    logic [7:0] m;
                    initial m[3:0] = 4'h3;
                    ChildMixedLocal u_m(.a(m), .d(d));

                    assign v1_out = v1;
                    assign v2_out = v2;
                    assign v3_out = v3;
                    assign s1_out = s1;
                    assign s2_out = s2;
                    assign m_out = m;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_correlated_overlap.sv"))], "Top");
        let sel = sim.signal("sel");
        let d = sim.signal("d");
        let v1_out = sim.signal("v1_out");
        let v2_out = sim.signal("v2_out");
        let v3_out = sim.signal("v3_out");
        let w1_out = sim.signal("w1_out");
        let w2_out = sim.signal("w2_out");
        let s1_out = sim.signal("s1_out");
        let s2_out = sim.signal("s2_out");
        let m_out = sim.signal("m_out");

        sim.modify(|io| {
            io.set(sel, 0u8);
            io.set(d, 9u8);
        })
        .unwrap();
        sim.run_until(0).unwrap();

        let v1_t0: u8 = sim.get(v1_out).try_into().unwrap();
        let v2_t0: u8 = sim.get(v2_out).try_into().unwrap();
        assert_eq!(v1_t0 & 0x0f, 0x5);
        assert_eq!(v2_t0 >> 4, 0xb);
        assert_eq!(sim.get(v3_out), 0xb5u8.into());
        assert_eq!(sim.get(w1_out), 0x92u8.into());
        assert_eq!(sim.get(w2_out), 0x91u8.into());
        assert_eq!(sim.get(s1_out), 0x95u8.into());
        assert_eq!(sim.get(m_out), 0x93u8.into());

        // t=1 fires ChildInit (#1) and ChildMixedLocal (#1); sel toggles s2[7:4] to 4'he.
        sim.modify(|io| io.set(sel, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(v1_out), 0xa5u8.into());
        let v2_t1: u8 = sim.get(v2_out).try_into().unwrap();
        assert_eq!(v2_t1 >> 4, 0xa);
        assert_eq!(sim.get(v3_out), 0xa5u8.into());
        assert_eq!(sim.get(w2_out), 0xcau8.into());
        let s2_t1: u8 = sim.get(s2_out).try_into().unwrap();
        assert_eq!(s2_t1 >> 4, 0xe);
        assert_eq!(sim.get(m_out), 0x9cu8.into());

        // t=2 changes d -> 3, updating ChildStarSlice (s1 & s2) and ChildMixedLocal.
        sim.modify(|io| io.set(d, 3u8)).unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(w1_out), 0x32u8.into());
        assert_eq!(sim.get(s1_out), 0x35u8.into());
        let s2_t2: u8 = sim.get(s2_out).try_into().unwrap();
        assert_eq!(s2_t2 >> 4, 0x3);
        assert_eq!(sim.get(m_out), 0x3cu8.into());
    }

    fn ref_ports_preserve_disjoint_ff_writes_across_processes(sim) {
        @setup {
            let source = r#"
                module ChildSlices(
                    const ref logic clk,
                    ref logic [7:0] a,
                    ref logic [7:0] b
                );
                    always_ff @(posedge clk) a[3:0] = 4'ha;
                    always_ff @(posedge clk) b[7:4] = 4'hb;
                endmodule

                module ChildMixed(
                    const ref logic clk,
                    ref logic [7:0] a,
                    ref logic [7:0] b,
                    output logic [7:0] y
                );
                    always_ff @(posedge clk) b[7:4] <= 4'hb;
                    always_ff @(posedge clk) begin
                        a[3:0] = 4'ha;
                        y <= b;
                    end
                endmodule

                module ChildCross(
                    const ref logic clk,
                    ref logic [7:0] a,
                    ref logic [7:0] b,
                    output logic [7:0] y
                );
                    always_ff @(posedge clk) begin
                        a[3:0] = 4'ha;
                        y <= b;
                    end
                endmodule

                module ChildArray(
                    const ref logic clk,
                    ref logic [7:0] a[2],
                    ref logic [7:0] b[2]
                );
                    always_ff @(posedge clk) a[0] = 8'h12;
                    always_ff @(posedge clk) b[1] = 8'h34;
                endmodule

                module ChildWatched(
                    const ref logic clk,
                    ref logic [7:0] a,
                    ref logic [7:0] b
                );
                    always_ff @(posedge clk) begin
                        a = 8'h01;
                        b = 8'h00;
                    end
                endmodule

                module Top(
                    input var logic clk,
                    output logic [7:0] x_out,
                    output logic [7:0] p_out,
                    output logic [7:0] q_out,
                    output logic [7:0] m_alias_out,
                    output logic [7:0] y_alias_out,
                    output logic [7:0] p2_out,
                    output logic [7:0] q2_out,
                    output logic [7:0] y_sep_out,
                    output logic [7:0] cross_out,
                    output logic [7:0] cross_y_out,
                    output logic [7:0] arr0_out,
                    output logic [7:0] arr1_out,
                    output logic [7:0] hits_out
                );
                    logic [7:0] x = 8'h00;
                    logic [7:0] p = 8'h00;
                    logic [7:0] q = 8'h00;
                    ChildSlices u_slice_alias(.clk(clk), .a(x), .b(x));
                    ChildSlices u_slice_sep(.clk(clk), .a(p), .b(q));

                    logic [7:0] m_alias = 8'h30;
                    logic [7:0] p2 = 8'h12;
                    logic [7:0] q2 = 8'h34;
                    ChildMixed u_mixed_alias(.clk(clk), .a(m_alias), .b(m_alias), .y(y_alias_out));
                    ChildMixed u_mixed_sep(.clk(clk), .a(p2), .b(q2), .y(y_sep_out));

                    logic [7:0] x_cross = 8'h50;
                    always_ff @(posedge clk) x_cross[7:4] <= 4'hb;
                    ChildCross u_cross(.clk(clk), .a(x_cross), .b(x_cross), .y(cross_y_out));

                    logic [7:0] arr[2] = '{8'h00, 8'h00};
                    ChildArray u_arr(.clk(clk), .a(arr), .b(arr));

                    logic [7:0] w = 8'h00;
                    logic [7:0] hits = 8'h00;
                    ChildWatched u_watch(.clk(clk), .a(w), .b(w));
                    always @(posedge w) hits = hits + 8'h01;

                    assign x_out = x;
                    assign p_out = p;
                    assign q_out = q;
                    assign m_alias_out = m_alias;
                    assign p2_out = p2;
                    assign q2_out = q2;
                    assign cross_out = x_cross;
                    assign arr0_out = arr[0];
                    assign arr1_out = arr[1];
                    assign hits_out = hits;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_disjoint_ff.sv"))], "Top");
        let clk = sim.signal("clk");
        let x_out = sim.signal("x_out");
        let p_out = sim.signal("p_out");
        let q_out = sim.signal("q_out");
        let m_alias_out = sim.signal("m_alias_out");
        let y_alias_out = sim.signal("y_alias_out");
        let p2_out = sim.signal("p2_out");
        let q2_out = sim.signal("q2_out");
        let y_sep_out = sim.signal("y_sep_out");
        let cross_out = sim.signal("cross_out");
        let cross_y_out = sim.signal("cross_y_out");
        let arr0_out = sim.signal("arr0_out");
        let arr1_out = sim.signal("arr1_out");
        let hits_out = sim.signal("hits_out");

        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(x_out), 0x00u8.into());
        assert_eq!(sim.get(hits_out), 0x00u8.into());

        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(x_out), 0xbau8.into());
        assert_eq!(sim.get(p_out), 0x0au8.into());
        assert_eq!(sim.get(q_out), 0xb0u8.into());
        assert_eq!(sim.get(m_alias_out), 0xbau8.into());
        assert_eq!(sim.get(y_alias_out), 0x3au8.into());
        assert_eq!(sim.get(p2_out), 0x1au8.into());
        assert_eq!(sim.get(q2_out), 0xb4u8.into());
        assert_eq!(sim.get(y_sep_out), 0x34u8.into());
        assert_eq!(sim.get(cross_out), 0xbau8.into());
        assert_eq!(sim.get(cross_y_out), 0x5au8.into());
        assert_eq!(sim.get(arr0_out), 0x12u8.into());
        assert_eq!(sim.get(arr1_out), 0x34u8.into());
        assert_eq!(sim.get(hits_out), 0x01u8.into());
    }

    fn ref_ports_stage_task_nba_writes_before_exposing_alias_working_state(sim) {
        @setup {
            let source = r#"
                module ChildDirectTask(
                    const ref logic clk,
                    ref int a,
                    ref int b,
                    output int y
                );
                    task set_b;
                        b <= 20;
                    endtask
                    always_ff @(posedge clk) begin
                        a = 10;
                        set_b();
                        y <= a;
                    end
                endmodule

                module ChildRefTask(
                    const ref logic clk,
                    ref int a,
                    ref int b,
                    output int y
                );
                    task automatic set_ref(ref int r);
                        r = 10;
                    endtask
                    task set_b;
                        b <= 20;
                    endtask
                    always_ff @(posedge clk) begin
                        set_ref(a);
                        set_b();
                        y <= b;
                    end
                endmodule

                module ChildArrayTask(
                    const ref logic clk,
                    ref logic [7:0] a[2],
                    ref logic [7:0] b[2],
                    output logic [7:0] y
                );
                    task set_arr;
                        b[0] <= 8'h20;
                    endtask
                    always_ff @(posedge clk) begin
                        a[0] = 8'h10;
                        set_arr();
                        y <= a[0];
                    end
                endmodule

                module Top(
                    input var logic clk,
                    output int x_out,
                    output int y0_out,
                    output int p_out,
                    output int q_out,
                    output int y1_out,
                    output int x_ref_out,
                    output int y_ref_out,
                    output logic [7:0] arr0_out,
                    output logic [7:0] arr_y_out
                );
                    int x = 0;
                    int p = 0;
                    int q = 0;
                    ChildDirectTask u0(.clk(clk), .a(x), .b(x), .y(y0_out));
                    ChildDirectTask u1(.clk(clk), .a(p), .b(q), .y(y1_out));

                    int x_ref = 0;
                    ChildRefTask u_ref(.clk(clk), .a(x_ref), .b(x_ref), .y(y_ref_out));

                    logic [7:0] arr[2] = '{8'h00, 8'h00};
                    ChildArrayTask u_arr(.clk(clk), .a(arr), .b(arr), .y(arr_y_out));

                    assign x_out = x;
                    assign p_out = p;
                    assign q_out = q;
                    assign x_ref_out = x_ref;
                    assign arr0_out = arr[0];
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_task_nba.sv"))], "Top");
        let clk = sim.signal("clk");
        let x_out = sim.signal("x_out");
        let y0_out = sim.signal("y0_out");
        let p_out = sim.signal("p_out");
        let q_out = sim.signal("q_out");
        let y1_out = sim.signal("y1_out");
        let x_ref_out = sim.signal("x_ref_out");
        let y_ref_out = sim.signal("y_ref_out");
        let arr0_out = sim.signal("arr0_out");
        let arr_y_out = sim.signal("arr_y_out");

        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(0).unwrap();

        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(y0_out), 10u32.into());
        assert_eq!(sim.get(x_out), 20u32.into());
        assert_eq!(sim.get(y1_out), 10u32.into());
        assert_eq!(sim.get(p_out), 10u32.into());
        assert_eq!(sim.get(q_out), 20u32.into());
        assert_eq!(sim.get(y_ref_out), 10u32.into());
        assert_eq!(sim.get(x_ref_out), 20u32.into());
        assert_eq!(sim.get(arr_y_out), 0x10u8.into());
        assert_eq!(sim.get(arr0_out), 0x20u8.into());
    }

    fn ref_ports_use_canonical_aliases_in_always_comb_latch_coverage(sim) {
        @setup {
            let source = r#"
                module ChildWhole(ref int a, ref int b, input logic sel);
                    always_comb begin
                        a = 10;
                        if (sel) b = 20;
                    end
                endmodule

                module ChildSlices(
                    ref logic [7:0] a,
                    ref logic [7:0] b,
                    input logic sel,
                    output logic [7:0] y
                );
                    always_comb begin
                        a[3:0] = 4'ha;
                        b[7:4] = 4'hb;
                        y = b;
                        if (sel) b[3:0] = 4'hc;
                    end
                endmodule

                module Top(
                    input logic sel,
                    output int x_out,
                    output logic [7:0] y_out,
                    output logic [7:0] z_out
                );
                    int x;
                    logic [7:0] z;
                    ChildWhole u_whole(.a(x), .b(x), .sel(sel));
                    ChildSlices u_slices(.a(z), .b(z), .sel(sel), .y(y_out));
                    assign x_out = x;
                    assign z_out = z;
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("ref_comb_alias_latch.sv"))], "Top")
            .strict(true);
        assert!(sim.warnings().is_empty(), "{:?}", sim.warnings());
        let sel = sim.signal("sel");
        let x_out = sim.signal("x_out");
        let y_out = sim.signal("y_out");
        let z_out = sim.signal("z_out");

        sim.modify(|io| io.set(sel, 0u8)).unwrap();
        assert_eq!(sim.get(x_out), 10u32.into());
        assert_eq!(sim.get(y_out), 0xbau8.into());
        assert_eq!(sim.get(z_out), 0xbau8.into());

        sim.modify(|io| io.set(sel, 1u8)).unwrap();
        assert_eq!(sim.get(x_out), 20u32.into());
        assert_eq!(sim.get(y_out), 0xbau8.into());
        assert_eq!(sim.get(z_out), 0xbcu8.into());
    }
}

#[test]
fn ref_ports_keep_unaliased_instance_latch_diagnostics() {
    let source = r#"
        module Child(ref int a, ref int b, input logic sel);
            always_comb begin
                a = 10;
                if (sel) b = 20;
            end
        endmodule

        module Top(
            input logic sel,
            output int x_out,
            output int p_out,
            output int q_out
        );
            int x;
            int p;
            int q;
            Child u_alias(.a(x), .b(x), .sel(sel));
            Child u_sep(.a(p), .b(q), .sel(sel));
            assign x_out = x;
            assign p_out = p;
            assign q_out = q;
        endmodule
    "#;

    let err = match celox::Simulation::from_sv_sources(
        vec![(source, Path::new("ref_comb_mixed_latch.sv"))],
        "Top",
    )
    .strict(true)
    .build_cranelift()
    {
        Ok(_) => panic!("expected unaliased instance latch to fail under strict mode"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("latch inference inside always_comb"), "{err}");

    let mut sim = celox::Simulation::from_sv_sources(
        vec![(source, Path::new("ref_comb_mixed_latch.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.warnings().len(), 1, "{:?}", sim.warnings());
    let sel = sim.signal("sel");
    let x_out = sim.signal("x_out");
    let p_out = sim.signal("p_out");
    let q_out = sim.signal("q_out");

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(x_out), 20u32.into());
    assert_eq!(sim.get(p_out), 10u32.into());
    assert_eq!(sim.get(q_out), 20u32.into());

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(x_out), 10u32.into());
    assert_eq!(sim.get(p_out), 10u32.into());
    assert_eq!(sim.get(q_out), 20u32.into());
}

fn assert_sv_rejected(source: &str, expected_substring: &str) {
    let raw =
        match celox::Simulation::from_sv_sources(vec![(source, Path::new("bad_ref.sv"))], "Top")
            .build_cranelift()
        {
            Ok(_) => {
                panic!("expected compile failure containing `{expected_substring}` for:\n{source}")
            }
            Err(err) => err.to_string(),
        };
    let err = raw.replace("-\n  │ ", "-").replace("\n  │ ", " ");
    assert!(
        err.contains(expected_substring),
        "expected `{expected_substring}` in error:\n{err}\nsource:\n{source}"
    );
}

#[test]
fn ref_port_illegal_syntax_and_connection_cases_are_rejected() {
    let cases = [
        (
            "module Child(ref wire a); endmodule module Top; logic x; Child u(.a(x)); endmodule",
            "net type",
        ),
        (
            "module Child(a); ref wire a; endmodule module Top; logic x; Child u(.a(x)); endmodule",
            "net type",
        ),
        (
            "module Child(a); ref a; wire a; endmodule module Top; logic x; Child u(.a(x)); endmodule",
            "net type",
        ),
        (
            "module Child(const ref logic clk); endmodule module Top(input logic clk); Child u(.clk(clk)); endmodule",
            "cannot connect to ref port",
        ),
        (
            "module Child(ref int a = 0); endmodule module Top; int x; Child u(.a(x)); endmodule",
            "default value",
        ),
        (
            "module Top(const ref int x); endmodule",
            "top-level ref port",
        ),
        (
            "module Child(ref int x); endmodule module Top; Child u(); endmodule",
            "unconnected",
        ),
        (
            "module Child(ref int x); endmodule module Top; Child u(.x()); endmodule",
            "unconnected",
        ),
        (
            "module Child(ref struct { int a; } s); endmodule module Top; Child u(); endmodule",
            "unconnected",
        ),
        (
            "module Child(ref int x); endmodule module Top; int a; Child u(.x(a + 1)); endmodule",
            "requires a variable",
        ),
        (
            "module Child(ref int x); endmodule module Top; Child u(.x(32'd1)); endmodule",
            "requires a variable",
        ),
        (
            "module Child(ref int x); endmodule module Top; parameter int P = 1; Child u(.x(P)); endmodule",
            "requires a variable",
        ),
        (
            "module Child(ref logic [3:0] x); endmodule module Top; logic [7:0] v; Child u(.x(v[3:0])); endmodule",
            "requires a variable",
        ),
        (
            "module Child(ref logic [7:0] x); endmodule module Top; logic [3:0] a, b; Child u(.x({a, b})); endmodule",
            "requires a variable",
        ),
        (
            "module Child(ref logic x); endmodule module Top; wire w = 1'b0; Child u(.x(w)); endmodule",
            "cannot connect to ref port",
        ),
        (
            "module Child(ref int x); endmodule module Top; int arr[2]; Child u(.x(arr[0])); endmodule",
            "unpacked array or container element",
        ),
        (
            "module Child(ref int x); endmodule module Top; logic [31:0] v; Child u(.x(v)); endmodule",
            "equivalent",
        ),
        (
            "module Child(ref int x); endmodule module Top; int unsigned v; Child u(.x(v)); endmodule",
            "equivalent",
        ),
        (
            "package p; typedef enum {A,B} e1_t; typedef enum {C,D} e2_t; endpackage
             module Child(ref p::e1_t x); endmodule
             module Top; p::e2_t v; Child u(.x(v)); endmodule",
            "equivalent",
        ),
        (
            "package p; class A; endclass class B extends A; endclass endpackage
             module Child(ref p::A x); endmodule
             module Top; p::B v; Child u(.x(v)); endmodule",
            "equivalent",
        ),
        (
            "module Child(ref int q[$]); endmodule
             module Top; int q[$:2]; Child u(.q(q)); endmodule",
            "different bounds",
        ),
        (
            "module Child(ref int q[$][$:2]); endmodule
             module Top; int q[$][$:1]; Child u(.q(q)); endmodule",
            "different bounds",
        ),
        (
            "package p; typedef struct { int a; } in_t; typedef struct { in_t inner; int b; } out_t; endpackage
             module Child(ref p::out_t s); endmodule
             module Top; Child u(); endmodule",
            "unconnected",
        ),
        (
            "package p; typedef struct { int a; } s1_t; typedef struct { int b; } s2_t; endpackage
             module Child(ref p::s1_t s); endmodule
             module Top; p::s2_t v; Child u(.s(v)); endmodule",
            "equivalent",
        ),
        (
            "module Child(ref int x); endmodule
             module Top; const int c = 1; Child u(.x(c)); endmodule",
            "non-const ref port",
        ),
    ];
    for (src, msg) in cases {
        assert_sv_rejected(src, msg);
    }

    assert_sv_rejected(
        "module Leaf(ref int x); endmodule
         module Mid(const ref int x); Leaf u(.x(x)); endmodule
         module Top; int v; Mid m(.x(v)); endmodule",
        "non-const ref port",
    );
    assert_sv_rejected(
        "package p; typedef struct { int a; bit b; } pair_t; endpackage
         module Leaf(ref p::pair_t s); endmodule
         module Mid(const ref p::pair_t s); Leaf u(.s(s)); endmodule
         module Top; p::pair_t v; Mid m(.s(v)); endmodule",
        "non-const ref port",
    );
    assert_sv_rejected(
        "module Leaf(output int x); assign x = 1; endmodule
         module Mid(const ref int x); Leaf u(.x(x)); endmodule
         module Top; int v; Mid m(.x(v)); endmodule",
        "cannot connect to output port",
    );
    assert_sv_rejected(
        "module Child(const ref int x); initial x = 1; endmodule
         module Top; int v; Child u(.x(v)); endmodule",
        "const ref port",
    );
    assert_sv_rejected(
        "module Child(const ref int q[$]); initial q.push_back(1); endmodule
         module Top; int q[$]; Child u(.q(q)); endmodule",
        "const ref port",
    );
    assert_sv_rejected(
        "module Child(const ref int q[$][$]); initial q[0].push_back(1); endmodule
         module Top; int q[$][$]; Child u(.q(q)); endmodule",
        "const ref port",
    );
    assert_sv_rejected(
        "module Child(const ref int q[$][$]); initial q[0][0] = 2; endmodule
         module Top; int q[$][$]; Child u(.q(q)); endmodule",
        "const ref port",
    );
    assert_sv_rejected(
        "module Child(const ref int x); logic [3:0] a; function automatic int f(ref int y); y = 1; return 0; endfunction initial a[f(x)] = 1'b1; endmodule
         module Top; int v; Child u(.x(v)); endmodule",
        "const ref port",
    );
    assert_sv_rejected(
        "module Child(ref int x); endmodule
         module Top; int arr[2]; Child u[2](.x(arr)); endmodule",
        "element-by-element ref port",
    );
}

#[test]
fn ref_port_driver_conflicts_and_procedural_force_assign_are_rejected() {
    for src in [
        // parent always_ff + child always_ff
        "module Child(const ref logic clk, ref logic [7:0] x); always_ff @(posedge clk) x <= 8'd1; endmodule
         module Top(input var logic clk); logic [7:0] v; Child u(.clk(clk), .x(v)); always_ff @(posedge clk) v <= 8'd2; endmodule",
        // two children always_ff on same var
        "module Child(const ref logic clk, ref logic [7:0] x); always_ff @(posedge clk) x <= 8'd1; endmodule
         module Top(input var logic clk); logic [7:0] v; Child u1(.clk(clk), .x(v)); Child u2(.clk(clk), .x(v)); endmodule",
        // child always_comb + parent always_ff
        "module Child(ref int x); always_comb x = 1; endmodule
         module Top(input var logic clk); int v; Child u(.x(v)); always_ff @(posedge clk) v <= 2; endmodule",
        // two children continuous assign
        "module Child(ref int x); assign x = 1; endmodule
         module Top; int v; Child u1(.x(v)); Child u2(.x(v)); endmodule",
        // explicit initial + child always_ff
        "module Child(const ref logic clk, ref int x); always_ff @(posedge clk) x <= 1; endmodule
         module Top(input var logic clk); int v; initial v = 0; Child u(.clk(clk), .x(v)); endmodule",
        // same-instance aliased ref ports written by two always_comb
        "module Child(ref int a, ref int b); always_comb a = 1; always_comb b = 2; endmodule
         module Top; int v; Child u(.a(v), .b(v)); endmodule",
        // same-instance aliased ref ports written by two always_ff
        "module Child(const ref logic clk, ref int a, ref int b); always_ff @(posedge clk) a <= 1; always_ff @(posedge clk) b <= 2; endmodule
         module Top(input var logic clk); int v; Child u(.clk(clk), .a(v), .b(v)); endmodule",
        // same-instance aliased ref ports written by two continuous assigns
        "module Child(ref int a, ref int b); assign a = 1; assign b = 2; endmodule
         module Top; int v; Child u(.a(v), .b(v)); endmodule",
        // overlapping cross-module slice drivers
        "module Child(ref logic [7:0] r); assign r[4:2] = 3'd1; endmodule
         module Top; logic [7:0] v; assign v[3:0] = 4'h5; Child u(.r(v)); endmodule",
        // overlapping shifted-bound slice drivers
        "module Child(ref logic [7:0] r); assign r[3:0] = 4'ha; endmodule
         module Top; logic [15:8] v; assign v[11:8] = 4'h5; Child u(.r(v)); endmodule",
        // always @* + always_comb across ref ports
        "module Child(ref int x); always_comb x = 1; endmodule
         module Top(input int d); int v; Child u(.x(v)); always @* v = d; endmodule",
        "module Child(ref int x, input int d); always @* x = d; endmodule
         module Top; int v; Child u(.x(v), .d(0)); always_comb v = 1; endmodule",
        // always @* + always_ff across ref ports
        "module Child(const ref logic clk, ref int x); always_ff @(posedge clk) x <= 1; endmodule
         module Top(input var logic clk, input int d); int v; Child u(.clk(clk), .x(v)); always @* v = d; endmodule",
        "module Child(ref int x, input int d); always @* x = d; endmodule
         module Top(input var logic clk); int v; Child u(.x(v), .d(0)); always_ff @(posedge clk) v <= 1; endmodule",
    ] {
        assert_sv_rejected(src, "multiple variable drivers");
    }

    assert_sv_rejected(
        "module Child(ref logic [7:0] x); initial force x = 8'd1; endmodule
         module Top; logic [7:0] v; Child u(.x(v)); endmodule",
        "force or release of ref port",
    );
    assert_sv_rejected(
        "module Child(ref logic [7:0] x); initial assign x = 8'd1; endmodule
         module Top; logic [7:0] v; Child u(.x(v)); endmodule",
        "procedural continuous assignment to ref port",
    );
    assert_sv_rejected(
        "module Child(ref logic [7:0] x); endmodule
         module Top; logic [7:0] v; Child u(.x(v)); initial force v = 8'hff; endmodule",
        "aliased by a ref port",
    );
    assert_sv_rejected(
        "module Child(ref int a, const ref int b, input int x);
             assign a = b + x;
         endmodule
         module Top(input int x, output int y);
             int v;
             Child u(.a(v), .b(v), .x(x));
             assign y = v;
         endmodule",
        "Combinational loop detected",
    );
    assert_sv_rejected(
        "module Child(ref int a, const ref int b);
             int y;
             always_comb a = y + 1;
             always_comb y = b + 1;
         endmodule
         module Top(output int z);
             int v;
             Child u(.a(v), .b(v));
             assign z = v;
         endmodule",
        "Combinational loop detected",
    );
}

#[test]
fn mixed_veryl_and_sv_ref_port_is_rejected() {
    let veryl = "module Top(a: input logic<32>, y: output logic<32>) { var v: logic<32>; assign v = a; inst child: $sv::Child(r: v); assign y = v; }";
    let sv = "module Child(ref logic [31:0] r); always_comb ; endmodule";
    let err = match celox::Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("child.sv"))],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("expected mixed Veryl/SV ref port to fail"),
        Err(err) => err.to_string(),
    };
    assert!(
        err.contains("ref") && err.contains("mixed-language") || err.contains("ref port"),
        "{err}"
    );
}
