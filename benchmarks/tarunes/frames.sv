// SPDX-License-Identifier: MIT
// Headless reference for the same inputs and sampling points as frames.veryl.
`timescale 1ns/1ps
module benchmark_frames;
  logic clk = 0, rst = 0, frame_sync = 1, sfc_data = 1;
  wire [8:0] scanline, cycle;
  wire [5:0] pixel_index;
  wire [15:0] debug_cpu_pc;
  wire [7:0] hdmi_r, hdmi_g, hdmi_b, audio_sample;
  wire hdmi_de, i2s_dout;
  tarunes_top #(.ENABLE_APU(1), .PROM_PATH("benchmark_prg.hex"), .CROM_PATH("benchmark_chr.hex")) dut
    (.clk, .rst, .frame_sync, .sfc_data, .scanline, .cycle, .pixel_index, .debug_cpu_pc,
     .hdmi_r, .hdmi_g, .hdmi_b, .audio_sample, .hdmi_de, .i2s_dout);
  always #5 clk = ~clk;
  integer frames;
  logic [8:0] previous_scanline;
  logic [31:0] pixel_hash, output_hash, hh, ha, hi;
  initial begin
    repeat (4) @(posedge clk);
    @(negedge clk);
    rst = 1;
    frames = 0; previous_scanline = 0; pixel_hash = 0; output_hash = 0; hh = 0; ha = 0; hi = 0;
    for (integer step = 0; step < 300000; step++) begin
      frame_sync = scanline == 261 && cycle == 340;
      @(posedge clk); #1;
      pixel_hash = {pixel_hash[26:0], pixel_hash[31:27]} ^ {26'b0, pixel_index};
      output_hash = {output_hash[28:0], output_hash[31:29]} ^ {hdmi_r, hdmi_g, hdmi_b, audio_sample} ^ {31'b0, i2s_dout} ^ {31'b0, hdmi_de};
      hh = {hh[28:0], hh[31:29]} ^ {8'b0, hdmi_r, hdmi_g, hdmi_b};
      ha = {ha[28:0], ha[31:29]} ^ {24'b0, audio_sample};
      hi = {hi[28:0], hi[31:29]} ^ {31'b0, i2s_dout} ^ {31'b0, hdmi_de};
      if (previous_scanline == 261 && scanline == 0) frames++;
      previous_scanline = scanline;
      if (frames == 3) break;
      @(negedge clk);
    end
    if (frames != 3) $fatal(1, "frame timeout");
    $display("hh=%h ha=%h hi=%h", hh, ha, hi);
    $display("frames=%0d pixel_hash=%h output_hash=%h", frames, pixel_hash, output_hash);
    $display("pc=%0d line=%0d cycle=%0d pixel=%0d", debug_cpu_pc, scanline, cycle, pixel_index);
    if (pixel_hash !== 32'h745d1730 || hh !== 32'hf11b12d6 ||
        ha !== 32'h76363624 || hi !== 32'h85e85e85 ||
        output_hash !== 32'he8ccbe50 || debug_cpu_pc !== 32867)
      $fatal(1, "frame output mismatch");
    $display("COLLECTION_REFERENCE_PASS frames-v1");
    $finish;
  end
endmodule
