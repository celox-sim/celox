// Adapter-free probes for IEEE 1800-2023 9.4.5, 10.4.1 and 11.8.2.
`ifdef CONVERSION_PORTS
module Top(output logic [129:0] wide,
           output bit [7:0] clean, output logic signed [127:0] signed_value);
  logic [129:0] value={2'bxz,128'h1234};
  logic [7:0] dirty=8'b1x0z1001;
  logic signed [7:0] small_value=-1;
  initial wide = #3 value;
  initial clean = #3 dirty;
  initial signed_value = #3 small_value;
  initial begin #1 value='0; dirty='1; small_value=0; end
  initial begin #4 $display("clean=%h (expected 89)",clean); $finish; end
endmodule
`else
module Top;
`ifdef UNKNOWN_REPEAT
`ifdef NEGATIVE_REPEAT
  int count=-3;
`elsif HIGH_Z
  logic [7:0] count='z;
`elsif MIXED_UNKNOWN
  logic [7:0] count=8'hxz;
`else
  logic [7:0] count='x;
`endif
  bit clk, done=0;
  int result=0;
  initial begin result = repeat(count) @(posedge clk) 9; done=1; end
  initial begin
    #1 $display("result=%0d done=%0d (expected 9 1)",result,done);
    $finish;
  end
`elsif REPEAT_EVENT
  bit clk;
  int count=3,result=0;
  initial repeat(3) begin #1 clk=1; #1 clk=0; end
  initial result = repeat(count) @(posedge clk) 9;
  initial begin #7 $display("result=%0d (expected 9)",result); $finish; end
`elsif CONVERSION
  bit [7:0] a=8'hff,b=1,clean=0;
  bit [15:0] sum=0;
  logic [7:0] dirty=8'b1x0z1001;
  initial sum = #2 a+b;
  initial clean = #2 dirty;
  initial begin
    #1 a=0; b=0; dirty='1;
    #2 $display("sum=%h clean=%h (expected 0100 89)",sum,clean);
    $finish;
  end
`elsif REFERENCE_PORT
  int result;
  task automatic sample(input int value, ref int destination);
    destination = #1 value;
  endtask
  initial begin sample(9,result); $display("result=%0d (expected 9)",result); $finish; end
`else
  bit [15:0] selected=0;
  int index=0,data=41;
  initial selected[(index++) +: 8] = #8 data++;
  initial begin #2 index=8; end
  initial begin
    #1 $display("early index=%0d data=%0d (expected 0 42)",index,data);
    #8 $display("late index=%0d selected=%h (expected 9 2900)",index,selected);
    $finish;
  end
`endif
endmodule
`endif
