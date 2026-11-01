// IEEE 1800-2023 7.9.9 and 7.9.11: replace the associative array value, including its
// default. After values = plain, values[missing] is the int element default 0. Xezim
// 0.11.4 retains -17. Deleting entries of copy preserves its own -17 default.

module Top(output bit same);
  int values[string], copy[string], plain[string];
  function automatic int make_default(); return -17; endfunction
  initial begin
    values = '{default: make_default()};
    values["known"] = 5;
    copy = values;
    values = plain;
    same = values["missing"] == 0 && values.num() == 0
      && copy["known"] == 5 && copy["missing"] == -17;
    $display("before_clear same=%b values=%0d copy=%0d,%0d", same, values["missing"], copy["known"], copy["missing"]);
    copy.delete();
    same = same && copy["missing"] == -17 && copy.num() == 0 && !copy.exists("missing");
    $display("same=%b copy_missing=%0d count=%0d exists=%0d", same, copy["missing"], copy.num(), copy.exists("missing"));
    $finish;
  end
endmodule
