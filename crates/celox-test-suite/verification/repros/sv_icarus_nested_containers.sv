// Icarus 13.0 rejects an associative array whose index type is a type name,
// a class or a typedef ("Type names are not valid expressions here."), which
// IEEE 1800-2023 7.8 and 7.8.5 permit.
module top;
  class Obj;
  endclass
  initial begin
    int seen[Obj];
    int m[string][int];
    Obj a;
    a = new;
    seen[a] = 1;
    m["a"][1] = 2;
    $display("%0d %0d", seen[a], m["a"][1]);
  end
endmodule
