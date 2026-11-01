// IEEE 1800-2023 7.8.6 and 7.9.11: an unknown index reads the specified user default 10xz
// and ignores writes. Xezim 0.11.4 warns and returns 0 for the unknown-index read, while a
// known missing index correctly returns 10xz.

module Top(output bit same);
  logic [3:0] values[logic [7:0]] = '{default: 4'b10xz}, copy[logic [7:0]];
  logic [7:0] unknown_index = 'x;
  initial begin
    values[unknown_index] = 0;
    same = values[unknown_index] === 4'b10xz && values[2] === 4'b10xz
      && values.num() == 0;
    $display("before_write same=%b invalid=%b missing=%b count=%0d", same, values[unknown_index], values[2], values.num());
    values[1] = 4'bz001;
    copy = values;
    values.delete();
    same = same && values[1] === 4'b10xz && copy[1] === 4'bz001
      && copy[2] === 4'b10xz && values.num() == 0;
    $display("same=%b values=%b copy=%b,%b count=%0d", same, values[1], copy[1], copy[2], values.num());
    $finish;
  end
endmodule
