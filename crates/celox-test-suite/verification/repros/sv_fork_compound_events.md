# Compound fork events: reviewed external results

Fresh focused runs of `fork::compound_event` used Verilator 5.052,
Icarus 13.0 and the pinned xezim 0.11.3. The four shared cases use separated
time steps; they do not require observing zero-time intermediate expression
results. Celox's stronger per-write checks live in its API tests, outside this
portable selection. IEEE 1800-2023 4.7 allows compound transient coalescing.

The retained `sv/{tool}/fork.json` shards preserve all four actual outcomes.
These are executable reviewed failures, not exclusions. Tool versions and
failure signatures must still match the daily gate.

## xezim: premature or missing compound events

`compound_event_tracks_result_not_operands` produces q=102 instead of 3.
`compound_event_four_state_lsb_edges` produces rises=0 instead of 3; the first
assertion stops the runner, so its falls result was not checked.

The following standalone reductions reproduce premature wakeups with the
pinned binary (no test-suite adapter involved):

```systemverilog
module top;
 initial begin
  automatic int value=0;
  fork
   begin @(value > 2); $display("wake time=%0t value=%0d", $time,value); end
   begin #1 value=1; #1 value=2; #1 value=3; end
  join
 end
endmodule
```

Observed: `wake time=0 value=0`; required wakeup is time 3, value 3.

```systemverilog
module top;
 initial begin
  automatic logic [1:0] value=0;
  fork
   begin @(posedge ({value[1],value[0]})); $display("rise time=%0t",$time); end
   begin #1 value=2'b10; #1 value=2'b01; #1 $display("end"); end
  join
 end
endmodule
```

Observed: `rise time=0`; required wakeup is time 2. IEEE 1800-2023 9.4.2
requires expression-result changes rather than operand-only changes and uses
only the result LSB for edge detection. Table 9-2 also governs the X/Z transitions
in the full shared case. These observations establish the discrepancy without
assuming the simulator's internal cause.

## Verilator: automatic scope lifetime

`compound_event_activations_outlive_parent` terminates with a runtime error:
`Null pointer dereferenced`, pointing at the nested child's `@(value+n == 4)`.
The retained runner detail is `native testbench failed: exit status: 1`;
the more specific diagnostic is printed on the protocol stream. Its reduced
shared source is:

```systemverilog
module Top(output int q);
 task automatic start(input int n);
  int value=0;
  fork begin
   fork
    begin @(value+n == 4); q=q+n; end
    begin #(n+1); value=4-n; end
   join_none
  end join_none
 endtask
 int count=2;
 initial begin q=0; for(int i=1;i<=count;i++) start(i); end
endmodule
```

IEEE 1800-2023 6.21 extends enclosing scope lifetime through all processes
spawned by its fork blocks; 9.3.2 allows children to continue after join_none.
The automatic inputs and local therefore remain available after the task and
intermediate child exit. Celox and xezim both complete with q=3. Verilator
passes the other two two-state cases; its adapter explicitly does not execute
the four-state case.

## Icarus: diagnosed unsupported constructs

The lifetime and pure-function cases report `complex event expressions are not
yet supported in automatic tasks`. The other two report `Overriding the default
variable lifetime is not yet supported`. These are compiler limitations for
legal automatic declarations and event expressions (6.21, 9.4.2), retained as
four compile errors rather than passing or excluded cases.
