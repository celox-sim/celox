`timescale 1ns/1ns
module Top(output logic [7:0] count);
    logic clk = 0;
    always #5 clk = ~clk;
    initial count = 0;
    always_ff @(posedge clk) count <= count + 1;
    initial begin
        #30 $display("finishing at %0t", $time);
        $finish;
    end
    initial #100 $display("late");
endmodule
