package TYPE; parameter int value=7; endpackage
module Top;
  class Item; static int value=42; endclass
  class Local #(type TYPE=Item);
    static function int get(); return TYPE::value; endfunction
  endclass
  initial $display("direct=%0d result=%0d pkg=%0d",Item::value,Local#()::get(),TYPE::value);
endmodule
module Wrapped; Top dut(); endmodule
