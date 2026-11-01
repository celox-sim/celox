`timescale 1ns/1ns
// Reset vectors select SP=0x800 and PC=0x100. The program executes
// MOVE.W #0x1234,0x1000, followed by BRA.S to itself.
// The first external word write must be the expected signature.
module Top #(parameter logic [15:0] ROM_VALUE = 16'h1234);
  logic clk = 0;
  always #1 clk = ~clk;
  logic phase = 0;
  always @(negedge clk) phase <= ~phase;
  logic extReset = 1, pwrUp = 1;
  wire rw_n, as_n, lds_n, uds_n;
  wire [23:1] address;
  wire [15:0] data_out;
  logic [15:0] data_in;
  fx68k dut(.clk(clk), .HALTn(1'b1), .extReset(extReset), .pwrUp(pwrUp),
    .enPhi1(phase), .enPhi2(~phase), .eRWn(rw_n), .ASn(as_n),
    .LDSn(lds_n), .UDSn(uds_n), .E(), .VMAn(), .FC0(), .FC1(), .FC2(),
    .BGn(), .oRESETn(), .oHALTEDn(), .DTACKn(1'b0), .VPAn(1'b1),
    .BERRn(1'b1), .BRn(1'b1), .BGACKn(1'b1),
    .IPL0n(1'b1), .IPL1n(1'b1), .IPL2n(1'b1),
    .iEdb(data_in), .oEdb(data_out), .eab(address));
  always_comb begin
    case (address)
      0: data_in = 16'h0000;
      1: data_in = 16'h0800;
      2: data_in = 16'h0000;
      3: data_in = 16'h0100;
      128: data_in = 16'h33fc;
      129: data_in = ROM_VALUE;
      130: data_in = 16'h0000;
      131: data_in = 16'h1000;
      132: data_in = 16'h60fe;
      default: data_in = 16'h4e71;
    endcase
  end
  always @(negedge clk) begin
    if (!as_n && !rw_n && (!uds_n || !lds_n)) begin
      $display("WRITE %h %h", {address, 1'b0}, data_out);
      if (uds_n !== 1'b0 || lds_n !== 1'b0 ||
          {address,1'b0} !== 24'h001000 || data_out !== 16'h1234)
        $fatal(1, "unexpected CPU write");
      $finish;
    end
  end
  initial begin
    #80 pwrUp = 0;
    #80 extReset = 0;
    #100000 $fatal(1, "CPU did not write its result");
  end
endmodule
