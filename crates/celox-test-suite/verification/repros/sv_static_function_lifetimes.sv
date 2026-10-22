module Top;
  function static bit [15:0] saved(input bit enable, input bit [15:0] value);
    if (enable) saved = value;
  endfunction
  function static logic [15:0] saved_logic(input bit enable, input bit [15:0] value);
    if (enable) saved_logic = value;
  endfunction
  function static void remember(input bit enable, input int value, output int saved_output);
    if (!enable) return;
    saved_output = value;
  endfunction
  int result;
  initial begin
    $display("bit %d", saved(1, 23));
    $display("bit %d", saved(0, 91));
    $display("logic %d", saved_logic(1, 23));
    $display("logic %d", saved_logic(0, 91));
    remember(1, 37, result); $display("output %d", result);
    remember(0, 91, result); $display("output %d", result);
    $finish;
  end
endmodule
