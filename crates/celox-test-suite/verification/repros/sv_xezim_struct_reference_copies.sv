// xezim 0.11.3 prints "mid 5 x" and "end 10 x+ 1": the task's ref formal
// is a copy of shared (IEEE 1800-2023 13.5.2 gives "mid 5 x+" and "end 50 x+ 5").
module top;
    typedef struct { int a; string s; } row_t;
    row_t shared;
    task automatic watch(ref row_t r, output int seen);
        #2 seen = r.a;
        r.s = {r.s, "+"};
        #2 r.a = r.a * 10;
    endtask
    initial begin
        int seen;
        shared = '{1, "x"};
        fork
            watch(shared, seen);
            begin
                #1 shared.a = 5;
                #2 $display("mid %0d %s", shared.a, shared.s);
            end
        join
        $display("end %0d %s %0d", shared.a, shared.s, seen);
    end
endmodule
