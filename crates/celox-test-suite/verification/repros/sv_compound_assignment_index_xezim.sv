// IEEE 1800-2023 11.4.1: an assignment operator evaluates any left-hand index
// expression only once. move_index() changes the index while the value is
// evaluated, so both lines print 01. xezim 0.11.3 prints 03 for both, as Top
// and as Wrapped: it reads bit 0 and writes bit 1.
module Top;
    class C;
        logic [7:0] p;
    endclass
    C a;
    int i;
    logic [7:0] v;
    function automatic logic [1:0] move_index();
        i = 1;
        return 2'd0;
    endfunction
    initial begin
        a = new;
        a.p = 8'h01;
        i = 0;
        a.p[i] += move_index();
        $display("property %h", a.p);
        v = 8'h01;
        i = 0;
        v[i] += move_index();
        $display("variable %h", v);
    end
endmodule

module Wrapped;
    Top dut ();
endmodule
