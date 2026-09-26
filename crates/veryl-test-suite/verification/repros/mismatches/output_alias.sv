module Top;
    logic [7:0] positional, named_forward, named_reverse, separate_first, separate_second;
    logic result;
    logic [7:0] void_positional, void_named_reverse;
    function automatic logic outputs(output logic [7:0] first, output logic [7:0] second);
        first = 1; second = 2; return 1;
    endfunction
    function automatic void split(input logic [7:0] x, output logic [7:0] first, output logic [7:0] second);
        first = x + 1; second = x + 2;
    endfunction
    initial begin
        result = outputs(positional, positional);
        result = outputs(.first(named_forward), .second(named_forward));
        result = outputs(.second(named_reverse), .first(named_reverse));
        result = outputs(.second(separate_second), .first(separate_first));
        $display("review:positional=%d named_forward=%d named_reverse=%d separate_first=%d separate_second=%d return=%b",
                 positional, named_forward, named_reverse, separate_first, separate_second, result);
        split(0, void_positional, void_positional);
        split(.second(void_named_reverse), .x(0), .first(void_named_reverse));
        $display("review:void_positional=%d void_named_reverse=%d", void_positional, void_named_reverse);
        $finish;
    end
endmodule
