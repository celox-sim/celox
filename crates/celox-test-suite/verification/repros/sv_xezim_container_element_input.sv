// xezim 0.11.3 prints
//   in: size 2 first 5; after push: size 3 last 1000
//   in: size 1 first 0; after push: size 1 last x
//   caller: m["a"] size 1
// The input formal is a copy of each actual (IEEE 1800-2023 13.5.1), so the
// second call prints the first line again and m["a"] keeps its 2 elements.
module top;
  function automatic void show(int q[$]);
    $write("in: size %0d first %0d;", q.size(), q[0]);
    q.push_back(1000);
    $display(" after push: size %0d last %0d", q.size(), q[$]);
  endfunction
  int m[string][$];
  int v[$];
  initial begin
    v = '{5, 6};
    m["a"] = '{5, 6};
    show(v);
    show(m["a"]);
    $display("caller: m[\"a\"] size %0d", m["a"].size());
  end
endmodule
