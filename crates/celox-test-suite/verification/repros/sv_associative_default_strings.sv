// IEEE 1800-2023 7.9.11: the associative array literal assigns the specified default
// value. Changing seed after initialization does not assign a new default. Expected
// before_copy same=1 words0=hello words1=world count=1 index=99 and final copy=hello.
// Xezim 0.11.4 instead changes the default to changed when seed changes, including in
// copies.

module Top(output bit same);
  string seed = "hello";
  string words[int] = '{default: seed}, copy[int];
  int index = 99;
  initial begin
    same = words[1] == "hello" && words.num() == 0 && !words.exists(1)
      && words.first(index) == 0 && index == 99;
    $display("before_copy same=%b words0=%s words1=%s count=%0d index=%0d", same, words[0], words[1], words.num(), index);
    copy = words;
    seed = "changed";
    words[1] = "world";
    same = same && words[0] == "hello" && words[1] == "world"
      && copy[1] == "hello" && copy.num() == 0;
    $display("before_copy same=%b words0=%s words1=%s count=%0d index=%0d", same, words[0], words[1], words.num(), index);
    copy = words;
    words.delete(1);
    same = same && words[1] == "hello" && words.num() == 0
      && copy[1] == "world" && copy[2] == "hello";
    copy.delete();
    words = '{default: "other"};
    #1;
    same = same && copy[7] == "hello" && copy.num() == 0
      && words[7] == "other" && words.num() == 0;
    $display("same=%b words=%s copy=%s counts=%0d,%0d", same, words[7], copy[7], words.num(), copy.num());
    $finish;
  end
endmodule
