// xezim 0.11.3: a string member of a class property of unpacked structure
// type prints with %s as a wide packed value padded with spaces, and a
// property of an object held in a handle member reads empty (IEEE 1800-2023
// 7.2, 8.4, 21.2.1.7). Expected: "[A] [A]".
module top;
  class Node;
    string n;
  endclass
  typedef struct { Node m_type; string m_type_name; } pair_t;
  class Holder;
    pair_t orig;
  endclass
  initial begin
    Holder h;
    Node a;
    h = new;
    a = new;
    a.n = "A";
    h.orig.m_type = a;
    h.orig.m_type_name = "A";
    $display("[%s] [%s]", h.orig.m_type_name, h.orig.m_type.n);
  end
endmodule
