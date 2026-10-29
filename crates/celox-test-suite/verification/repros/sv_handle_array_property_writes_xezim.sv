// Property writes through an element of an array of class handles.
// IEEE 1800-2023 8.4, 7.4, 11.5.1: hs[0] names the object a refers to, so
// each write changes a.p. xezim 0.11.3 drops the writes when Top is
// instantiated (top Wrapped); as the top module they work.
module Top;
    class C;
        logic [7:0] p;
    endclass
    C a;
    C hs[2];
    int i;
    initial begin
        a = new;
        hs[0] = a;
        a.p = 8'h34;
        hs[0].p = 8'h56;
        $display("whole %h", a.p);
        hs[0].p[7:4] = 4'h9;
        $display("constant select %h", a.p);
        hs[i].p[3:0] = 4'h1;
        $display("variable select %h", a.p);
    end
endmodule

module Wrapped;
    Top dut ();
endmodule
