`timescale 1ns/1ns
module Top(output int initialized, child, tail, other);
    function automatic int init_value(); initialized++; return 7; endfunction
    initial begin initialized=0; child=0; tail=0; other=0; #1 $finish; end
    final begin
        fork
            automatic int captured = init_value();
            child = captured;
        join_none
        tail = 1;
        $display("fork initialized=%0d child=%0d tail=%0d", initialized, child, tail);
    end
    final begin other = 1; $display("other=%0d", other); end
endmodule
