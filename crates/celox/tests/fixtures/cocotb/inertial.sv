`timescale 1ns/1ps
module Top #(parameter WITH_INITIAL = 0) (
    input logic clk,
    input logic [7:0] d,
    output logic [7:0] q
);
    if (WITH_INITIAL) begin : initialized
        initial q = 0;
    end
    always_ff @(posedge clk) q <= d;
endmodule
