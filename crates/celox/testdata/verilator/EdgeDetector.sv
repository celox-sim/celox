
module edge_detector #(

    parameter int unsigned WIDTH = 1,

    parameter bit [WIDTH-1:0] INITIAL_VALUE = '0
) (

    input var logic i_clk,

    input var logic i_rst,

    input var logic i_clear,

    input var logic [WIDTH-1:0] i_data,

    output var logic [WIDTH-1:0] o_edge,

    output var logic [WIDTH-1:0] o_posedge,

    output var logic [WIDTH-1:0] o_negedge
);
    logic [WIDTH-1:0] data;

    always_comb o_edge    = (i_data) ^ (data) & (~i_clear);
    always_comb o_posedge = (i_data) & (~data) & (~i_clear);
    always_comb o_negedge = (~i_data) & (data) & (~i_clear);

    always_ff @ (posedge i_clk, negedge i_rst) begin
        if (!i_rst) begin
            data <= INITIAL_VALUE;
        end else if (i_clear) begin
            data <= INITIAL_VALUE;
        end else begin
            data <= i_data;
        end
    end
endmodule

module Top (
    input  var logic          clk      ,
    input  var logic          rst      ,
    input  var logic [32-1:0] i_data   ,
    output var logic [32-1:0] o_edge   ,
    output var logic [32-1:0] o_posedge,
    output var logic [32-1:0] o_negedge
);
    edge_detector #(
        .WIDTH     (32       )
    ) u (
        .i_clk     (clk      ),
        .i_rst     (rst      ),
        .i_clear   (1'b0     ),
        .i_data    (i_data   ),
        .o_edge    (o_edge   ),
        .o_posedge (o_posedge),
        .o_negedge (o_negedge)
    );
endmodule