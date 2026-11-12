module Top (
    input logic i_clk,
    input logic i_rst,
    input counter_types::data_t d,
    output counter_types::data_t q
);
    Child child (.i_clk(i_clk), .i_rst(i_rst), .d(d), .q(q));
endmodule
