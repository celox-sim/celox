// Verilator 5.052 rejects this with %Error-IMPURE "External variable referenced by
// non-inlined function/task: 'set'" (also with a ref Node formal); IEEE 1800-2023
// 13.5.2 permits a class property actual. Celox prints "h".
module Top;
    class Node;
        string text;
        function new(string text);
            this.text = text;
        endfunction
    endclass
    typedef struct { Node m_type; } pair_t;
    class Holder;
        pair_t p;
    endclass
    Holder h = new;
    function automatic void set(ref pair_t r, input string text);
        r.m_type = new(text);
    endfunction
    initial begin
        set(h.p, "h");
        $display("%s", h.p.m_type.text);
    end
endmodule
