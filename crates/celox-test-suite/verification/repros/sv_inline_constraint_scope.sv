// IEEE 1800-2023 18.7: inline names start at the handle's declared class.
module top;
  class Base;
    rand int x;
  endclass
  class Derived extends Base;
    rand int x;
    constraint c { x == 9; }
  endclass
  Base b;
  Derived d;
  initial begin
    d = new;
    b = d;
    if (!(b.randomize() with { x == 2; })) $fatal(1, "static inline scope");
    if (b.x != 2 || d.x != 9) $fatal(1, "hidden property binding");
    $display("PASS");
    $finish;
  end
endmodule
