module Child (
    input logic i_clk,
    input logic i_rst,
    input counter_types::data_t d,
    output counter_types::data_t q
);
    always_ff @(posedge i_clk or negedge i_rst) begin
        if (!i_rst) q <= '0;
        else q <= d;
    end
endmodule
