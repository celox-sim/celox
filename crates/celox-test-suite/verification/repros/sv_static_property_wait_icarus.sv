class C;
    static int s_state;
    task wait_for(int s);
        wait (s_state >= s);
        $display("reached %0d at %0d", s, $time);
    endtask
endclass
module Top;
    C c;
    initial begin
        c = new;
        fork
            c.wait_for(2);
            begin #1 c.s_state = 1; #1 c.s_state = 2; end
        join
    end
endmodule
