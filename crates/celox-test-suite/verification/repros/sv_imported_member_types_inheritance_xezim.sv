package p;
    class Base #(type T = byte);
        typedef T item_t;
    endclass
    class Derived #(int N = 9) extends Base#(bit [2:0]); endclass
    typedef Derived#() Tiny;
    typedef Base#(bit [6:0]) Other;
endpackage
package q;
    import p::Tiny;
    import p::Other;
    export p::Tiny;
    export p::Other;
    class Sub extends Tiny;
        typedef bit [4:0] item_t;
        typedef item_t copied_t;
    endclass
endpackage
module Top(output int sum, output bit done);
    import q::*;
    Sub::copied_t a;
    Other::T b;
    Tiny::item_t c;
    initial begin
        a = '1;
        b = '1;
        c = '1;
        sum = a + b + c;
        done = $bits(a) == 5 && $bits(b) == 7 && $bits(c) == 3;
        $display("a=%0d b=%0d c=%0d sum=%0d done=%0d",$bits(a),$bits(b),$bits(c),sum,done);
    end
endmodule
module Wrapped; Top dut(); endmodule
