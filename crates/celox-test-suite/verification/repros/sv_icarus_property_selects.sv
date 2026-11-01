// Icarus 13.0 rejects the part-select written to the property ("error: Index
// expressions don't apply to this type of property."), as it rejects selects
// of a property read through `this` in a method. IEEE 1800-2023 11.5.1
// defines bit-selects and part-selects of vectors, which class properties of
// packed types are (8.4); Verilator 5.052 and xezim 0.11.3 print "1 3 1034".
module top;
  class C;
    logic [15:0] p;
  endclass
  C c;
  initial begin
    c = new;
    c.p = 16'h1234;
    $write("%b %h ", c.p[2], c.p[7:4]);
    c.p[11:8] = 4'h0;
    $display("%h", c.p);
  end
endmodule
