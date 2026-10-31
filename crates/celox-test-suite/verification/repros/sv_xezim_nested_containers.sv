// xezim 0.11.3: first() of a class property that is an associative array of
// associative arrays indexed by class handles reports an empty array, and
// new[] assigned to an element of a dynamic array of dynamic arrays is lost
// (IEEE 1800-2023 7.8.5, 7.9.5, 7.5.1). Expected: "first 1 a" and "3 7".
module top;
  class Obj;
    string n;
  endclass
  class Holder;
    int m[Obj][Obj];
  endclass
  initial begin
    Holder h;
    int grid[][];
    Obj a, k;
    h = new;
    a = new;
    a.n = "a";
    h.m[a][a] = 1;
    $display("first %0d %s", h.m.first(k), k == null ? "-" : k.n);
    grid = new[2];
    grid[1] = new[3];
    grid[1][2] = 7;
    $display("%0d %0d", grid[1].size(), grid[1][2]);
  end
endmodule
