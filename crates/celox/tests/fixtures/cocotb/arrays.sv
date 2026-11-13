module Top (
    input logic flags [3:2],
    output logic flag,
    input logic [7:0] words [3:2][-1:1],
    output logic [7:0] first, middle, last
);
    assign flag = flags[2];
    assign first = words[3][-1];
    assign middle = words[2][0];
    assign last = words[2][1];
endmodule
