// A signed value passed to a queue method of a wider element type.
// IEEE 1800-2023 7.10.2, 13.5.1: the item argument is copied as by
// assignment, so a signed int of -1 extends to all ones (10.7), as the
// element assignment and the variable assignment do. Expected output:
// 1 1 1 / 1 1. Verilator 5.052 prints 0 for the method arguments.
module Top;
    bit [199:0] u[$];
    bit [199:0] v;
    bit [47:0] n[$];
    int minus_one = -1;
    initial begin
        u.push_back(minus_one);
        v = minus_one;
        n.push_back(minus_one);
        $display("%0d %0d %0d", &u[0], &v, &n[0]);
        u.insert(0, minus_one);
        u[1] = minus_one;
        $display("%0d %0d", &u[0], &u[1]);
    end
endmodule
