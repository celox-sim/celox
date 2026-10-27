package p;
    class Box #(int N = 5);
        typedef bit [N-1:0] element_t;
    endclass
    typedef Box#() Small;
    typedef Box#(9) Wide;
endpackage
module Top(output int sum, output bit done);
    import p::*;
    localparam int N = 29;
    Small::element_t sample;
    Wide::element_t other;
    initial begin
        sample = '1;
        other = '1;
        sum = sample + other;
        done = $bits(sample) == 5 && $bits(other) == 9;
        $display("small=%0d wide=%0d sum=%0d done=%0d",$bits(sample),$bits(other),sum,done);
    end
endmodule
module Wrapped; Top dut(); endmodule
