// IEEE 1800-2023 6.16.11-14: conversion methods write the selected string variable.
// Verilator 5.052 fails at words[0].itoa(-17) with V3Ast.h:1113: AstNode is not of
// expected type, but instead has type ARRAYSEL. Xezim 0.11.4 passes the shared case.

module Top(output bit same);
  string words[4];
  int index = 2;
  initial begin
    words[0].itoa(-17);
    words[1].hextoa(255);
    words[index].bintoa(5);
    words[3].octtoa(8);
    same = words[0] == "-17" && words[1] == "ff"
      && words[2] == "101" && words[3] == "10";
    $display("words=%s,%s,%s,%s same=%b", words[0], words[1], words[2], words[3], same);
    $finish;
  end
endmodule
