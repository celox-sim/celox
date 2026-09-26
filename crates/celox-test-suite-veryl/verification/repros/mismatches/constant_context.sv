module ConstantContext (
    output var logic [32-1:0] bits_result    ,
    output var logic          argument_result,
    output var logic [8-1:0]  shift_result
);
    function automatic logic f(
        input var logic [4-1:0] x
    ) ;
        return x[2];
    endfunction
    localparam logic signed [8-1:0]  SIGNED_VALUE     = 8'hff;
    localparam logic        [32-1:0] BITS_CONTEXT     = ((1'b0) ? ( $bits(logic [5-1:0]) ) : ( SIGNED_VALUE ));
    localparam logic                 ARGUMENT_CONTEXT = f(2'b11 + 2'b01);
    localparam logic        [8-1:0]  SHIFT_CONTEXT    = 8'bxzxzxzxz >> 0;
    always_comb shift_result     = SHIFT_CONTEXT;
    always_comb bits_result      = BITS_CONTEXT;
    always_comb argument_result  = ARGUMENT_CONTEXT;
endmodule
//# sourceMappingURL=constant_context.sv.map

module Top;
    wire [31:0] bits_result;
    wire argument_result;
    wire [7:0] shift_result;
    ConstantContext dut(bits_result, argument_result, shift_result);
    initial begin
        #1;
        $display("review:constant_bits=%h constant_argument=%b constant_shift=%b", bits_result, argument_result, shift_result);
        $finish;
    end
endmodule
