// A conditional operator whose operands are unpacked arrays of an equivalent
// type yields the selected array (IEEE 1800-2023 11.4.11): `h = s ? c : d`
// with s = 1 copies c. The pinned xezim 0.11.3 prints h='{0, 0}; Verilator
// 5.052 and Celox print h='{4, 3}.
module Sub;
    int b[2] = '{7, 8};
endmodule
module Top;
    typedef enum {A, B} e_t;
    typedef int ia_t[2];
    Sub u();
    int a[4], c[2], d[2], h[2];
    e_t ea[2], eb[2];
    bit signed [31:0] w[2];
    bit s;
    function ia_t f();
        return d;
    endfunction
    function int g(int p[2] = d);
        return p[0] + p[1];
    endfunction
    initial begin
        d = '{1, 2};
        a = '{1, 2, 3, 4};
        a[0:1] = a[2:3];
        c = a[1+:2];
        $display("a=%p c=%p", a, c);
        w = c;
        s = 1;
        h = s ? c : d;
        $display("w=%p h=%p", w, h);
        eb = '{B, A};
        ea = eb;
        c = f();
        $display("c=%p g=%0d", c, g());
        h[1] = u.b[1];
        $display("h=%p ea0=%0d", h, ea[0]);
    end
endmodule
