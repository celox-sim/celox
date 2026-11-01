// IEEE 1800-2023 21.3.4.2 defines the return count but does not explicitly
// specify the destination after a no-data EOF. Print that tool policy and
// NUL input behavior separately from the portable line-reading assertions.
module Top;
  int fd,n;
  string line;
  initial begin
    fd=$fopen("fgets-lines.txt","w");
    $fwrite(fd,"first\nsecond\n");
    $fclose(fd);
    fd=$fopen("fgets-lines.txt","r");
    n=$fgets(line,fd);
    if (n!=6 || line!="first\n") $fatal(1,"first line");
    n=$fgets(line,fd);
    if (n!=7 || line!="second\n") $fatal(1,"second line");
    n=$fgets(line,fd);
    if (n!=0) $fatal(1,"EOF count");
    $display("EOF: count=%0d length=%0d",n,line.len());
    $fclose(fd);
    fd=$fopen("fgets-bytes.bin","wb");
    $fwrite(fd,"%c%c%c%c",65,0,66,10);
    $fclose(fd);
    fd=$fopen("fgets-bytes.bin","rb");
    n=$fgets(line,fd);
    $display("NUL: count=%0d length=%0d bytes=%0d,%0d,%0d",n,line.len(),line[0],line[1],line[2]);
    $fclose(fd);
    $finish;
  end
endmodule
