// Control for Icarus, which rejects output parameters on functions.
// This exercises the shared SV subroutine copy-out mechanism using a task.
module Top;
    logic [7:0] positional, named_forward, named_reverse, separate_first, separate_second;
    task automatic outputs(output logic [7:0] first, output logic [7:0] second);
        first = 1; second = 2;
    endtask
    initial begin
        outputs(positional, positional);
        outputs(.first(named_forward), .second(named_forward));
        outputs(.second(named_reverse), .first(named_reverse));
        outputs(.second(separate_second), .first(separate_first));
        $display("review:task_positional=%d task_named_forward=%d task_named_reverse=%d task_separate_first=%d task_separate_second=%d",
                 positional, named_forward, named_reverse, separate_first, separate_second);
        $finish;
    end
endmodule
