package Alias; parameter int value=7; endpackage
module Top;
  class Item; static int value=42; endclass
  class Reader;
    typedef Item Alias;
    extern static function int get();
  endclass
  function int Reader::get(); return Alias::value; endfunction
  initial $display("direct=%0d result=%0d pkg=%0d",Item::value,Reader::get(),Alias::value);
endmodule
module Wrapped; Top dut(); endmodule
