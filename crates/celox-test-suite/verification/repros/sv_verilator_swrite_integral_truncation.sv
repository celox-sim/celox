// $swrite into an integral variable assigns the string as a string literal
// is assigned (IEEE 1800-2023 21.3.3, 5.9): a string longer than the
// variable keeps its rightmost characters. The plain literal assignment
// shows the expected value.
module top;
  bit [7:0] by_swrite, by_literal;
  initial begin
    $swrite(by_swrite, "%0s", "XYZ");
    by_literal = "XYZ";
    $display("swrite=%h literal=%h", by_swrite, by_literal);
  end
endmodule
