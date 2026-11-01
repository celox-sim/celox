// IEEE 1800-2023 21.3.7 requires a zero status and cleared description
// when the most recent I/O operation had no error.
module Top;
  int fd,code;
  string text="old";
  logic [639:0] packed_text='1;
  initial begin
    fd=$fopen("ferror-probe.txt","w");
    code=$ferror(fd,text);
    $display("string: code=%0d length=%0d text=%s",code,text.len(),text);
    code=$ferror(fd,packed_text);
    $display("packed: code=%0d cleared=%0d",code,packed_text==0);
    $fclose(fd);
    $finish;
  end
endmodule
