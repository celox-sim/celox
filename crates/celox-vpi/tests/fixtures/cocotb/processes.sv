`timescale 1ns/1ps
module Top(input logic tick, input logic go, output logic [7:0] count, output logic ack,
           output logic [7:0] twice, output logic [7:0] delayed);
    logic clk = 0;
    always #5 clk = ~clk;
    initial count = 0;
    always_ff @(posedge clk) count <= count + 1;
    assign twice = count * 2;
    initial #12 $display("at %0t count=%0d", $time, count);
    initial begin
        ack = 0;
        @(posedge go);
        #3 ack = 1;
    end
    initial delayed = 0;
    always @(posedge tick) #1 delayed = count;
endmodule
