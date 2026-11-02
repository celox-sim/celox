// IEEE 1800-2023 6.16.2: putc is equivalent to assigning a character; evaluating the
// selected string variable preserves its index expression effects. Expected calls=2
// index=1 words=12,def same=1. Xezim 0.11.4 prints calls=4 index=2 words=ZXQ,12 same=0:
// both receiver calls and the postfix index are evaluated twice.
module Top(output bit same);
  string words[2] = '{"abc", "def"};
  int calls, index;
  function automatic int select_word(); calls++; return 0; endfunction
  function automatic int character_value(); return "Z"; endfunction
  initial begin
    words[select_word()].putc(1, "X");
    words[index].putc(0, character_value());
    words[select_word()][2] = "Q";
    same = calls == 2 && index == 0 && words[0] == "ZXQ" && words[1] == "def";
    words[index++].itoa(12);
    same = same && index == 1 && words[0] == "12" && words[1] == "def";
    $display("calls=%0d index=%0d words=%s,%s same=%b", calls, index, words[0], words[1], same);
    $finish;
  end
endmodule
