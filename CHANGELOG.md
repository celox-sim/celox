# Changelog

## 0.14.0 (2026-11-20)

## What's Changed
* perf(scheduler): avoid redundant change tracking by @tignear in https://github.com/celox-sim/celox/pull/3013
* chore(deps): update rust toolchain to 6b29b8e by @renovate[bot] in https://github.com/celox-sim/celox/pull/3016
* fix(sir): preserve loaded values across extraction folds by @tignear in https://github.com/celox-sim/celox/pull/3018
* fix(sir): preserve effects on case dispatch exit paths by @tignear in https://github.com/celox-sim/celox/pull/3019
* feat(sv): support strobed output after time-slot settling by @tignear in https://github.com/celox-sim/celox/pull/2976
* feat(sir): optimize programs and function units with calls by @tignear in https://github.com/celox-sim/celox/pull/2993
* chore(deps): update mbx to 1.29.1 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/3028
* feat(jit): add opt-in incremental compilation of hot execution groups by @tignear in https://github.com/celox-sim/celox/pull/3026
* perf(jit): share hot kernels in a compact native scheduler by @tignear in https://github.com/celox-sim/celox/pull/3027
* feat(sv): monitor static values at time-slot boundaries by @tignear in https://github.com/celox-sim/celox/pull/3033
* feat(sv): support hierarchical string variables by @tignear in https://github.com/celox-sim/celox/pull/3031
* fix(sv): infer flip-flop clocks independently of event order by @tignear in https://github.com/celox-sim/celox/pull/3032
* perf(scheduler): reduce repeated dependency slicing and discarded copies by @tignear in https://github.com/celox-sim/celox/pull/3036
* fix(jit): preserve loop invariants across native region exits by @tignear in https://github.com/celox-sim/celox/pull/3038
* perf(cli): use mimalloc for compilation allocations by @tignear in https://github.com/celox-sim/celox/pull/3041
* perf(jit): pool remaining-use records in a block arena by @tignear in https://github.com/celox-sim/celox/pull/3042
* perf(sv): borrow expressions when lowering needs no rewrite by @tignear in https://github.com/celox-sim/celox/pull/3043
* perf(scheduler): reuse dependency analysis without cloning executable IR by @tignear in https://github.com/celox-sim/celox/pull/3045
* chore(deps): update pnpm to v12.16.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3048
* chore(deps): update rust toolchain to fb3eee9 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3054
* perf(x86): schedule balanced reductions when operands are ready by @tignear in https://github.com/celox-sim/celox/pull/3051
* perf(x86): reuse predicates for conditional counter updates by @tignear in https://github.com/celox-sim/celox/pull/3052
* perf(jit): overlap scheduler allocation with hot group compilation by @tignear in https://github.com/celox-sim/celox/pull/3053
* chore(deps): update rust crate parol to v5.0.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3057
* ci: stop running the Veryl reference simulator in validation by @tignear in https://github.com/celox-sim/celox/pull/3059
* chore(deps): Bump markdown-it from 14.3.0 to 14.3.3 by @dependabot[bot] in https://github.com/celox-sim/celox/pull/3061
* feat(jit): expose hotspot execution statistics by @tignear in https://github.com/celox-sim/celox/pull/3067
* perf(x86): reuse reload snapshot proofs during verification by @tignear in https://github.com/celox-sim/celox/pull/3068
* chore(deps): update pnpm to v12.16.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3074
* perf(scheduler)!: compile larger RTL schedules and counted events by @tignear in https://github.com/celox-sim/celox/pull/3069
* perf(jit): overlap scheduler callee compilation with comb by @tignear in https://github.com/celox-sim/celox/pull/3070
* perf(cli): select hotspot JIT for supported runs by default by @tignear in https://github.com/celox-sim/celox/pull/3071
* chore(deps): update rust crate iced-x86 to v1.22.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3078
* perf(scheduler): compile clocks without source event domains by @tignear in https://github.com/celox-sim/celox/pull/3076
* feat(scheduler): report compiled runs and native service calls by @tignear in https://github.com/celox-sim/celox/pull/3080
* feat(sir): lower native scheduler memory operations without host calls by @tignear in https://github.com/celox-sim/celox/pull/3081
* fix(scheduler): consume Active occurrences before NBA waves by @tignear in https://github.com/celox-sim/celox/pull/3082
* perf(scheduler): keep repeated Active domain waves in native code by @tignear in https://github.com/celox-sim/celox/pull/3083
* perf(scheduler): keep loop budget checks in native code by @tignear in https://github.com/celox-sim/celox/pull/3085
* perf(sv): share scope constant tables as persistent maps by @tignear in https://github.com/celox-sim/celox/pull/3096
* feat(sir): let the scheduler, settle, layout and frontend analyses handle calls by @tignear in https://github.com/celox-sim/celox/pull/3098
* perf(sv): settle a scope's declarations once for the queries of its contexts by @tignear in https://github.com/celox-sim/celox/pull/3099
* perf(scheduler): keep lazy settles and compiled kernels for units with calls by @tignear in https://github.com/celox-sim/celox/pull/3101
* feat(sv): lower subroutines out of line behind a test switch by @tignear in https://github.com/celox-sim/celox/pull/3103
* feat(sv): discover class specializations named with procedural typedefs and localparams by @tignear in https://github.com/celox-sim/celox/pull/3110
* chore(deps): update mbx to 1.29.2 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/3122
* perf(scheduler): compile general event controls by @tignear in https://github.com/celox-sim/celox/pull/3091
* feat(sv): bind return values against the declared result type by @tignear in https://github.com/celox-sim/celox/pull/3105
* perf(scheduler): run opaque wait dependencies in native code by @tignear in https://github.com/celox-sim/celox/pull/3092
* feat(sv): randomize constrained classes with handle, string and container state by @tignear in https://github.com/celox-sim/celox/pull/3123
* feat(sv): decide out-of-line eligibility without lowering a module again by @tignear in https://github.com/celox-sim/celox/pull/3127
* perf(scheduler): generate general event waves for RTL writers by @tignear in https://github.com/celox-sim/celox/pull/3093
* perf(scheduler): compile combinational observer activation by @tignear in https://github.com/celox-sim/celox/pull/3106
* perf(scheduler): keep inactive delays in generated event waves by @tignear in https://github.com/celox-sim/celox/pull/3107
* perf(scheduler): preserve event writes across native host calls by @tignear in https://github.com/celox-sim/celox/pull/3108
* perf(scheduler): generate queued NBA capture and publication by @tignear in https://github.com/celox-sim/celox/pull/3109
* perf(scheduler): compile dynamic and event-driven NBA destinations by @tignear in https://github.com/celox-sim/celox/pull/3111
* perf(scheduler): schedule delayed NBA updates in native code by @tignear in https://github.com/celox-sim/celox/pull/3113
* perf(scheduler): resume restored active queues in native code by @tignear in https://github.com/celox-sim/celox/pull/3114
* feat(sv): randomize the objects rand class handles refer to by @tignear in https://github.com/celox-sim/celox/pull/3132
* chore(deps): update dependency @types/node to v26.6.6 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3138
* perf(scheduler): process host input changes in native event waves by @tignear in https://github.com/celox-sim/celox/pull/3115
* perf(scheduler): resume portable region queues in native code by @tignear in https://github.com/celox-sim/celox/pull/3116
* perf(scheduler): run public steps in generated event loops by @tignear in https://github.com/celox-sim/celox/pull/3118
* fix(scheduler): compare logical state in mixed differential runs by @tignear in https://github.com/celox-sim/celox/pull/3120
* perf(scheduler): compile canonical and shared event domains by @tignear in https://github.com/celox-sim/celox/pull/3121
* feat(sv): randomize rand dynamic arrays and queues by their size constraints by @tignear in https://github.com/celox-sim/celox/pull/3147
* feat(sv): solve shifts in constraints by @tignear in https://github.com/celox-sim/celox/pull/3144
* chore(deps): update rust crate test-case to v3.4.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3154
* chore(deps): update dependency @biomejs/biome to v2.6.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3158
* feat(sv): copy output and inout arguments out to class properties by @tignear in https://github.com/celox-sim/celox/pull/3151
* feat(sv): lower heap and container calls in function units by @tignear in https://github.com/celox-sim/celox/pull/3145
* feat(sv): wait on interface members through virtual interfaces in class properties by @tignear in https://github.com/celox-sim/celox/pull/3152
* ci(sv-tests): cap conformance cases at 78 seconds over five PR shards by @tignear in https://github.com/celox-sim/celox/pull/3163
* feat(sv): draw on the calling process's RNG in function units by @tignear in https://github.com/celox-sim/celox/pull/3164
* feat(sv): solve constraint loops over the elements of state arrays by @tignear in https://github.com/celox-sim/celox/pull/3146
* feat(sv): build associative arrays from keyed patterns and merge containers of ambiguous conditionals by @tignear in https://github.com/celox-sim/celox/pull/3148
* feat(sv): raise prints and messages in function units by @tignear in https://github.com/celox-sim/celox/pull/3173
* feat(sv): call subroutines from concurrent assertion actions by @tignear in https://github.com/celox-sim/celox/pull/3172
* perf(scheduler): keep scheduler state resident between compiled runs by @tignear in https://github.com/celox-sim/celox/pull/3177
* feat(backend): enable hotspot JIT on AArch64 and by default by @tignear in https://github.com/celox-sim/celox/pull/3176
* chore(deps): update rust toolchain to 19e0e6c by @renovate[bot] in https://github.com/celox-sim/celox/pull/3112
* ci(bench): run the Heliodor tiered benchmark with hotspot JIT by @tignear in https://github.com/celox-sim/celox/pull/3187
* fix(scheduler): run a resident wake at the last time by @tignear in https://github.com/celox-sim/celox/pull/3186
* perf(scheduler): enter and leave compiled runs through raw slot access by @tignear in https://github.com/celox-sim/celox/pull/3182
* perf(sv): settle class and constant scopes once per scope by @tignear in https://github.com/celox-sim/celox/pull/3191
* feat(sv): make host requests inside function units by @tignear in https://github.com/celox-sim/celox/pull/3179
* perf(scheduler): keep observed-data slots compiled when no domain wakes a waiter by @tignear in https://github.com/celox-sim/celox/pull/3192
* fix(tiered): defer derived-edge units and run RTLMeter with hotspot JIT by @tignear in https://github.com/celox-sim/celox/pull/3195
* fix(sv): display severity task messages with the conversions of $display by @tignear in https://github.com/celox-sim/celox/pull/3198
* test(sv): fail sv-tests simulations that report a run-time error by @tignear in https://github.com/celox-sim/celox/pull/3199
* fix(scheduler): stop delayed NBA publication from copying wait checks per site by @tignear in https://github.com/celox-sim/celox/pull/3204
* docs(bench): label RTLMeter's Celox series as tiered hotspot JIT by @tignear in https://github.com/celox-sim/celox/pull/3209
* perf(hotspot): outline scheduler kernels past an inline budget by @tignear in https://github.com/celox-sim/celox/pull/3207
* perf(hotspot): replace a budgeted scheduler with its fully inlined stage by @tignear in https://github.com/celox-sim/celox/pull/3208
* feat(sv): assert named sequences and properties without arguments by @tignear in https://github.com/celox-sim/celox/pull/3200
* perf(sv): walk each subroutine body once when computing timed-task effects by @tignear in https://github.com/celox-sim/celox/pull/3212
* feat(sv): delays, $stop and $finish inside out-of-line tasks by @tignear in https://github.com/celox-sim/celox/pull/3214
* refactor(sv): record class ancestry in the semantic type table by @tignear in https://github.com/celox-sim/celox/pull/3219
* feat(sv): type conditionals whose arms are handles of different classes by @tignear in https://github.com/celox-sim/celox/pull/3220
* fix(sv): bind imported subroutine calls inside fork branches by @tignear in https://github.com/celox-sim/celox/pull/3221
* perf(frontend): gate always_comb on one startup readiness bit by @tignear in https://github.com/celox-sim/celox/pull/3225
* fix(sv): reject pure virtual methods in classes that are not abstract by @tignear in https://github.com/celox-sim/celox/pull/3226
* fix(sv): check class handles passed to input and inout formals by @tignear in https://github.com/celox-sim/celox/pull/3227
* perf(settle): branch over reader marks of unchanged stores by @tignear in https://github.com/celox-sim/celox/pull/3234
* fix(vpi): run value change callbacks after each update by @tignear in https://github.com/celox-sim/celox/pull/3202
* perf(sv): propagate timed-task effects to callers through a worklist by @tignear in https://github.com/celox-sim/celox/pull/3235
* perf(sv): settle the scope around a class once for all of its methods by @tignear in https://github.com/celox-sim/celox/pull/3238
* feat(sim): show blocking updates of watched signals at value change points by @tignear in https://github.com/celox-sim/celox/pull/3217
* feat(sv): declare and compare types with the type operator by @tignear in https://github.com/celox-sim/celox/pull/3249
* refactor(sv): judge casts by a castable relation of semantic types by @tignear in https://github.com/celox-sim/celox/pull/3242
* chore(deps): update rust toolchain to 74cebce by @renovate[bot] in https://github.com/celox-sim/celox/pull/3255
* feat(sv): process methods and module variable writes in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3257
* feat(sv): cast between numeric types with $cast by @tignear in https://github.com/celox-sim/celox/pull/3232
* feat(sv): replicate string literals by a count that is not constant by @tignear in https://github.com/celox-sim/celox/pull/3260
* feat(sv): ref formals in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3258
* feat(sv): name the members of an enumeration a class property declares by @tignear in https://github.com/celox-sim/celox/pull/3265
* feat(sv): automatic unpacked arrays of out-of-line units with one live activation by @tignear in https://github.com/celox-sim/celox/pull/3262
* feat(sv): lower concurrent sequences with fixed cycle delays and local variables by @tignear in https://github.com/celox-sim/celox/pull/3245
* fix(sv): check class handles that functions return by @tignear in https://github.com/celox-sim/celox/pull/3228
* feat(sv): event triggers in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3269
* fix(sv): check class handle defaults of formals by @tignear in https://github.com/celox-sim/celox/pull/3229
* chore(deps): update pnpm to v12.17.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3272
* fix(sv): check class handles that output formals copy back by @tignear in https://github.com/celox-sim/celox/pull/3230
* feat(sv): process control and semaphore keys in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3276
* feat(sv): unpacked arrays in the frames of out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3267
* feat(sv): stores into what waits watch in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3280
* fix(sv): assign the elements of dynamically sized arrays to fixed-size ones by @tignear in https://github.com/celox-sim/celox/pull/3281
* feat(sv): mailboxes and blocking process methods in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3282
* feat(sv): wake level waits on stores of class properties by @tignear in https://github.com/celox-sim/celox/pull/3284
* fix(sv): pack strings and containers in bit-stream casts by @tignear in https://github.com/celox-sim/celox/pull/3243
* feat(sv): static casts of class handles by @tignear in https://github.com/celox-sim/celox/pull/3247
* feat(sv): $cast to singular destinations of any type by @tignear in https://github.com/celox-sim/celox/pull/3248
* fix(sv): reject size and signing casts of values that are not integral by @tignear in https://github.com/celox-sim/celox/pull/3254
* feat(sv): lower recursive and large subroutines out of line by default by @tignear in https://github.com/celox-sim/celox/pull/3285
* fix(sv): check class handles in array assignment patterns by @tignear in https://github.com/celox-sim/celox/pull/3231
* feat(sv): let out-of-line units write the caller's locals that ref formals name by @tignear in https://github.com/celox-sim/celox/pull/3298
* fix(sv): assign dynamic arrays of any element type to fixed-size ones by @tignear in https://github.com/celox-sim/celox/pull/3297
* chore(deps): update rust crate toml to v1.2.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3303
* feat(sv): $cast of dynamically sized arrays to fixed-size ones by @tignear in https://github.com/celox-sim/celox/pull/3288
* fix(sv): reject fixed-size arrays of container typedefs by @tignear in https://github.com/celox-sim/celox/pull/3306
* refactor(sv): check class handle assignments through one check_assign by @tignear in https://github.com/celox-sim/celox/pull/3307
* docs(sv): document only deviations from IEEE 1800-2023 by @tignear in https://github.com/celox-sim/celox/pull/3309
* perf: speed up the E902 CoreMark simulation by @tignear in https://github.com/celox-sim/celox/pull/3295
* feat(waveform): add FST waveform output alongside VCD by @tignear in https://github.com/celox-sim/celox/pull/3312
* feat(sv): property iff between Boolean expressions by @tignear in https://github.com/celox-sim/celox/pull/3321
* feat(sv): fork in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3302
* chore(deps): update rust toolchain to d3fe742 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3333
* feat(sv): event controls and level waits in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3304
* fix(sv): body parameters of packages and the compilation unit are local by @tignear in https://github.com/celox-sim/celox/pull/3334
* feat(sv): named event waits in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3322
* feat(sv): nonblocking assignments in out-of-line units by @tignear in https://github.com/celox-sim/celox/pull/3326
* fix(sv): name each static variable of a subroutine by its owner and declaration by @tignear in https://github.com/celox-sim/celox/pull/3337
* feat(sv): expect statements by @tignear in https://github.com/celox-sim/celox/pull/3324
* feat(sv): waits on variables that forked children share by @tignear in https://github.com/celox-sim/celox/pull/3327
* feat(sv): event controls on class properties by @tignear in https://github.com/celox-sim/celox/pull/3328
* fix(sv): take packed dimensions from the analysis's type table by @tignear in https://github.com/celox-sim/celox/pull/3330
* fix(x86): address frame slots of block copies and fills from the frame register by @tignear in https://github.com/celox-sim/celox/pull/3348
* fix(sv): bind static property initializers in the scope of their class by @tignear in https://github.com/celox-sim/celox/pull/3345
* feat(sv): unpacked arrays of handles in the frames of function units by @tignear in https://github.com/celox-sim/celox/pull/3331
* fix(sv): lower the initializers of packages with out-of-line calls by @tignear in https://github.com/celox-sim/celox/pull/3347
* perf(slt): mark only the readers of the bits a partition changed by @tignear in https://github.com/celox-sim/celox/pull/3353
* fix(sv): name the subroutine that calls a task in %m by @tignear in https://github.com/celox-sim/celox/pull/3359
* fix(sv): keep whether the else arm runs across suspending calls by @tignear in https://github.com/celox-sim/celox/pull/3366
* feat(settle): dump the partitioned settle's plan with a dependency lint by @tignear in https://github.com/celox-sim/celox/pull/3356
* feat(settle): count each partition's runs and the runs that changed nothing by @tignear in https://github.com/celox-sim/celox/pull/3358
* fix(sv): read string variables of file I/O as their text by @tignear in https://github.com/celox-sim/celox/pull/3371
* refactor(sv): check unpacked structure assignments through check_assign by @tignear in https://github.com/celox-sim/celox/pull/3375
* refactor(sv): check virtual interface assignments through the type table by @tignear in https://github.com/celox-sim/celox/pull/3376
* refactor(sv): judge ref arguments by type equivalence in the type table by @tignear in https://github.com/celox-sim/celox/pull/3377
* fix(sv): count time in the global precision for finer-precision scopes by @tignear in https://github.com/celox-sim/celox/pull/3373
* refactor(sv): check array and container assignments through check_types by @tignear in https://github.com/celox-sim/celox/pull/3378
* fix(sv): check assignments to enumerated places through check_assign by @tignear in https://github.com/celox-sim/celox/pull/3385
* test(settle): mark a process for what its out-of-line callee reads by @tignear in https://github.com/celox-sim/celox/pull/3387
* fix(frontend-sv): keep always @* procedures whose callees read more as processes by @tignear in https://github.com/celox-sim/celox/pull/3389
* fix(sv): keep sampled copies and $past histories out of process frames by @tignear in https://github.com/celox-sim/celox/pull/3390
* refactor(sv): give process frames only the scratch their lowering opts into by @tignear in https://github.com/celox-sim/celox/pull/3394
* fix(sv): let function units a process calls store events and wired variables by @tignear in https://github.com/celox-sim/celox/pull/3397
* feat(sv): report elaboration phase times and dump the plan without codegen by @tignear in https://github.com/celox-sim/celox/pull/3395
* fix(slt): preserve external wait flags on whole-variable copy targets by @tignear in https://github.com/celox-sim/celox/pull/3405
* perf(sv): stop analyzing virtual interface designs twice by @tignear in https://github.com/celox-sim/celox/pull/3406
* fix(settle): retain publish marks for readers touching other publisher writes by @tignear in https://github.com/celox-sim/celox/pull/3408
* ci(rtlmeter): add e902, e906 and e203 completion and cycle check by @tignear in https://github.com/celox-sim/celox/pull/3411
* perf(tiered): escalate from hotspot JIT to whole-image native execution by @tignear in https://github.com/celox-sim/celox/pull/3414
* fix(tiered): keep mixed executor in hotspot scheduler unit tests by @tignear in https://github.com/celox-sim/celox/pull/3418
* perf(rtlmeter): measure steady-state simulation throughput with coremark by @tignear in https://github.com/celox-sim/celox/pull/3412
* ci(deps): Bump Mergifyio/setup-cli from 1.3.0 to 1.4.0 by @dependabot[bot] in https://github.com/celox-sim/celox/pull/3422
* ci(deps): Bump github/codeql-action/init from 4.39.1 to 4.39.3 by @dependabot[bot] in https://github.com/celox-sim/celox/pull/3423
* ci: migrate workflows from pull_request_target to pull_request by @tignear in https://github.com/celox-sim/celox/pull/3434
* chore(deps): update rust crate toml to v1.2.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3427
* chore(deps): update rust toolchain to b705383 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3399
* feat(sv): strings, pattern type keys and external constraints by @tignear in https://github.com/celox-sim/celox/pull/3360
* fix(runtime): copy sampled values through tracked writes by @tignear in https://github.com/celox-sim/celox/pull/3438
* feat(sv): wildcard indices, pull/supply nets, wildcard ports by @tignear in https://github.com/celox-sim/celox/pull/3361
* feat(rtlmeter): register Servant and XuanTie-C906 cases by @tignear in https://github.com/celox-sim/celox/pull/3449
* perf(sv): drop two-state assertion monitors that can never fail by @tignear in https://github.com/celox-sim/celox/pull/3439
* perf(sim): drop sampled copies no unit reads by @tignear in https://github.com/celox-sim/celox/pull/3440
* perf(sv): substitute unobserved combinational nets into their readers by @tignear in https://github.com/celox-sim/celox/pull/3453
* feat(cli): apply --sir-pass to SystemVerilog designs by @tignear in https://github.com/celox-sim/celox/pull/3454
* perf(sv): fuse flip-flops with delayed nonblocking assignments by @tignear in https://github.com/celox-sim/celox/pull/3441
* perf(scheduler): refresh sampled shadows inside compiled runs by @tignear in https://github.com/celox-sim/celox/pull/3442
* perf(tiered): use wide flip-flop commits in tiered layouts by @tignear in https://github.com/celox-sim/celox/pull/3443
* build(rtlmeter): update Verilator to 5.054 by @tignear in https://github.com/celox-sim/celox/pull/3444
* perf(settle): partition large estimated settles more finely by @tignear in https://github.com/celox-sim/celox/pull/3448
* perf(sv): leave scope tables alone when an edit changes nothing by @tignear in https://github.com/celox-sim/celox/pull/3461
* perf(sv): collect only static class functions as constant functions by @tignear in https://github.com/celox-sim/celox/pull/3462
* chore(deps): update rust toolchain to 89750a2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3465
* chore(deps): update mbx to 1.30.0 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/3470
* perf(sv): bind type operands of queries without discovering the scope by @tignear in https://github.com/celox-sim/celox/pull/3473
* perf(sv): merge scope symbols by name sets by @tignear in https://github.com/celox-sim/celox/pull/3474
* perf(sv): keep type alias tables in a persistent map by @tignear in https://github.com/celox-sim/celox/pull/3476
* fix(settle): keep a commit that follows a wide run's shadows by @tignear in https://github.com/celox-sim/celox/pull/3479
* feat(sv): randcase and named-block disable by @tignear in https://github.com/celox-sim/celox/pull/3362
* perf(sv): analyze a package once when nothing hides its self-qualified names by @tignear in https://github.com/celox-sim/celox/pull/3480
* fix(sv): key a disabled block by its subroutine, not by a per-scope count by @tignear in https://github.com/celox-sim/celox/pull/3483
* perf(sv): analyze a package once for the specializations other scopes ask of it by @tignear in https://github.com/celox-sim/celox/pull/3484
* feat(sv): Clause 40 coverage system functions by @tignear in https://github.com/celox-sim/celox/pull/3363
* feat(sv-tests): elaborate every module root by @tignear in https://github.com/celox-sim/celox/pull/3364
* perf(sv): take over the classes a package's last analysis lowered by @tignear in https://github.com/celox-sim/celox/pull/3489
* refactor(simulator): split the host simulator into modules and group its state by @tignear in https://github.com/celox-sim/celox/pull/3490
* refactor(native): split the native backend into modules by @tignear in https://github.com/celox-sim/celox/pull/3495
* refactor(scheduler): split the scheduler into modules and build plan and units in phases by @tignear in https://github.com/celox-sim/celox/pull/3497
* refactor(wasm): split the wasm backend into modules by instruction kind by @tignear in https://github.com/celox-sim/celox/pull/3493
* refactor(builder): split the simulator builder into modules by @tignear in https://github.com/celox-sim/celox/pull/3492
* refactor(sv): split the procedural lowering into modules and group its state by @tignear in https://github.com/celox-sim/celox/pull/3498
* refactor(slt): split the logic path scheduler into modules by @tignear in https://github.com/celox-sim/celox/pull/3494
* refactor(native): split lower_instruction into one function per instruction kind by @tignear in https://github.com/celox-sim/celox/pull/3496
* refactor(sv): split lowering.rs into topical modules by @tignear in https://github.com/celox-sim/celox/pull/3499
* refactor(tiered): split the tiered backend into modules by @tignear in https://github.com/celox-sim/celox/pull/3500
* refactor(x86): move the large inline test modules to their own files by @tignear in https://github.com/celox-sim/celox/pull/3511
* refactor(sir-opt): move the large inline test modules to their own files by @tignear in https://github.com/celox-sim/celox/pull/3510
* refactor(testbench): split the testbench runner into modules by @tignear in https://github.com/celox-sim/celox/pull/3509
* refactor: move the remaining large inline test modules to their own files by @tignear in https://github.com/celox-sim/celox/pull/3512
* refactor(veryl): split the logic tree lowering and its largest functions by @tignear in https://github.com/celox-sim/celox/pull/3513
* refactor(frontend): split symbolic assembly into modules and schedule in phases by @tignear in https://github.com/celox-sim/celox/pull/3514
* refactor(sv): split the procedural analysis and the direct value binder by @tignear in https://github.com/celox-sim/celox/pull/3516
* refactor(native): split the remaining large functions of the native code generators by @tignear in https://github.com/celox-sim/celox/pull/3521
* refactor(sv): split the combinational, call and module-context lowering and group their state by @tignear in https://github.com/celox-sim/celox/pull/3530
* refactor(x86): split reload and spill planning into topical modules by @tignear in https://github.com/celox-sim/celox/pull/3532
* refactor(sir-opt): split the two largest passes into topical modules by @tignear in https://github.com/celox-sim/celox/pull/3531
* refactor(sv): split ast.rs and elaborate module scopes and interface bindings in phases by @tignear in https://github.com/celox-sim/celox/pull/3551
* feat(sv): accept DPI-C imports at compilation-unit scope by @tignear in https://github.com/celox-sim/celox/pull/3535
* refactor(napi): split the N-API bindings into modules by @tignear in https://github.com/celox-sim/celox/pull/3539
* refactor(veryl): split the testbench compiler and the module parser into modules by @tignear in https://github.com/celox-sim/celox/pull/3550
* perf(tiered): share the program between the tiers instead of copying it by @tignear in https://github.com/celox-sim/celox/pull/3549
* refactor: split the remaining large functions of the core, Cranelift, wasm and VPI runner by @tignear in https://github.com/celox-sim/celox/pull/3552
* refactor(veryl): split the sequential lowering into modules and group its state by @tignear in https://github.com/celox-sim/celox/pull/3553
* feat(sv): bind an upward name from instances at different depths to its one object by @tignear in https://github.com/celox-sim/celox/pull/3556
* chore(deps): update rust toolchain to 0070500 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3579
* chore(deps): update vitest monorepo to v5.1.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3433
* chore(deps): update dependency @biomejs/biome to v2.6.5 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3584
* fix(sv): expand a macro that ends at a token paste before joining by @tignear in https://github.com/celox-sim/celox/pull/3523
* fix(cli): take file-list sources of any extension as SystemVerilog by @tignear in https://github.com/celox-sim/celox/pull/3524
* fix(sv): store the text of $value$plusargs in a string variable by @tignear in https://github.com/celox-sim/celox/pull/3525
* fix(sv): let parser arenas grow with the compilation unit by @tignear in https://github.com/celox-sim/celox/pull/3526
* fix(sv): type assignment patterns in the branches of a conditional assigned to a selection by @tignear in https://github.com/celox-sim/celox/pull/3527
* feat(sv): connect interface-port modules with wildcard port connections by @tignear in https://github.com/celox-sim/celox/pull/3528
* perf(sv): decide two-state branch conditions without revisiting shared operands by @tignear in https://github.com/celox-sim/celox/pull/3563
* feat(sv): take members of packed structure parameters as elaboration constants by @tignear in https://github.com/celox-sim/celox/pull/3540
* perf(sv): query the dimensions of generate-block variables without discovering the scope by @tignear in https://github.com/celox-sim/celox/pull/3574
* fix(sv): name the formals and result of a static subroutine by its declaration by @tignear in https://github.com/celox-sim/celox/pull/3591
* perf(sv): look up the storage of subroutines through an index that follows the names by @tignear in https://github.com/celox-sim/celox/pull/3590
* perf(native): compile function units against one layout instead of a copy each by @tignear in https://github.com/celox-sim/celox/pull/3593
* feat(sv): resolve the continuous drivers of a net by @tignear in https://github.com/celox-sim/celox/pull/3529
* test(sv): run the sv-tests driver on the allocator of the celox CLI by @tignear in https://github.com/celox-sim/celox/pull/3596
* perf(sir): collect the effects of calls once per component of the call graph by @tignear in https://github.com/celox-sim/celox/pull/3594
* fix(bench): accept hotspot evidence after whole-image promotion by @tignear in https://github.com/celox-sim/celox/pull/3606
* perf(sim): find the heap handles of a process value by bisection by @tignear in https://github.com/celox-sim/celox/pull/3602
* feat(sv): pass strings, outputs and packed arrays to DPI-C imports by @tignear in https://github.com/celox-sim/celox/pull/3536
* feat(cli): link DPI-C libraries in celox run by @tignear in https://github.com/celox-sim/celox/pull/3537
* fix(sv): accept a conditional expression as a select index by @tignear in https://github.com/celox-sim/celox/pull/3538
* test(sv): stop a simulation of the sv-tests driver at a time, and report how far it ran by @tignear in https://github.com/celox-sim/celox/pull/3599
* fix(sv): preserve specialized class parameter types by @tignear in https://github.com/celox-sim/celox/pull/2974
* ci(sv-tests): run the UVM cases only in full validation by @tignear in https://github.com/celox-sim/celox/pull/3608
* perf(sim): save and restore the frame of an activation as the bytes of its slots by @tignear in https://github.com/celox-sim/celox/pull/3607
* fix(sv): share the static variables of methods a package specialization lowers by @tignear in https://github.com/celox-sim/celox/pull/3603
* fix(sv): leave defaults that name undeclared variables to the calls that use them by @tignear in https://github.com/celox-sim/celox/pull/3614
* perf(sv): keep in a process frame only the temporaries live across a suspension by @tignear in https://github.com/celox-sim/celox/pull/3615
* chore(deps): update pnpm to v12.18.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3618
* perf(sv): find why a subroutine stays inlined once per component of the call graph by @tignear in https://github.com/celox-sim/celox/pull/3597
* feat(sv): take an array parameter's value from an expression of its type by @tignear in https://github.com/celox-sim/celox/pull/3571
* fix(sv)!: report undeclared type names, also of typedefs no module uses by @tignear in https://github.com/celox-sim/celox/pull/3374
* refactor(sv): tell a chandle by its type by @tignear in https://github.com/celox-sim/celox/pull/3617
* refactor(sv): make chandle a kind of type, not a flag on every type by @tignear in https://github.com/celox-sim/celox/pull/3622
* feat(sv): run an always_ff whose event control names no edge by @tignear in https://github.com/celox-sim/celox/pull/3541
* fix(sv): name the formals, result and this of a subroutine by its declaration by @tignear in https://github.com/celox-sim/celox/pull/3619
* feat(sv)!: always build SystemVerilog and DPI-C support by @tignear in https://github.com/celox-sim/celox/pull/3624
* fix(sv): name every variable of a subroutine by the subroutine by @tignear in https://github.com/celox-sim/celox/pull/3627
* test(sv): run the always_ff event tests in every build by @tignear in https://github.com/celox-sim/celox/pull/3634
* chore(deps): update dependency vite to v8.3.6 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3650
* perf(sv): carry only the package subroutines a module can call by @tignear in https://github.com/celox-sim/celox/pull/3631
* fix(sv): leave casts in the items of a pattern-valued parameter to the pattern by @tignear in https://github.com/celox-sim/celox/pull/3637
* fix(sv): take static function results filled through calls as temporaries by @tignear in https://github.com/celox-sim/celox/pull/3648
* feat(sv): apply defparam assignments to the instances of a module by @tignear in https://github.com/celox-sim/celox/pull/3542
* feat(sv): refer to associative arrays, dynamic arrays and queues by hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/3543
* perf(sv): hand a scope's symbols to foreign specializations by name sets by @tignear in https://github.com/celox-sim/celox/pull/3654
* fix(sv): take the signedness of a constant wider than 64 bits from its literal by @tignear in https://github.com/celox-sim/celox/pull/3638
* feat(sv)!: set the format of %t with $timeformat by @tignear in https://github.com/celox-sim/celox/pull/3609
* perf(sv): make the subroutine tables of a module once for all its lowerings by @tignear in https://github.com/celox-sim/celox/pull/3662
* chore(deps): update rust toolchain to d1e30fd by @renovate[bot] in https://github.com/celox-sim/celox/pull/3678
* perf(sv): elaborate a scope for class specializations only when there is a class by @tignear in https://github.com/celox-sim/celox/pull/3679
* feat(sv): declare the named constants of ranged enumeration members by @tignear in https://github.com/celox-sim/celox/pull/3639
* feat(sv): complement parameters in parameter initializers by @tignear in https://github.com/celox-sim/celox/pull/3640
* feat(sv): select members and bits of packed container elements by @tignear in https://github.com/celox-sim/celox/pull/3645
* feat(sv): let ordinary always processes write one variable together by @tignear in https://github.com/celox-sim/celox/pull/3646
* perf(sv): type the subexpressions of one type query once by @tignear in https://github.com/celox-sim/celox/pull/3690
* feat(sv): evaluate $sformatf of constants as a constant expression by @tignear in https://github.com/celox-sim/celox/pull/3557
* feat(sv): take assignment patterns as new[] initializers by @tignear in https://github.com/celox-sim/celox/pull/3702
* refactor(sv): tell a virtual interface by its type by @tignear in https://github.com/celox-sim/celox/pull/3677
* feat(sv): run deferred assertions in function units by @tignear in https://github.com/celox-sim/celox/pull/3633
* fix(sv): pass enum constants to enum formals in constant functions by @tignear in https://github.com/celox-sim/celox/pull/3711
* fix(sv): name the compilation-unit classes of other files in $typename by @tignear in https://github.com/celox-sim/celox/pull/3682
* fix(sv): read the bracket expressions of UVM's regular expressions as POSIX does by @tignear in https://github.com/celox-sim/celox/pull/3653
* ci: skip scheduled benchmarks while pull request CI is backed up by @tignear in https://github.com/celox-sim/celox/pull/3726
* test: run the suite cases with scripts/run-vtest.py instead of Rust tests by @tignear in https://github.com/celox-sim/celox/pull/3725
* feat(sv): refer to variables of generate blocks by their scope names by @tignear in https://github.com/celox-sim/celox/pull/3717
* fix(sv): select declared indices in constant functions by @tignear in https://github.com/celox-sim/celox/pull/3720
* perf(sir-opt): optimize function units and processes on the unit workers by @tignear in https://github.com/celox-sim/celox/pull/3660
* fix(sv): lower the abstract classes of a compilation unit by @tignear in https://github.com/celox-sim/celox/pull/3673
* feat(sv): call UVM's DPI-C functions in function units by @tignear in https://github.com/celox-sim/celox/pull/3647
* perf(native): compile function units on the compile workers by @tignear in https://github.com/celox-sim/celox/pull/3644
* fix(sv): keep the header ports of a non-ANSI module in a design with interfaces by @tignear in https://github.com/celox-sim/celox/pull/3689
* perf(sv): share the subroutines of a scope's symbols instead of copying them by @tignear in https://github.com/celox-sim/celox/pull/3667
* perf(sv): reuse the context of the last type query of a scope by @tignear in https://github.com/celox-sim/celox/pull/3742
* fix(sv): cast call results by their width in constant functions by @tignear in https://github.com/celox-sim/celox/pull/3710
* perf(sv): type a variable shape with literal bounds once by @tignear in https://github.com/celox-sim/celox/pull/3743
* chore(deps): update dependency @playwright/test to v1.65.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3747
* chore(deps): update rust toolchain to 8bbfc85 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3759
* fix(sv): evaluate constant functions that divide by an argument by @tignear in https://github.com/celox-sim/celox/pull/3708
* fix(sv): select parameters wider than 128 bits whose value is another parameter by @tignear in https://github.com/celox-sim/celox/pull/3722
* fix(sv): evaluate the bounds of a member select in the scope by @tignear in https://github.com/celox-sim/celox/pull/3697
* fix(sv): scan into two-state members of four-state structures by @tignear in https://github.com/celox-sim/celox/pull/3709
* fix(sv): select parameters by their declared indices in generate conditions by @tignear in https://github.com/celox-sim/celox/pull/3750
* feat(sv): procedural continuous assignments of variables by @tignear in https://github.com/celox-sim/celox/pull/3565
* feat(cli): install the celox command with cargo install celox by @tignear in https://github.com/celox-sim/celox/pull/3751
* feat(sv): use the first declaration of a module declared twice, by diagnostic by @tignear in https://github.com/celox-sim/celox/pull/3641
* feat(sv): let edge-sensitive always procedures write one variable together by @tignear in https://github.com/celox-sim/celox/pull/3737
* fix(sv): take $clog2 of constants wider than 128 bits in parameter overrides by @tignear in https://github.com/celox-sim/celox/pull/3700
* fix(sv): take the compilation unit into the modules that reach its classes through a package by @tignear in https://github.com/celox-sim/celox/pull/3692
* fix(sv): take members of inline enums of generate blocks as constants by @tignear in https://github.com/celox-sim/celox/pull/3767
* fix(sv): read selects that unrolled loops take past a vector as X by @tignear in https://github.com/celox-sim/celox/pull/3666
* fix(sv): select members of elements of packed array members of structures by @tignear in https://github.com/celox-sim/celox/pull/3712
* fix(sv): select members of structures of other instances through hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/3768
* feat(sv): print packed structures with %p by @tignear in https://github.com/celox-sim/celox/pull/3756
* fix(sv): fill new[] arrays from '{default: v} initializers by @tignear in https://github.com/celox-sim/celox/pull/3772
* perf(sv): share the formal shapes, locals and classes of a scope's symbols by @tignear in https://github.com/celox-sim/celox/pull/3671
* feat(sv)!: get and set the random state of class objects by @tignear in https://github.com/celox-sim/celox/pull/3778
* feat(cli): run the celox command as veryl celox by @tignear in https://github.com/celox-sim/celox/pull/3758
* perf(sv): tell no expression is a chandle in a design that has no chandle type by @tignear in https://github.com/celox-sim/celox/pull/3688
* refactor(sv): check in debug builds that no chandle escapes the shortcut by @tignear in https://github.com/celox-sim/celox/pull/3744
* fix(sv): select elements of packed arrays whose element width calls $clog2 by @tignear in https://github.com/celox-sim/celox/pull/3545
* fix(sv): connect an interface port to a modport of an interface instance by @tignear in https://github.com/celox-sim/celox/pull/3546
* fix(sv): take only the modport outputs a module assigns as its drivers by @tignear in https://github.com/celox-sim/celox/pull/3547
* feat(sv): assign concatenations with assignment operators by @tignear in https://github.com/celox-sim/celox/pull/3698
* test(sv): expect assign_deassign to pass by @tignear in https://github.com/celox-sim/celox/pull/3791
* ci(release): attach CLI binaries to GitHub Releases by @tignear in https://github.com/celox-sim/celox/pull/3760
* refactor(sv): tell an integral type by its type by @tignear in https://github.com/celox-sim/celox/pull/3684
* refactor(sv): ask the type whether a shape holds a virtual interface by @tignear in https://github.com/celox-sim/celox/pull/3774
* feat(sv): refer to members of interface instances of other instances by hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/3771
* fix(sv): fold a division by a parameter in a typed parameter initializer by @tignear in https://github.com/celox-sim/celox/pull/3566
* fix(sv): fill packed union members of structure patterns by default by @tignear in https://github.com/celox-sim/celox/pull/3761
* feat(sv): connect ports to calls of subroutines by @tignear in https://github.com/celox-sim/celox/pull/3753
* fix(sv): type members of interface array elements by @tignear in https://github.com/celox-sim/celox/pull/3558
* feat(sv): check concurrent sequences with cycle delay ranges and or by @tignear in https://github.com/celox-sim/celox/pull/3779
* fix(sv): index unpacked arrays whose element width calls $clog2 by @tignear in https://github.com/celox-sim/celox/pull/3813
* refactor(sv): ask the type for the class of a declared shape by @tignear in https://github.com/celox-sim/celox/pull/3824
* test(sv): expect sv-tests' assign/deassign case to pass by @tignear in https://github.com/celox-sim/celox/pull/3815
* fix(sv): take negated reductions in constant expressions by @tignear in https://github.com/celox-sim/celox/pull/3754
* chore(deps): update rust toolchain to 84edd48 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3832
* feat(sv): declare tasks in interfaces by @tignear in https://github.com/celox-sim/celox/pull/3701
* feat(sv): connect output ports to members of the elements of a packed array member by @tignear in https://github.com/celox-sim/celox/pull/3792
* chore(deps): update dependency @biomejs/biome to v2.6.6 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3840
* feat(sv): stop and restart assertions with $assertoff and $asserton by @tignear in https://github.com/celox-sim/celox/pull/3567
* fix: read the x and z digits of a memory file as X and Z by @tignear in https://github.com/celox-sim/celox/pull/3568
* feat(sv): read the file of $readmemh named at run time by @tignear in https://github.com/celox-sim/celox/pull/3569
* fix(sv): select by an index that reads an element of an unpacked array by @tignear in https://github.com/celox-sim/celox/pull/3797
* feat(sv): expand let declarations where they are called by @tignear in https://github.com/celox-sim/celox/pull/3831
* feat(sv): take calls of constant functions as parameter overrides by @tignear in https://github.com/celox-sim/celox/pull/3663
* feat(sv): take array parameter overrides from calls of functions that return arrays by @tignear in https://github.com/celox-sim/celox/pull/3669
* fix(sv): size generate-local typedefs by the parameters of their block by @tignear in https://github.com/celox-sim/celox/pull/3693
* fix(sv): take container formals of interface and package subroutines in their own scope by @tignear in https://github.com/celox-sim/celox/pull/3775
* fix(sv): connect generate-local variables of parameter-dependent types to instance ports by @tignear in https://github.com/celox-sim/celox/pull/3570
* feat(sv): select members of elements of packed array members by run-time indices by @tignear in https://github.com/celox-sim/celox/pull/3773
* fix(sv): unroll counted loops over module variables by @tignear in https://github.com/celox-sim/celox/pull/3559
* feat(sv): check concurrent sequences with and and intersect by @tignear in https://github.com/celox-sim/celox/pull/3780
* feat(sv): fixed-size unpacked array class properties by @tignear in https://github.com/celox-sim/celox/pull/3706
* refactor(sv): ask the type for the class of stream and assignment-expression targets by @tignear in https://github.com/celox-sim/celox/pull/3853
* refactor(sv): tell the enumeration of an expression by its type by @tignear in https://github.com/celox-sim/celox/pull/3819
* test(sv): pin enum assignments between instances of one module by @tignear in https://github.com/celox-sim/celox/pull/3864
* feat(sv): select member elements by run-time indices through hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/3793
* refactor(sv): ask the type whether a declared shape holds an unpacked structure by @tignear in https://github.com/celox-sim/celox/pull/3857
* feat(sv): constrain the elements of random arrays by @tignear in https://github.com/celox-sim/celox/pull/3814
* refactor(sv): ask the type whether an operand is an unpacked structure by @tignear in https://github.com/celox-sim/celox/pull/3858
* refactor(sv): ask the type whether a declared shape holds dynamically sized arrays by @tignear in https://github.com/celox-sim/celox/pull/3860
* feat(sv): constrain random variables with distributions by @tignear in https://github.com/celox-sim/celox/pull/3798
* test(sv): give the symbolic queue test helper the fixed-size field by @tignear in https://github.com/celox-sim/celox/pull/3880
* fix(sv): check what an output argument copies out, of any type by @tignear in https://github.com/celox-sim/celox/pull/3865
* fix(sv): take an identifier delay value without call arguments by @tignear in https://github.com/celox-sim/celox/pull/3884
* feat(sv): constrain random variables to be unique by @tignear in https://github.com/celox-sim/celox/pull/3808
* refactor(sv): declare named and anonymous aggregate types per module analysis by @tignear in https://github.com/celox-sim/celox/pull/3867
* feat(sv): accept $psprintf as $sformatf by @tignear in https://github.com/celox-sim/celox/pull/3887
* fix(sv): read virtual interface members at their declared width outside the instances by @tignear in https://github.com/celox-sim/celox/pull/3889
* fix(sv): accept assertions in interfaces a virtual interface refers to by @tignear in https://github.com/celox-sim/celox/pull/3892
* test(sv): expect root-less sources to elaborate in the sv-tests driver's tests by @tignear in https://github.com/celox-sim/celox/pull/3898
* feat(sv): provide UVM 1.2's regular expression DPI functions by @tignear in https://github.com/celox-sim/celox/pull/3894
* feat(sv): constrain members of objects through their handles by @tignear in https://github.com/celox-sim/celox/pull/3810
* feat(sv): write members of container elements through the element by @tignear in https://github.com/celox-sim/celox/pull/3904
* feat(sv): randomize static class properties by @tignear in https://github.com/celox-sim/celox/pull/3733
* fix(sv): keep an inherited property's type from its own class's typedefs and parameters by @tignear in https://github.com/celox-sim/celox/pull/3910
* feat(sv): provide UVM 1.2's uvm_dpi_regcomp, uvm_dpi_regexec and uvm_dpi_regfree by @tignear in https://github.com/celox-sim/celox/pull/3917
* fix(sv): report an undeclared class member as an error, not unsupported by @tignear in https://github.com/celox-sim/celox/pull/3900
* feat(sv)!: solve soft constraints by their priorities by @tignear in https://github.com/celox-sim/celox/pull/3825
* fix(sv): lay a class out in the scope that declares it by @tignear in https://github.com/celox-sim/celox/pull/3919
* feat(sv): report UVM 1.2's C library errors through the UVM package by @tignear in https://github.com/celox-sim/celox/pull/3918
* feat(sv): name the static variables of subroutines through their subroutines by @tignear in https://github.com/celox-sim/celox/pull/3915
* refactor(sv): keep the semantic type a type was bound to by @tignear in https://github.com/celox-sim/celox/pull/3874
* feat(sv)!: compare class handles in constraint guards by @tignear in https://github.com/celox-sim/celox/pull/3826
* refactor(sv): keep the members of an enumeration with its declaration by @tignear in https://github.com/celox-sim/celox/pull/3876
* chore(deps): update rust toolchain to 5b87ee0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3931
* fix(sv): type the members of generate-scope enumerations with their scope's type by @tignear in https://github.com/celox-sim/celox/pull/3890
* fix(native): queue a fused flip-flop's delayed update when the host publishes it by @tignear in https://github.com/celox-sim/celox/pull/3935
* feat(sv): turn static constraint blocks on and off for every object by @tignear in https://github.com/celox-sim/celox/pull/3827
* refactor(sv): bind types to their semantic types where they are declared by @tignear in https://github.com/celox-sim/celox/pull/3891
* refactor(sv): bind subroutine interfaces and class handles where they are declared by @tignear in https://github.com/celox-sim/celox/pull/3896
* refactor(sv): bind variables and built-in types where they are made by @tignear in https://github.com/celox-sim/celox/pull/3923
* refactor(sv): keep a type's binding when its bounds take their values by @tignear in https://github.com/celox-sim/celox/pull/3936
* fix(sv): reject static formals only of functions that fork themselves by @tignear in https://github.com/celox-sim/celox/pull/3962
* refactor(sv): build a file's span index and facts with its arenas, not on first use by @tignear in https://github.com/celox-sim/celox/pull/3957
* fix(sv): build interface declarations with only the packages they name by @tignear in https://github.com/celox-sim/celox/pull/3955
* feat(sv): report a declaration initializer's fork when it runs by @tignear in https://github.com/celox-sim/celox/pull/3953
* refactor(sv): keep the locals of locator calls in the session, not behind a mutex by @tignear in https://github.com/celox-sim/celox/pull/3949
* fix(sv): report undeclared names read in package and unit subroutines by @tignear in https://github.com/celox-sim/celox/pull/3946
* feat(sv): expand randsequence productions with weighted rules by @tignear in https://github.com/celox-sim/celox/pull/3922
* fix(sv): report undeclared names read in interfaces no source references by @tignear in https://github.com/celox-sim/celox/pull/3952
* feat(sv): expand the if, case and repeat production controls of randsequence by @tignear in https://github.com/celox-sim/celox/pull/3924
* feat(sv): leave a randsequence or its production by break and return by @tignear in https://github.com/celox-sim/celox/pull/3929
* feat(sv): interleave the productions of a randsequence rand join at random by @tignear in https://github.com/celox-sim/celox/pull/3932
* feat(sv): pass values between randsequence productions by @tignear in https://github.com/celox-sim/celox/pull/3942
* test(sv): expect the randsequence if-else production cases with an undeclared name to pass by @tignear in https://github.com/celox-sim/celox/pull/3983
* fix(sv): compile a sv-tests case once for all of its simvariants by @tignear in https://github.com/celox-sim/celox/pull/3897
* ci(sv-tests): report biquad_example's missing module as an upstream input issue by @tignear in https://github.com/celox-sim/celox/pull/3987
* fix(sv): reject casts to queue types assigned to fixed-size arrays by @tignear in https://github.com/celox-sim/celox/pull/3861
* fix(sv): give a process parked in an out-of-line task the result of its own request by @tignear in https://github.com/celox-sim/celox/pull/3969
* fix(sv): accept unpacked array concatenations for fixed-size array class properties by @tignear in https://github.com/celox-sim/celox/pull/3986
* refactor(sv): publish the IR with the analysis's types by @tignear in https://github.com/celox-sim/celox/pull/3937
* feat(sv): check concurrent sequences with throughout and named sequence operands by @tignear in https://github.com/celox-sim/celox/pull/3781
* refactor(sv): fall back to a shape's metadata when its type is not formed by @tignear in https://github.com/celox-sim/celox/pull/3866
* feat(sv): sort and rsort with a with expression on elements held in records by @tignear in https://github.com/celox-sim/celox/pull/3914
* chore(lint): ban shared-mutability types outside reasoned sites by @tignear in https://github.com/celox-sim/celox/pull/3967
* feat(sv): keep randc cycles of up to 16 bits by @tignear in https://github.com/celox-sim/celox/pull/3950
* fix(sv): start the processes of a fork when their parent blocks by @tignear in https://github.com/celox-sim/celox/pull/3974
* ci: run the UVM library cases of sv-tests in shards of their own by @tignear in https://github.com/celox-sim/celox/pull/3877
* fix(sv): keep the enumeration types of members through virtual interfaces by @tignear in https://github.com/celox-sim/celox/pull/3956
* refactor(test-suite): report a verification adapter's failures through a channel by @tignear in https://github.com/celox-sim/celox/pull/3990
* refactor(sv): bind locals, subroutine interfaces and constant types where they are made by @tignear in https://github.com/celox-sim/celox/pull/3945
* refactor(backend-x86): keep the frozen spill-set keys in a local option by @tignear in https://github.com/celox-sim/celox/pull/3989
* refactor(runtime): let the file I/O manager own its file system backend by @tignear in https://github.com/celox-sim/celox/pull/3980
* refactor(sim): build a plan's combinational slicer and its caches through &mut by @tignear in https://github.com/celox-sim/celox/pull/3981
* refactor(sir-opt): keep the passes' lazy caches in local Options, not OnceCells by @tignear in https://github.com/celox-sim/celox/pull/3978
* refactor(sim): compute the state-layout tables of a simulator at build by @tignear in https://github.com/celox-sim/celox/pull/3996
* refactor(interp): make a small register's SIR value when asked, not in a OnceCell by @tignear in https://github.com/celox-sim/celox/pull/3993
* test(sv): expect 16.7 throughout and 18.14 random stability UVM cases to pass by @tignear in https://github.com/celox-sim/celox/pull/4009
* refactor(native)!: compute block live sets when asked, not in OnceLock caches by @tignear in https://github.com/celox-sim/celox/pull/3976
* feat(sv)!: solve scope randomize calls with inline constraints by @tignear in https://github.com/celox-sim/celox/pull/3829
* refactor(sv): keep a variable shape's binding as a plain field, not behind a mutex by @tignear in https://github.com/celox-sim/celox/pull/3951
* fix(sv): keep arrays written before a fork out of the fork's boxes by @tignear in https://github.com/celox-sim/celox/pull/3973
* refactor(sv): a container carries the semantic type its type was bound to by @tignear in https://github.com/celox-sim/celox/pull/3954
* feat(sv): dynamic array, queue and associative array members of unpacked structures by @tignear in https://github.com/celox-sim/celox/pull/3909
* feat(sv)!: call functions in constraints in their own phase by @tignear in https://github.com/celox-sim/celox/pull/3835
* fix(sv): run the casts and updates of immediate assertion expressions by @tignear in https://github.com/celox-sim/celox/pull/3977
* refactor(sv): compare unpacked array arguments by their semantic types by @tignear in https://github.com/celox-sim/celox/pull/3959
* refactor(sv): tell a procedural assign target's type by its semantic type by @tignear in https://github.com/celox-sim/celox/pull/3960
* feat(sv): disable iff of concurrent sequences over cycles by @tignear in https://github.com/celox-sim/celox/pull/3812
* refactor(sv): tell virtual interface types by their semantic types by @tignear in https://github.com/celox-sim/celox/pull/3982
* feat(sv)!: resolve inline constraint names in the caller and in restricted blocks by @tignear in https://github.com/celox-sim/celox/pull/3856
* refactor(sv): tell class handle types by their semantic types by @tignear in https://github.com/celox-sim/celox/pull/3995
* refactor(sv): tell enumeration types by their semantic types by @tignear in https://github.com/celox-sim/celox/pull/3997
* feat(sv): constrain bit-selects and part-selects of properties by @tignear in https://github.com/celox-sim/celox/pull/3905
* feat(sv): check multiclocked sequences joined by ##1 or ##0 by @tignear in https://github.com/celox-sim/celox/pull/3816
* feat(sv)!: accept solve before in constraint blocks by @tignear in https://github.com/celox-sim/celox/pull/3906
* test(sv): ignore the constrained selects case on wasm by @tignear in https://github.com/celox-sim/celox/pull/4037
* chore(deps): update rust toolchain to 67f1944 by @renovate[bot] in https://github.com/celox-sim/celox/pull/4035
* refactor(sv): tell unpacked structure types by their semantic types by @tignear in https://github.com/celox-sim/celox/pull/4033
* refactor(sv): tell types by their semantic types where readers had no table by @tignear in https://github.com/celox-sim/celox/pull/4044
* fix(sv): leave the parentheses after a delay value or event name to the controlled expression by @tignear in https://github.com/celox-sim/celox/pull/4043
* test(sv): expect the sv-tests multiclocked UVM sequence to pass by @tignear in https://github.com/celox-sim/celox/pull/4042
* feat(sv)!: randomize 4-state class properties by @tignear in https://github.com/celox-sim/celox/pull/3907
* feat(sv)!: constrain array elements at constant positions, sizes first by @tignear in https://github.com/celox-sim/celox/pull/3908
* test(sv): ignore the solve before case on wasm by @tignear in https://github.com/celox-sim/celox/pull/4049
* feat(sv)!: name each design element that takes the default time scale by @tignear in https://github.com/celox-sim/celox/pull/4047
* feat(sv): solve soft distributions with their weights by @tignear in https://github.com/celox-sim/celox/pull/3941
* refactor(sv): read unpacked structure declarations through their semantic types by @tignear in https://github.com/celox-sim/celox/pull/4048
* feat(sv): name the randomized object through its caller variable in inline constraints by @tignear in https://github.com/celox-sim/celox/pull/3958
* feat(sv): simulate several top-level modules together by @tignear in https://github.com/celox-sim/celox/pull/4050
* feat(sv): call interface tasks and functions through virtual interfaces by @tignear in https://github.com/celox-sim/celox/pull/4053
* fix(sv): tell anonymous enumerations apart by their declarations by @tignear in https://github.com/celox-sim/celox/pull/3999
* feat(sv): take 4-state caller variables as inputs of inline constraints by @tignear in https://github.com/celox-sim/celox/pull/3961
* fix(sv): spell `$typename` from semantic types by @tignear in https://github.com/celox-sim/celox/pull/4066
* refactor(sv): name classes and interfaces of semantic types by their declarations by @tignear in https://github.com/celox-sim/celox/pull/4068
* feat(sv): run every top-level module from the command line and sv-tests by @tignear in https://github.com/celox-sim/celox/pull/4061
* feat(sv): take selects of members through handle paths in constraints by @tignear in https://github.com/celox-sim/celox/pull/3988
* feat(sv): reach class handle members of interfaces through virtual interfaces by @tignear in https://github.com/celox-sim/celox/pull/4065
* test(sv): expect the sv-tests UVM many-to-one example to pass by @tignear in https://github.com/celox-sim/celox/pull/4077
* feat(sv): take elements of caller arrays as inputs of inline constraints by @tignear in https://github.com/celox-sim/celox/pull/4076
* feat(sv): reach the callees of specializations for other packages' classes by @tignear in https://github.com/celox-sim/celox/pull/4073
* feat(sv): order fixed-size array variables with their ordering methods by @tignear in https://github.com/celox-sim/celox/pull/4078
* feat(sv): let function units store into targets of nonblocking assignments by @tignear in https://github.com/celox-sim/celox/pull/4081
* feat(sv): randomize inline in modules that bind an interface of the package by @tignear in https://github.com/celox-sim/celox/pull/4085
* refactor(sv): keep the semantic type of a type's elements across changes of its unpacked dimensions by @tignear in https://github.com/celox-sim/celox/pull/4086
* refactor(sv): let a body builder hold the context it analyzes in by @tignear in https://github.com/celox-sim/celox/pull/4088
* test(sv): expect the sv-tests UVM generation_seq examples to pass by @tignear in https://github.com/celox-sim/celox/pull/4087
* refactor(sv)!: tell virtual interface types by their semantic types alone by @tignear in https://github.com/celox-sim/celox/pull/4098
* feat(sv): call the overrides of module classes through handles from any scope by @tignear in https://github.com/celox-sim/celox/pull/4074
* ci(deps): Bump Mergifyio/setup-cli from 1.4.0 to 1.5.0 by @dependabot[bot] in https://github.com/celox-sim/celox/pull/4094
* ci(deps): Bump peter-evans/create-pull-request from 8.1.1 to 8.1.3 by @dependabot[bot] in https://github.com/celox-sim/celox/pull/4097
* feat(sv): specialize classes in package typedefs by the package's constants by @tignear in https://github.com/celox-sim/celox/pull/4102
* test(sv): expect the sv-tests UVM config_db examples of module class overrides to pass by @tignear in https://github.com/celox-sim/celox/pull/4101
* ci(deps): Bump cachix/install-nix-action from 31.11.3 to 31.11.5 by @dependabot[bot] in https://github.com/celox-sim/celox/pull/4095
* feat(sv): import packages in procedural blocks and subroutine bodies by @tignear in https://github.com/celox-sim/celox/pull/4093
* feat(sv): run combinational loops whose condition depends on values by @tignear in https://github.com/celox-sim/celox/pull/3656
* feat(sv): wait through virtual interfaces that class properties hold by @tignear in https://github.com/celox-sim/celox/pull/4107
* fix(sv): take set membership tests of constants as constants by @tignear in https://github.com/celox-sim/celox/pull/3695
* fix(sv): assign enumeration members of interfaces through generic ports by @tignear in https://github.com/celox-sim/celox/pull/4111
* fix(sv): reject the covergroups of classes where they are used by @tignear in https://github.com/celox-sim/celox/pull/4113
* feat(sv)!: select bits of packed structure members at run time by @tignear in https://github.com/celox-sim/celox/pull/3560
* feat(sv): procedural force and release of whole variables and nets by @tignear in https://github.com/celox-sim/celox/pull/3762
* feat(sv): call functions in concurrent assertion operands by @tignear in https://github.com/celox-sim/celox/pull/3821
* fix(sv): let continuous assignments read other bits of their net by @tignear in https://github.com/celox-sim/celox/pull/3757
* fix(sv): declare no package item for out-of-block method definitions by @tignear in https://github.com/celox-sim/celox/pull/4114
* feat(playground): support SystemVerilog sources and add SV examples by @tignear in https://github.com/celox-sim/celox/pull/4119
* refactor(sv)!: bind class handle types where they are made by @tignear in https://github.com/celox-sim/celox/pull/4123
* fix(sv): complete non-ANSI subroutine formals with variable declarations by @tignear in https://github.com/celox-sim/celox/pull/4122
* fix(sv): give unsized decimal literals wider than 32 bits a sign bit by @tignear in https://github.com/celox-sim/celox/pull/4124
* fix(sv): keep the lines of skipped conditional branches in preprocessed text by @tignear in https://github.com/celox-sim/celox/pull/4130
* fix(sv): take elements of array parameters as elaboration constants by @tignear in https://github.com/celox-sim/celox/pull/3572
* fix(sv): honor begin_keywords versions wherever an identifier appears by @tignear in https://github.com/celox-sim/celox/pull/4128
* fix(sv): type parameters with a range or a signing but no type as declared by @tignear in https://github.com/celox-sim/celox/pull/4132
* fix(sv): initialize variable output ports with their initializers by @tignear in https://github.com/celox-sim/celox/pull/4133
* fix(sv): write to and close MCD 0 as an empty set of channels by @tignear in https://github.com/celox-sim/celox/pull/4136
* chore(deps): update dependency vite to v8.3.7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/4146
* fix(sv): define the outputs of latched always_comb processes bit by bit by @tignear in https://github.com/celox-sim/celox/pull/4135
* feat(sv): lower buf and not gate primitives by @tignear in https://github.com/celox-sim/celox/pull/4147
* fix(sv): read instance members in select bounds of virtual interface code by @tignear in https://github.com/celox-sim/celox/pull/4154
* refactor(hotspot): count a unit's register uses in a cache the JIT worker owns by @tignear in https://github.com/celox-sim/celox/pull/3991
* feat(sv): combinational user-defined primitives by @tignear in https://github.com/celox-sim/celox/pull/4157
* feat(sv): take an array parameter's value from a package's array parameter by @tignear in https://github.com/celox-sim/celox/pull/3575
* feat(sv): connect ports to hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/3685
* refactor(cli): tell an installed SIGUSR1 registration by the installed action by @tignear in https://github.com/celox-sim/celox/pull/3984
* fix(sv): dump nothing for an instance without variables at the dumped levels by @tignear in https://github.com/celox-sim/celox/pull/4162
* fix(sv): check the types of non-ANSI port redeclarations by @tignear in https://github.com/celox-sim/celox/pull/4125
* fix(deps): update rust crate object to 0.41 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3801
* chore(deps): update rust crate syn to v3.0.10 by @renovate[bot] in https://github.com/celox-sim/celox/pull/3651
* feat!: read memory files when $readmemh and $readmemb execute by @tignear in https://github.com/celox-sim/celox/pull/3649
* fix(sv): keep the unknown bits of constant selects of parameter values by @tignear in https://github.com/celox-sim/celox/pull/4168
* fix(sv): take elements of array parameters given by enumeration members by @tignear in https://github.com/celox-sim/celox/pull/3652
* fix(sv): reject enumerations of non-integer bases and repeated values by @tignear in https://github.com/celox-sim/celox/pull/4148
* chore(deps): update rust crate libloading to v0.9.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/4172
* feat(sv): monitor selects and expressions by the changes of their values by @tignear in https://github.com/celox-sim/celox/pull/4159
* fix(sv): round real repeat counts to integers by @tignear in https://github.com/celox-sim/celox/pull/4137
* feat(sv): sequential user-defined primitives by @tignear in https://github.com/celox-sim/celox/pull/4167
* feat(sv): connect explicitly named ports and non-ANSI port expressions by @tignear in https://github.com/celox-sim/celox/pull/4141
* fix(sv): reject values that are no unpacked array given to one by @tignear in https://github.com/celox-sim/celox/pull/4158
* fix(sv): read elements of unpacked real arrays as reals by @tignear in https://github.com/celox-sim/celox/pull/4182
* fix(sv): size parameters by members of the parameters before them by @tignear in https://github.com/celox-sim/celox/pull/3670
* fix(sv): leave parameters that name parameters not known yet to the later pass by @tignear in https://github.com/celox-sim/celox/pull/3683
* refactor(sv)!: tell class handle types by their semantic types alone by @tignear in https://github.com/celox-sim/celox/pull/4181
* feat(sv): resolve upward names from interface instance bodies by @tignear in https://github.com/celox-sim/celox/pull/4140
* fix(sv): reject end labels that differ from the name of their block by @tignear in https://github.com/celox-sim/celox/pull/4190
* fix(sv): reject items the grammar does not admit where they are written by @tignear in https://github.com/celox-sim/celox/pull/4193
* fix(sv): reject declarations whose names, lifetimes or returns are illegal by @tignear in https://github.com/celox-sim/celox/pull/4194
* feat(sv): lower arrays of gate instances by @tignear in https://github.com/celox-sim/celox/pull/4179
* feat(sv): select elements of packed structure members of several dimensions at run time by @tignear in https://github.com/celox-sim/celox/pull/3686
* fix(sv): evaluate typed parameter values at the width of their type by @tignear in https://github.com/celox-sim/celox/pull/4197
* feat(sv): take members of the elements of array parameters as elaboration constants by @tignear in https://github.com/celox-sim/celox/pull/3664
* feat(sv): take values the caller computes as inputs of inline constraints by @tignear in https://github.com/celox-sim/celox/pull/4138
* fix(sv): reject real operands of integral operators and string nets by @tignear in https://github.com/celox-sim/celox/pull/4195
* fix(sv): reject variables read where constant expressions are required by @tignear in https://github.com/celox-sim/celox/pull/4188
* feat(sv)!: warn on recoverable diagnostics by default, with opt-in strict mode by @tignear in https://github.com/celox-sim/celox/pull/4156
* docs(sv): drop the latch entry from intentional deviations by @tignear in https://github.com/celox-sim/celox/pull/4209
* chore(deps): update pnpm to v12.19.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/4211
* feat(sv): add [compatibility] opt-ins for legacy empty reductions and shared always temporaries by @tignear in https://github.com/celox-sim/celox/pull/4213
* perf(frontend): release module units once every instance holds its relocated copy by @tignear in https://github.com/celox-sim/celox/pull/3881
* fix(sv): cast members of enumerations wider than 128 bits in constant expressions by @tignear in https://github.com/celox-sim/celox/pull/3672
* feat(sv): load associative arrays with $readmemh and $readmemb by @tignear in https://github.com/celox-sim/celox/pull/3544
* feat(sv): call writing interface functions from generate, functions and tasks by @tignear in https://github.com/celox-sim/celox/pull/4216
* feat(sv): warn on an initialized static variable without the static keyword by @tignear in https://github.com/celox-sim/celox/pull/4215
* fix(sv): evaluate a repeat count once by @tignear in https://github.com/celox-sim/celox/pull/3925
* feat(sv): implement generic interconnect nets by @tignear in https://github.com/celox-sim/celox/pull/4224
* perf(sv): release the lowering before scheduling when no level list can be demoted by @tignear in https://github.com/celox-sim/celox/pull/3882
* perf(sv): glue the lowered modules themselves when no level list can be demoted by @tignear in https://github.com/celox-sim/celox/pull/3883
* perf(sv): release the analyses before lowering the package states by @tignear in https://github.com/celox-sim/celox/pull/3886
* fix(sv): select an element of a packed parameter wider than 128 bits by @tignear in https://github.com/celox-sim/celox/pull/3674
* test(sv): expect the sv-tests UVM slave_agent examples to pass by @tignear in https://github.com/celox-sim/celox/pull/4232
* feat(sv): select bits of members of array elements at run time by @tignear in https://github.com/celox-sim/celox/pull/3691
* feat(sv): hold the released targets of always_comb, always_latch and always @* by @tignear in https://github.com/celox-sim/celox/pull/3765
* chore(deps): update dependency vite to v8.3.8 by @renovate[bot] in https://github.com/celox-sim/celox/pull/4236
* fix(sv): take the bits a select indexed by a loop variable writes as its process's driver by @tignear in https://github.com/celox-sim/celox/pull/4205
* feat(sv): take constant values of up to 1048576 bits by @tignear in https://github.com/celox-sim/celox/pull/3675
* feat(sv): procedural force and release of constant selects and members by @tignear in https://github.com/celox-sim/celox/pull/3766
* fix(sv): take the bits a run-time select of a structure member drives as its driver by @tignear in https://github.com/celox-sim/celox/pull/4241
* feat(sv): procedural force and release through hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/3769
* fix(sv): size part-selects and casts by elements of array parameters by @tignear in https://github.com/celox-sim/celox/pull/3694
* feat(sv): select bits of elements of array class properties by @tignear in https://github.com/celox-sim/celox/pull/4246
* fix(sv): lower ** in port actuals, such as run-time selects of **-bounded packed arrays by @tignear in https://github.com/celox-sim/celox/pull/4247
* fix(sv): compare the items of a case statement on a string as strings by @tignear in https://github.com/celox-sim/celox/pull/4245
* feat(sv): procedural force and release in tasks and of random values by @tignear in https://github.com/celox-sim/celox/pull/3802
* fix(native): keep the low bits when a copy narrows a register by @tignear in https://github.com/celox-sim/celox/pull/3687
* perf(sv): find what the writes before an observer reach in linear time by @tignear in https://github.com/celox-sim/celox/pull/3704
* fix(sv)!: keep run-time part-selects within their array element by @tignear in https://github.com/celox-sim/celox/pull/3703
* feat(sv): expand loop generate constructs to a million iterations per module by @tignear in https://github.com/celox-sim/celox/pull/3705
* fix(sv): compare the members of an inside set on strings as strings by @tignear in https://github.com/celox-sim/celox/pull/4255
* feat(sv): procedural force and release of elements of unpacked arrays by @tignear in https://github.com/celox-sim/celox/pull/3803
* fix(backends): clear the bits above the width of multi-word arithmetic results by @tignear in https://github.com/celox-sim/celox/pull/4258
* feat(sv): call the subroutines of other instances by hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/3732
* refactor(sv): keep the enumerated type a type is as its declaration by @tignear in https://github.com/celox-sim/celox/pull/4231
* fix(sv): zero-extend unsigned values into parameters wider than 128 bits by @tignear in https://github.com/celox-sim/celox/pull/3564
* feat(sv): virtual interfaces of parameterized interfaces by @tignear in https://github.com/celox-sim/celox/pull/4237
* perf(sv): derive the query context of an operand from the names it reads by @tignear in https://github.com/celox-sim/celox/pull/3788
* feat(sv): declare implicit nets in port connections by @tignear in https://github.com/celox-sim/celox/pull/4253
* feat(sv): take members of structure parameters wider than 128 bits as constants by @tignear in https://github.com/celox-sim/celox/pull/3668
* feat(sv): implications whose consequents have several ways to match by @tignear in https://github.com/celox-sim/celox/pull/4268
* ci(deps): Bump github/codeql-action/analyze from 4.39.1 to 4.39.3 by @dependabot[bot] in https://github.com/celox-sim/celox/pull/3424
* test(sv): reduce the rows of a transposed array that a bit copy takes by @tignear in https://github.com/celox-sim/celox/pull/4274
* chore(deps): ignore RUSTSEC-2026-0543 (chrono unmaintained) by @tignear in https://github.com/celox-sim/celox/pull/4272
* fix(sv): select members of structure parameters wider than 128 bits in generate conditions by @tignear in https://github.com/celox-sim/celox/pull/4275
* feat(sv): assign variables of other instances continuously through hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/4278
* feat(sv): run an always_ff whose event is an edge of an expression by @tignear in https://github.com/celox-sim/celox/pull/3573
* fix(sv): take array parameters as arguments of constant functions in parameter values by @tignear in https://github.com/celox-sim/celox/pull/4282
* feat(sv): upward names in interface tasks called through virtual interfaces by @tignear in https://github.com/celox-sim/celox/pull/4281
* feat(sv): run the nonblocking assignments of always @* procedures by @tignear in https://github.com/celox-sim/celox/pull/4285
* fix(sv): take parameters of other packages in the first parameter pass by @tignear in https://github.com/celox-sim/celox/pull/4288
* fix(sir): keep the Muxes that coupled-store regions store by @tignear in https://github.com/celox-sim/celox/pull/4289
* fix(sv): take values of package parameters wider than 128 bits in port actuals by @tignear in https://github.com/celox-sim/celox/pull/4290
* feat(sv): members of elements of arrays of other instances by hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/4287
* refactor(sv): read the enumerated type of a type from its declaration by @tignear in https://github.com/celox-sim/celox/pull/4295
* fix(sv): leave the operands that a known operand does not evaluate unread by @tignear in https://github.com/celox-sim/celox/pull/4299
* feat(sv): take unpacked array parameters of generate blocks by @tignear in https://github.com/celox-sim/celox/pull/4297
* fix(sv): publish package parameters of array-argument calls with the arrays' values by @tignear in https://github.com/celox-sim/celox/pull/4308
* feat(sv): DPI-C context function imports by @tignear in https://github.com/celox-sim/celox/pull/4305
* feat(sv)!: parameters of other instances by hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/4301
* fix(sv): run clocked processes whose only effect is a heap call by @tignear in https://github.com/celox-sim/celox/pull/4302
* fix(sim): clock a design whose clock only wakes a waiting process by @tignear in https://github.com/celox-sim/celox/pull/4303
* feat(sv)!: parse covergroup declarations into typed nodes by @tignear in https://github.com/celox-sim/celox/pull/4309
* chore(deps): update rust crate rand to v0.10.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/4312
* feat(sv): call user DPI-C functions from out-of-line functions by @tignear in https://github.com/celox-sim/celox/pull/4316
* feat(sv): procedural force and release of members of interface instances through hierarchical names by @tignear in https://github.com/celox-sim/celox/pull/4314
* refactor(sv): tell enumerated types by their declarations alone by @tignear in https://github.com/celox-sim/celox/pull/4320
* perf(heap): keep container stores to the elements they store by @tignear in https://github.com/celox-sim/celox/pull/4319
* perf(sv): find whether a loop's callees write its variable once per module by @tignear in https://github.com/celox-sim/celox/pull/4307
* fix(sv): plan the interfaces with ports bound to generic interface ports by @tignear in https://github.com/celox-sim/celox/pull/4323
* feat(sv): trigger named events of interfaces through virtual interfaces by @tignear in https://github.com/celox-sim/celox/pull/4326
* fix(runtime): share the description of a forked process and reuse ended activations without a scan by @tignear in https://github.com/celox-sim/celox/pull/4271
* perf(sv): resolve genvar-indexed hierarchical names before the analysis by @tignear in https://github.com/celox-sim/celox/pull/4329
* fix(sv): name generate-block instances in the virtual interface value rejection by @tignear in https://github.com/celox-sim/celox/pull/4331
* fix(sv): flush the deferred assertion reports of processes $exit kills by @tignear in https://github.com/celox-sim/celox/pull/4336
* test(sv): easyUVM passes in both state modes by @tignear in https://github.com/celox-sim/celox/pull/4337
* fix(sv): let nested declarations shadow interface items by @tignear in https://github.com/celox-sim/celox/pull/4325
* perf(scheduler): skip unchanged settles with comb captures or opaque calls by @tignear in https://github.com/celox-sim/celox/pull/4335
* perf(sv): instrument a write only for the event probes whose bits it can change by @tignear in https://github.com/celox-sim/celox/pull/4344
* perf(scheduler): serve file opens, closes, flushes and plusargs natively by @tignear in https://github.com/celox-sim/celox/pull/4342
* test(sv): expect the sv-tests UVM register_mem example to pass by @tignear in https://github.com/celox-sim/celox/pull/4351
* refactor(sv): keep the unpacked structure type a type is as its declaration by @tignear in https://github.com/celox-sim/celox/pull/4353
* feat(sv): wait on named events of interfaces through virtual interfaces by @tignear in https://github.com/celox-sim/celox/pull/4352
* fix(sv): dispatch to the classes an interface instance sees in the scope that binds it by @tignear in https://github.com/celox-sim/celox/pull/4357
* chore(deps): update dependency @biomejs/biome to v2.6.7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/4360
* refactor(sv): read the unpacked structure type of a type from its declaration by @tignear in https://github.com/celox-sim/celox/pull/4363
* perf(native): call the larger bodies of the compiled scheduler as functions of their own by @tignear in https://github.com/celox-sim/celox/pull/4364
* perf(sv): evaluate the bounds of a run-time select with the names they read by @tignear in https://github.com/celox-sim/celox/pull/4367
* perf(scheduler): skip unchanged settles that call noncontext DPI-C imports by @tignear in https://github.com/celox-sim/celox/pull/4365
* perf(slt): partition settles that hold combinational captures by @tignear in https://github.com/celox-sim/celox/pull/4345
* chore(deps): update pnpm to v12.20.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/4372
* perf(sv): keep the constant types of a scope in a persistent table by @tignear in https://github.com/celox-sim/celox/pull/4375
* fix(sv): stop at a virtual method call that no override serves by @tignear in https://github.com/celox-sim/celox/pull/4362
* refactor(sv): tell unpacked structure types by their declarations alone by @tignear in https://github.com/celox-sim/celox/pull/4373
* fix(sv): tell nets of a package only by its own declarations by @tignear in https://github.com/celox-sim/celox/pull/4382
* perf(scheduler): run many slots per compiled run with deferred assertions or heap handles by @tignear in https://github.com/celox-sim/celox/pull/4378
* perf(sim): find the settle's retained storage without copying it per block by @tignear in https://github.com/celox-sim/celox/pull/4383
* fix(sv): name named blocks and generate blocks in %m by @tignear in https://github.com/celox-sim/celox/pull/4370
* perf(sim): share one domain among the clock copies sensed at the other edge by @tignear in https://github.com/celox-sim/celox/pull/4377
* perf(sv): select small run-time indices of wide vectors by their values by @tignear in https://github.com/celox-sim/celox/pull/4386
* fix(sv): bind wildcard imports only for the references after them by @tignear in https://github.com/celox-sim/celox/pull/4349
* refactor(sir): move the SIR interpreter into celox-sir by @tignear in https://github.com/celox-sim/celox/pull/4394
* perf(sv): publish memories written bit by bit at run-time indices sparsely by @tignear in https://github.com/celox-sim/celox/pull/4397
* docs(sv): organize partial support into tables and clarify strict mode by @tignear in https://github.com/celox-sim/celox/pull/4401
* perf(sv): keep the packed dimensions of a scope's parameters as they are declared by @tignear in https://github.com/celox-sim/celox/pull/4387
* perf(sv): enumerate selects decided by a slice of a run-time variable by @tignear in https://github.com/celox-sim/celox/pull/4403
* perf(native): call the settle's own code from the compiled scheduler by @tignear in https://github.com/celox-sim/celox/pull/4391
* perf(sim): compare a sparse memory around its commit only when it was written by @tignear in https://github.com/celox-sim/celox/pull/4396
* perf(sv): lower always_ff to a next-state form with memory write ports, as a report by @tignear in https://github.com/celox-sim/celox/pull/4395
* fix(sv): fold logical operators whose known right operand decides them by @tignear in https://github.com/celox-sim/celox/pull/4409
* perf(native): keep 64-bit addressing in shortened absolute state moves by @tignear in https://github.com/celox-sim/celox/pull/4410
* perf(sim)!: build the next-state DAG of each clock domain behind next_state_dag by @tignear in https://github.com/celox-sim/celox/pull/4399
* fix(sir-opt): fold an XOR chain over all bits of a wide source by @tignear in https://github.com/celox-sim/celox/pull/4412
* perf(sv): take a run-time array element's index from its position's terms by @tignear in https://github.com/celox-sim/celox/pull/4418
* perf(slt): lower a two-state slice to one Slice instruction by @tignear in https://github.com/celox-sim/celox/pull/4406
* fix(sir): name the root of an alias chain in every redirected access by @tignear in https://github.com/celox-sim/celox/pull/4425
* perf(scheduler): settle before NBA only for observed blocking writes of containers by @tignear in https://github.com/celox-sim/celox/pull/4426
* perf(scheduler): format $sformatf requests natively by @tignear in https://github.com/celox-sim/celox/pull/4429
* perf(sim): build the observations of every NBA target in one pass by @tignear in https://github.com/celox-sim/celox/pull/4415
* perf(scheduler): read $fread into integral variables natively by @tignear in https://github.com/celox-sim/celox/pull/4431
* perf(sv): tell a literal's unknown bits without parsing it when it has no X or Z digit by @tignear in https://github.com/celox-sim/celox/pull/4433
* perf(sv): propagate bit boundaries with a worklist by @tignear in https://github.com/celox-sim/celox/pull/4435
* perf(slt): share the guarded concat analyses of one lowered cone by @tignear in https://github.com/celox-sim/celox/pull/4438
* perf(slt): slice a shared node of one word once instead of each part of it by @tignear in https://github.com/celox-sim/celox/pull/4421
* perf(slt): lower each step of a shared "found" chain from the step before by @tignear in https://github.com/celox-sim/celox/pull/4437
* perf(sv): cut combinational units at an instruction budget by @tignear in https://github.com/celox-sim/celox/pull/4424
* fix(sv)!: run converted always @* processes once at time zero in two-state mode by @tignear in https://github.com/celox-sim/celox/pull/4442
* fix(sv): keep disjoint FF array element writes out of sparse next state by @tignear in https://github.com/celox-sim/celox/pull/4445
* chore(deps): update rust crate clap to v4.6.8 by @renovate[bot] in https://github.com/celox-sim/celox/pull/4449
* fix(deps): update wasmtime to 49.0.6 and 50.0.2 for RUSTSEC-2026-0566..0573 by @tignear in https://github.com/celox-sim/celox/pull/4417

## New Contributors
* @dependabot[bot] made their first contribution in https://github.com/celox-sim/celox/pull/3061

**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.13.0...v0.14.0

## 0.13.0 (2026-11-02)

## What's Changed
* fix(sv): keep class type parameters out of package dependencies by @tignear in https://github.com/celox-sim/celox/pull/2292
* fix(sv): resolve class typedef qualifiers in their lexical scopes by @tignear in https://github.com/celox-sim/celox/pull/2293
* ci: repair crate publication from immutable source tags by @tignear in https://github.com/celox-sim/celox/pull/2300
* fix(sv): preserve imported class member types by @tignear in https://github.com/celox-sim/celox/pull/2306
* fix(vpi): defer inertial deposits before enabling native cocotb clocks by @tignear in https://github.com/celox-sim/celox/pull/2307
* perf(vpi): reuse selected hosts and callback signal metadata by @tignear in https://github.com/celox-sim/celox/pull/2308
* ci: repair normalized archive checks in the crate publisher by @tignear in https://github.com/celox-sim/celox/pull/2309
* fix(veryl): recognize exhaustive binary cases in latch diagnostics by @tignear in https://github.com/celox-sim/celox/pull/2312
* fix(veryl): infer testbench reset polarity from connected declarations by @tignear in https://github.com/celox-sim/celox/pull/2313
* feat(benchmarks): add representative circuit workloads and publication by @tignear in https://github.com/celox-sim/celox/pull/2314
* fix(veryl): mask function-local static partial writes before merging by @tignear in https://github.com/celox-sim/celox/pull/2317
* test(xezim): update xezim digest to 270b39d by @renovate[bot] in https://github.com/celox-sim/celox/pull/2319
* fix(veryl): bound compiler stack usage for deep decoder trees by @tignear in https://github.com/celox-sim/celox/pull/2318
* chore(deps): update actions/download-artifact action to v8 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2327
* chore(deps): update dependency ubuntu to v26 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2328
* feat(benchmarks): run rica kernels with Veryl native testbenches by @tignear in https://github.com/celox-sim/celox/pull/2321
* feat(benchmarks): compare Celox and Veryl tiered execution by @tignear in https://github.com/celox-sim/celox/pull/2323
* feat(sv)!: judge iff-qualified event controls in processes by @tignear in https://github.com/celox-sim/celox/pull/1829
* perf(vpi): reduce callback comparison and host lookup overhead by @tignear in https://github.com/celox-sim/celox/pull/2333
* perf(vpi): avoid redundant untimed value-change polling by @tignear in https://github.com/celox-sim/celox/pull/2334
* perf(vpi): reuse value buffers and stream packed digits by @tignear in https://github.com/celox-sim/celox/pull/2335
* perf(vpi): read packed native scalars without wide conversion by @tignear in https://github.com/celox-sim/celox/pull/2336
* perf(vpi): retain the selected host through deposit settlement by @tignear in https://github.com/celox-sim/celox/pull/2337
* perf(vpi): decode narrow deposits without wide arithmetic by @tignear in https://github.com/celox-sim/celox/pull/2339
* perf(vpi): reuse scalar metadata during reads and edge replay by @tignear in https://github.com/celox-sim/celox/pull/2340
* chore(deps): update dependency node to v26 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2348
* fix(sir): truncate store operands before coalescing by @tignear in https://github.com/celox-sim/celox/pull/2351
* perf(vpi): handle packed values up to 128 bits in native words by @tignear in https://github.com/celox-sim/celox/pull/2341
* perf(vpi): store packed known values without temporary byte vectors by @tignear in https://github.com/celox-sim/celox/pull/2342
* perf(vpi): avoid redundant work in ordinary signal deposits by @tignear in https://github.com/celox-sim/celox/pull/2343
* perf(vpi): load packed scalars with matching native word sizes by @tignear in https://github.com/celox-sim/celox/pull/2344
* perf(cli): discover cocotb runtime configuration in one interpreter by @tignear in https://github.com/celox-sim/celox/pull/2345
* fix(sir): preserve snapshots across canonical writes by @tignear in https://github.com/celox-sim/celox/pull/2352
* perf(veryl): avoid redundant work in static loops by @tignear in https://github.com/celox-sim/celox/pull/2353
* perf(sir): avoid unused CFG facts and duplicate propagation by @tignear in https://github.com/celox-sim/celox/pull/2354
* perf(testbench): avoid allocations for small expression stacks by @tignear in https://github.com/celox-sim/celox/pull/2355
* perf(testbench): avoid redundant RTL settling and clock dispatch by @tignear in https://github.com/celox-sim/celox/pull/2356
* perf(runtime): reuse coincident-edge execution plans by @tignear in https://github.com/celox-sim/celox/pull/2357
* perf(native): read attached images without copying the runtime prefix by @tignear in https://github.com/celox-sim/celox/pull/2360
* perf(vpi): retain narrow callback samples in native words by @tignear in https://github.com/celox-sim/celox/pull/2374
* fix(sv): bind class alias defaults after enclosing parameters by @tignear in https://github.com/celox-sim/celox/pull/2365
* fix(sv): defer class type arguments bounded by package constants by @tignear in https://github.com/celox-sim/celox/pull/2366
* feat(sv): accept named enumeration and struct class type arguments by @tignear in https://github.com/celox-sim/celox/pull/2377
* test(xezim): update xezim digest to d491f94 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2347
* perf(x86): use popcnt for wide xor reductions by @tignear in https://github.com/celox-sim/celox/pull/2384
* chore(deps): update dependency vite to v8.3.5 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2401
* perf(testbench): reuse async loop futures and clock batches by @tignear in https://github.com/celox-sim/celox/pull/2378
* perf(runtime): defer edge plan preparation until a cache miss by @tignear in https://github.com/celox-sim/celox/pull/2399
* perf(tiered): compile derived edge kernels on demand by @tignear in https://github.com/celox-sim/celox/pull/2400
* feat(sv): support 4-state class properties by @tignear in https://github.com/celox-sim/celox/pull/2381
* perf(vpi): encode native value words without temporary big integers by @tignear in https://github.com/celox-sim/celox/pull/2409
* perf(vpi): reuse inactive callback request records by @tignear in https://github.com/celox-sim/celox/pull/2410
* chore(deps): update rust toolchain to 19ecdd0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2413
* feat(sv)!: execute clocked Boolean concurrent assertions by @tignear in https://github.com/celox-sim/celox/pull/2408
* feat(sv): implement built-in process handles and control by @tignear in https://github.com/celox-sim/celox/pull/2322
* fix(native): store the mask plane of wide two-state values by @tignear in https://github.com/celox-sim/celox/pull/2425
* ci: start the fixer agent from PR mentions by @tignear in https://github.com/celox-sim/celox/pull/2430
* perf(sv): bind class signatures once per scope during type discovery by @tignear in https://github.com/celox-sim/celox/pull/2421
* chore(deps): update dependency @biomejs/biome to v2.6.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2428
* chore(deps): update pnpm to v12.15.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2436
* fix(sv): reject waits on class properties that never wake by @tignear in https://github.com/celox-sim/celox/pull/2434
* perf(native): specialize wiring-only clock updates by @tignear in https://github.com/celox-sim/celox/pull/2420
* feat(sv): pass subroutine arguments by reference by @tignear in https://github.com/celox-sim/celox/pull/2411
* fix(sv): read string characters as signed bytes by @tignear in https://github.com/celox-sim/celox/pull/2446
* perf(vpi): avoid allocating empty names for temporary handles by @tignear in https://github.com/celox-sim/celox/pull/2449
* perf(vpi): snapshot pending deposits once per edge batch by @tignear in https://github.com/celox-sim/celox/pull/2450
* ci: keep every CI job's Rust dependency cache warm and keyed alike by @tignear in https://github.com/celox-sim/celox/pull/2454
* perf(vpi): skip unused value format metadata by @tignear in https://github.com/celox-sim/celox/pull/2459
* feat(sv): specialize package classes from importing scopes by @tignear in https://github.com/celox-sim/celox/pull/2429
* test(xezim): update xezim digest to 21834c5 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2453
* feat(sv): implement enumerated type methods by @tignear in https://github.com/celox-sim/celox/pull/2390
* feat(sv): initialize process static locals with run-time values by @tignear in https://github.com/celox-sim/celox/pull/2407
* perf(native): map attached image input and bulk-decode machine code by @tignear in https://github.com/celox-sim/celox/pull/2465
* refactor(runtime)!: support explicitly invoked SIR process kernels by @tignear in https://github.com/celox-sim/celox/pull/2444
* ci: compile the Rust test workspace once for the vtest executor and tests by @tignear in https://github.com/celox-sim/celox/pull/2467
* chore(deps): update dependency @napi-rs/cli to v3.10.9 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2469
* fix(sv): keep package specialization state single in interface-bound modules by @tignear in https://github.com/celox-sim/celox/pull/2474
* fix(deps): drop the zstd versions with open RustSec advisories by @tignear in https://github.com/celox-sim/celox/pull/2475
* feat(sv): support $sformatf and $sformat by @tignear in https://github.com/celox-sim/celox/pull/2402
* feat(sv): support the $swrite family by @tignear in https://github.com/celox-sim/celox/pull/2403
* ci: key the vtest dependency cache by the executor's build plan by @tignear in https://github.com/celox-sim/celox/pull/2487
* perf(native): keep attached machine code in file mappings by @tignear in https://github.com/celox-sim/celox/pull/2480
* feat(sv): support unpacked structures by @tignear in https://github.com/celox-sim/celox/pull/2415
* feat(sv): support class properties of unpacked structure type by @tignear in https://github.com/celox-sim/celox/pull/2419
* feat(sv): support unpacked structures as dynamic container elements by @tignear in https://github.com/celox-sim/celox/pull/2422
* feat(sv): support nested dynamic containers and class handle keys by @tignear in https://github.com/celox-sim/celox/pull/2424
* feat(sv): pass unpacked structures by reference by @tignear in https://github.com/celox-sim/celox/pull/2473
* feat(sv): bind method arguments by name and default by @tignear in https://github.com/celox-sim/celox/pull/2494
* feat(sv): support %p for integral, string, handle and container values by @tignear in https://github.com/celox-sim/celox/pull/2423
* perf(testbench)!: roll up compiled Veryl testbench execution by @tignear in https://github.com/celox-sim/celox/pull/2484
* perf(runtime)!: defer settles that are a function of their inputs by @tignear in https://github.com/celox-sim/celox/pull/2497
* feat(sv): use enumeration constants that classes declare by @tignear in https://github.com/celox-sim/celox/pull/2491
* feat(sv): use container typedefs that classes declare by @tignear in https://github.com/celox-sim/celox/pull/2501
* feat(sv): pass dynamic containers by value and as output arguments by @tignear in https://github.com/celox-sim/celox/pull/2508
* perf(sv): stop repeating discovery work per query and class reference by @tignear in https://github.com/celox-sim/celox/pull/2509
* fix(sv): reject %p of unpacked structures and allow mixed enum conditionals by @tignear in https://github.com/celox-sim/celox/pull/2512
* perf(sv): leave unchanged signatures out of shared discovery tables by @tignear in https://github.com/celox-sim/celox/pull/2514
* fix(sv): fix structure defaults, record padding, bare pops and nested reads by @tignear in https://github.com/celox-sim/celox/pull/2500
* feat(sv): support array locator methods by @tignear in https://github.com/celox-sim/celox/pull/2498
* ci(bench): publish daily Taxi comparisons with Verilator by @tignear in https://github.com/celox-sim/celox/pull/2499
* ci: split sv-tests conformance into shards by @tignear in https://github.com/celox-sim/celox/pull/2520
* chore(deps): update dependency cocotb-test to v0.3.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2522
* chore(deps): update actions/setup-python action to v7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2529
* chore(deps): update dependency scapy to v2.8.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2528
* feat(sv): support $typename by @tignear in https://github.com/celox-sim/celox/pull/2492
* chore(deps): update dependency cocotb to v2.1.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2521
* chore(deps): update dependency ubuntu to v26 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2535
* feat(cli): report when each tier of a tiered run ran by @tignear in https://github.com/celox-sim/celox/pull/2526
* perf(tiered): compile the baseline scheduler without fused domain units by @tignear in https://github.com/celox-sim/celox/pull/2533
* chore(deps): update dependency pytest to v9 [security] by @renovate[bot] in https://github.com/celox-sim/celox/pull/2524
* feat(sv): select members of class handles that calls return by @tignear in https://github.com/celox-sim/celox/pull/2537
* chore(deps): update dependency pytest to v9.2.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2543
* fix(sv): accept class typedefs as members of class structures by @tignear in https://github.com/celox-sim/celox/pull/2515
* feat(sv): support increment and decrement in expressions by @tignear in https://github.com/celox-sim/celox/pull/2541
* feat(sv): accept DPI-C exports by @tignear in https://github.com/celox-sim/celox/pull/2525
* feat(sv): support virtual interface variables by @tignear in https://github.com/celox-sim/celox/pull/2440
* chore(deps): update rust toolchain to afcefd0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2554
* feat(sv): wait on members through virtual interfaces by @tignear in https://github.com/celox-sim/celox/pull/2441
* refactor(x86): select the register allocator by strategy by @tignear in https://github.com/celox-sim/celox/pull/2551
* perf(x86): allocate baseline-tier registers with a linear allocator by @tignear in https://github.com/celox-sim/celox/pull/2552
* feat(native): add a builder option for the baseline native pipeline by @tignear in https://github.com/celox-sim/celox/pull/2553
* feat(sv): accept virtual interface class type arguments by @tignear in https://github.com/celox-sim/celox/pull/2442
* fix(x86): read rotated loop phis before their homes are overwritten by @tignear in https://github.com/celox-sim/celox/pull/2562
* test(xezim): update xezim digest to 0fca605 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2490
* feat(sv): implement named events by @tignear in https://github.com/celox-sim/celox/pull/2493
* feat(sv): support streaming concatenation of fixed-size data by @tignear in https://github.com/celox-sim/celox/pull/2557
* feat(sv): support streaming concatenation of dynamically sized data by @tignear in https://github.com/celox-sim/celox/pull/2564
* feat(sv): apply compound assignments to dynamic container elements by @tignear in https://github.com/celox-sim/celox/pull/2575
* feat(sv): take strings that calls return as associative indices by @tignear in https://github.com/celox-sim/celox/pull/2573
* perf(sir): optimize the unit groups of all events concurrently by @tignear in https://github.com/celox-sim/celox/pull/2581
* chore(diagnostics): time the Veryl parser and analyzer passes by @tignear in https://github.com/celox-sim/celox/pull/2582
* fix(sv): follow event variables in triggered waits and keep saved events distinct by @tignear in https://github.com/celox-sim/celox/pull/2569
* feat(sv): concatenate arrays as items of unpacked array concatenations by @tignear in https://github.com/celox-sim/celox/pull/2578
* fix(runtime): order lane tasks by the rounded writes of unpadded strided arrays by @tignear in https://github.com/celox-sim/celox/pull/2587
* feat(sir): add four-state plane moves by @tignear in https://github.com/celox-sim/celox/pull/2574
* feat(sv): support class properties wider than 64 bits by @tignear in https://github.com/celox-sim/celox/pull/2579
* chore(deps): update dependency chart.js to v4.6.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2595
* feat(sv): implement semaphores by @tignear in https://github.com/celox-sim/celox/pull/2570
* feat(sv): implement parameterized mailboxes by @tignear in https://github.com/celox-sim/celox/pull/2599
* fix(sv): extend values stored in class properties by their own signedness by @tignear in https://github.com/celox-sim/celox/pull/2600
* feat(sv): share static task locals between calls by @tignear in https://github.com/celox-sim/celox/pull/2603
* fix(veryl): compile complete binary cases combinationally in two-state builds by @tignear in https://github.com/celox-sim/celox/pull/2605
* ci: write the mention agent messages in English by @tignear in https://github.com/celox-sim/celox/pull/2607
* fix(deps): update dependency vscode-languageclient to v11 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2572
* perf(testbench): keep latch designs' TB edges in the generated scheduler by @tignear in https://github.com/celox-sim/celox/pull/2609
* feat(sv): index associative arrays with process handles by @tignear in https://github.com/celox-sim/celox/pull/2610
* test(suite): expect the retained Icarus failure of process-keyed arrays by @tignear in https://github.com/celox-sim/celox/pull/2616
* feat(sv): write $swrite and $sformat results into integral outputs by @tignear in https://github.com/celox-sim/celox/pull/2612
* feat(sv): support bit and part selects of class properties by @tignear in https://github.com/celox-sim/celox/pull/2588
* feat(sv): support dynamic container elements wider than 64 bits by @tignear in https://github.com/celox-sim/celox/pull/2589
* perf(scheduler): run four-state designs in the compiled scheduler by @tignear in https://github.com/celox-sim/celox/pull/2621
* perf(testbench): run TB assertions, displays and $finish in the generated scheduler by @tignear in https://github.com/celox-sim/celox/pull/2623
* perf(testbench): keep VCD runs and TB-only clocks on native edges by @tignear in https://github.com/celox-sim/celox/pull/2622
* chore(deps): update rust toolchain to 96a0986 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2637
* test(xezim): update xezim digest to 80f00d3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2580
* fix(sv): bind handles declared with class-scoped typedefs by @tignear in https://github.com/celox-sim/celox/pull/2626
* chore(deps): update mbx to 1.29.0 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/2642
* fix(sv): check element types and every assignment of unpacked arrays by @tignear in https://github.com/celox-sim/celox/pull/2638
* refactor(sv): intern semantic types of the analyzer by @tignear in https://github.com/celox-sim/celox/pull/2643
* refactor(sv): define type equivalence and unpacked array assignability by @tignear in https://github.com/celox-sim/celox/pull/2644
* feat(sv): support unpacked structure input and output ports by @tignear in https://github.com/celox-sim/celox/pull/2548
* perf(testbench): run rst.assert, its release and reset edges in the generated scheduler by @tignear in https://github.com/celox-sim/celox/pull/2624
* fix(testbench): keep unknown bits in TB message arguments by @tignear in https://github.com/celox-sim/celox/pull/2625
* fix(sv): compare unpacked array elements by their semantic types by @tignear in https://github.com/celox-sim/celox/pull/2653
* refactor(sv): spell unpacked array diagnostics from semantic types by @tignear in https://github.com/celox-sim/celox/pull/2656
* fix(ci): restore daily Taxi benchmark publication by @tignear in https://github.com/celox-sim/celox/pull/2659
* ci: lock GitHub Actions workflow dependencies by @tignear in https://github.com/celox-sim/celox/pull/2662
* refactor(sv): type unpacked array operands by their selects by @tignear in https://github.com/celox-sim/celox/pull/2663
* fix(ci): build pinned Verilator on Ubuntu 26.04 by @tignear in https://github.com/celox-sim/celox/pull/2664
* fix(ci): require workflow locks before merging by @tignear in https://github.com/celox-sim/celox/pull/2669
* chore(deps): update rust crate globset to v0.4.21 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2676
* refactor(sv): carry the imports of a scope in its analysis context by @tignear in https://github.com/celox-sim/celox/pull/2673
* fix(testbench): keep unknown bits in TB writes and conditions by @tignear in https://github.com/celox-sim/celox/pull/2654
* perf(sv): share scope tables between class specializations by @tignear in https://github.com/celox-sim/celox/pull/2672
* fix(bench): publish rica comparisons on the benchmark dashboard by @tignear in https://github.com/celox-sim/celox/pull/2668
* refactor(native): own spill-plan costs and test counters instead of sharing them through Cell by @tignear in https://github.com/celox-sim/celox/pull/2681
* refactor(sir): pass alias oracles and batch verifiers by `&mut` instead of counting through Cell by @tignear in https://github.com/celox-sim/celox/pull/2682
* refactor(slt): own the lowering caches instead of sharing them through RefCell by @tignear in https://github.com/celox-sim/celox/pull/2683
* refactor(veryl): scope loop ranges through a borrowing guard instead of a shared RefCell by @tignear in https://github.com/celox-sim/celox/pull/2684
* refactor(sv): collect call walks and lowering memos without Cell or RefCell by @tignear in https://github.com/celox-sim/celox/pull/2685
* refactor(runtime): request derived-edge units through `&mut` and drop Cell from tests by @tignear in https://github.com/celox-sim/celox/pull/2686
* test(suite): intern signal and event names through `&mut self` instead of RefCell by @tignear in https://github.com/celox-sim/celox/pull/2687
* refactor(lydite): record proof bundle poisoning through `&mut self` instead of Cell by @tignear in https://github.com/celox-sim/celox/pull/2688
* test(xezim): update xezim digest to 9fadb2e by @renovate[bot] in https://github.com/celox-sim/celox/pull/2678
* perf(sv): bind class signatures once a scope's declarations settle by @tignear in https://github.com/celox-sim/celox/pull/2658
* refactor(sv): pass constant functions explicitly instead of installing them by @tignear in https://github.com/celox-sim/celox/pull/2674
* refactor(sv): carry interface instances and time scopes in the context by @tignear in https://github.com/celox-sim/celox/pull/2675
* ci(deps): configure security-only Cargo updates by @tignear in https://github.com/celox-sim/celox/pull/2199
* perf(scheduler): leave settle inputs and aliases out of TB level slices by @tignear in https://github.com/celox-sim/celox/pull/2699
* chore(deps): update rust toolchain to 6ecf534 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2700
* refactor(sv)!: pass the analysis session explicitly instead of sharing it through RefCell by @tignear in https://github.com/celox-sim/celox/pull/2695
* perf(sv): fold zero-argument functions whose kept result is never read by @tignear in https://github.com/celox-sim/celox/pull/2704
* perf(sv): parse wide based literals in linear time by @tignear in https://github.com/celox-sim/celox/pull/2703
* ci: stop nightly develop external comparisons and Heliodor by @tignear in https://github.com/celox-sim/celox/pull/2714
* fix(deps): update Wasmtime to resolve security advisories by @tignear in https://github.com/celox-sim/celox/pull/2706
* perf(scheduler): settle before asynchronous reset domains instead of fusing them by @tignear in https://github.com/celox-sim/celox/pull/2710
* test(xezim): update xezim digest to e826641 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2697
* ci(release): track pending releases in an issue by @tignear in https://github.com/celox-sim/celox/pull/2718
* feat(sv): support the chandle type by @tignear in https://github.com/celox-sim/celox/pull/2464
* chore(lint): forbid RefCell and Cell by @tignear in https://github.com/celox-sim/celox/pull/2721
* fix(deps): update dependency release-please to v17.11.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2726
* fix(sv): compare the bounds, not the spelling, of non-ANSI port ranges by @tignear in https://github.com/celox-sim/celox/pull/2133
* ci: start an advisory agent when cargo-deny fails on master by @tignear in https://github.com/celox-sim/celox/pull/2707
* feat(sv): provide UVM's DPI-C functions by @tignear in https://github.com/celox-sim/celox/pull/2471
* fix(cranelift): give wide inequality 0 or 1 by @tignear in https://github.com/celox-sim/celox/pull/2730
* feat(cli)!: share SystemVerilog compilation units across ordered source files by @tignear in https://github.com/celox-sim/celox/pull/2724
* refactor(sv): resolve handle classes from expression types by @tignear in https://github.com/celox-sim/celox/pull/2733
* chore(deps): update rust crate regex to v1.13.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2732
* perf(runtime): run designs with more than 64 events in the compiled scheduler by @tignear in https://github.com/celox-sim/celox/pull/2084
* perf(native): settle next-use distances in postorder by @tignear in https://github.com/celox-sim/celox/pull/2737
* perf(native): rewrite all counted loops in one pass by @tignear in https://github.com/celox-sim/celox/pull/2738
* ci: disable LTO in validation builds by @tignear in https://github.com/celox-sim/celox/pull/2745
* feat(sv): support associative array indices wider than 64 bits by @tignear in https://github.com/celox-sim/celox/pull/2747
* fix(sv): select members of unpacked array elements at run-time indices by @tignear in https://github.com/celox-sim/celox/pull/2740
* fix(sv): coalesce repeated literal net drivers by @tignear in https://github.com/celox-sim/celox/pull/2748
* fix(nix): pin xezim to stable releases by @tignear in https://github.com/celox-sim/celox/pull/2746
* chore(deps): update dependency @biomejs/biome to v2.6.3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2753
* feat(bench): publish RTLMeter state-model comparisons by @tignear in https://github.com/celox-sim/celox/pull/2750
* fix(vpi): preserve cocotb edges and expose packed bit handles by @tignear in https://github.com/celox-sim/celox/pull/2756
* feat(sv): print unpacked structures and containers of them with %p by @tignear in https://github.com/celox-sim/celox/pull/2763
* fix(backends): use element-strided arrays in Cranelift and Wasm by @tignear in https://github.com/celox-sim/celox/pull/2758
* fix(backends): use element-strided defaults in every execution path by @tignear in https://github.com/celox-sim/celox/pull/2762
* fix(sv): support queue slice values and assignment updates by @tignear in https://github.com/celox-sim/celox/pull/2755
* fix(runtime): exclude final procedures from combinational startup by @tignear in https://github.com/celox-sim/celox/pull/2770
* fix(sv): support sampled history in concurrent assertion predicates by @tignear in https://github.com/celox-sim/celox/pull/2766
* fix(sv): call methods of package-scoped containers by @tignear in https://github.com/celox-sim/celox/pull/2752
* fix(sv): preserve typed values of assignment expressions by @tignear in https://github.com/celox-sim/celox/pull/2760
* fix(sim)!: run $tb::clock_gen only while a process waits on it by @tignear in https://github.com/celox-sim/celox/pull/2772
* feat(sv): iterate every dimension of a foreach over nested containers by @tignear in https://github.com/celox-sim/celox/pull/2775
* feat(sv): run run-time static initializers once before time zero by @tignear in https://github.com/celox-sim/celox/pull/2782
* fix(sv): support blocking intra-assignment timing controls by @tignear in https://github.com/celox-sim/celox/pull/2776
* perf(sv): reuse unaffected module lowerings on schedule retries by @tignear in https://github.com/celox-sim/celox/pull/2786
* perf(sv): reuse hierarchy discovery analyses during final lowering by @tignear in https://github.com/celox-sim/celox/pull/2790
* feat(sv)!: lower scalar logic gate primitives by @tignear in https://github.com/celox-sim/celox/pull/2778
* feat(vpi)!: pass the main upstream cocotb regression by @tignear in https://github.com/celox-sim/celox/pull/2779
* feat(sv): resolve typedefs of specialized and package-qualified class scopes by @tignear in https://github.com/celox-sim/celox/pull/2793
* ci: retain versions in automated Veryl HEAD rolls by @tignear in https://github.com/celox-sim/celox/pull/2783
* perf(sv): defer executable lowering until hierarchy is resolved by @tignear in https://github.com/celox-sim/celox/pull/2795
* fix(vpi): expose simulation time to cocotb bridge threads by @tignear in https://github.com/celox-sim/celox/pull/2799
* feat(sim): let a drain on another thread take every event of compiled runs by @tignear in https://github.com/celox-sim/celox/pull/2784
* perf(cli): run SystemVerilog simulations in the compiled scheduler by @tignear in https://github.com/celox-sim/celox/pull/2785
* feat(sv): print enum values by name with %p by @tignear in https://github.com/celox-sim/celox/pull/2796
* feat(sv): initialize integral members of unpacked structures by @tignear in https://github.com/celox-sim/celox/pull/2805
* feat(sv): support fixed unpacked string arrays by @tignear in https://github.com/celox-sim/celox/pull/2803
* feat(sv)!: support shared always blocks and delayed nonblocking assignments by @tignear in https://github.com/celox-sim/celox/pull/2807
* ci(bench): add the E203 default hello workload by @tignear in https://github.com/celox-sim/celox/pull/2808
* perf(sv): discover signatures without copying the scope tables by @tignear in https://github.com/celox-sim/celox/pull/2812
* perf(sv): look up only the parameter types an enum value reads by @tignear in https://github.com/celox-sim/celox/pull/2813
* perf(sv): share scope tables with the contexts that bind in them by @tignear in https://github.com/celox-sim/celox/pull/2814
* fix(native): rebind callees when replacing tiered code by @tignear in https://github.com/celox-sim/celox/pull/2816
* feat(sim): let a paced runtime event drain sleep until it has work by @tignear in https://github.com/celox-sim/celox/pull/2815
* fix(sv): assign bit-selects and part-selects of dynamic container elements by @tignear in https://github.com/celox-sim/celox/pull/2806
* feat(sv): let method defaults use the object the method is called on by @tignear in https://github.com/celox-sim/celox/pull/2830
* feat(sv): cast class handles to class properties and in void casts by @tignear in https://github.com/celox-sim/celox/pull/2810
* fix(sv): resolve unqualified names in methods to class members before module names by @tignear in https://github.com/celox-sim/celox/pull/2834
* perf(sv): look up scope names by name when collecting imports by @tignear in https://github.com/celox-sim/celox/pull/2836
* perf(sv): keep the classes that dispatchers select from while no class changes by @tignear in https://github.com/celox-sim/celox/pull/2837
* perf(sv): find class declarations by name through a file index by @tignear in https://github.com/celox-sim/celox/pull/2838
* feat(sv): call class methods without their empty parentheses by @tignear in https://github.com/celox-sim/celox/pull/2839
* feat(scheduler): compile kernels that wait for several edges and delays by @tignear in https://github.com/celox-sim/celox/pull/2823
* feat(sv): support fixed arrays of integral unpacked structures by @tignear in https://github.com/celox-sim/celox/pull/2820
* refactor(sv): share semantic expression result types by @tignear in https://github.com/celox-sim/celox/pull/2822
* fix(heap): share storage between retained container versions by @tignear in https://github.com/celox-sim/celox/pull/2848
* feat(sv): declare typedefs in block and subroutine bodies by @tignear in https://github.com/celox-sim/celox/pull/2831
* perf(scheduler): evaluate blocking clock updates before publishing NBAs by @tignear in https://github.com/celox-sim/celox/pull/2817
* feat(cli): report SystemVerilog scheduler execution statistics by @tignear in https://github.com/celox-sim/celox/pull/2818
* chore(deps): update rust toolchain to c85ccd3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2854
* perf(scheduler): allow unrelated Active updates with procedural clocks by @tignear in https://github.com/celox-sim/celox/pull/2853
* perf(scheduler): preserve deferred NBAs when derived clock domains finish by @tignear in https://github.com/celox-sim/celox/pull/2855
* fix(sv): preserve X/Z in dynamic blocking assignments by @tignear in https://github.com/celox-sim/celox/pull/2858
* feat(sv): support integral array members of unpacked structs by @tignear in https://github.com/celox-sim/celox/pull/2859
* fix(sv): preserve semantic types of last queue elements by @tignear in https://github.com/celox-sim/celox/pull/2869
* feat(sv): support default-only associative array literals by @tignear in https://github.com/celox-sim/celox/pull/2870
* ci(release): retry npm provenance log conflicts by @tignear in https://github.com/celox-sim/celox/pull/2874
* perf(scheduler): compile wide and OR-list event waits by @tignear in https://github.com/celox-sim/celox/pull/2868
* test: require wasm component hosting for the registry VIP by @tignear in https://github.com/celox-sim/celox/pull/2879
* refactor(sv): type numeric value expressions without binding by @tignear in https://github.com/celox-sim/celox/pull/2880
* feat(sv): mutate elements of fixed string arrays by @tignear in https://github.com/celox-sim/celox/pull/2882
* fix(sv): read a prototype's lone formal identifier as its name when it names no type by @tignear in https://github.com/celox-sim/celox/pull/2881
* fix(sv): preserve X/Z in package enum constants by @tignear in https://github.com/celox-sim/celox/pull/2883
* fix(x86): legalize masks for wide helper results by @tignear in https://github.com/celox-sim/celox/pull/2884
* ci(sv-tests): publish daily verification dashboard by @tignear in https://github.com/celox-sim/celox/pull/2863
* fix(sv): resolve semantic types of string characters by @tignear in https://github.com/celox-sim/celox/pull/2872
* refactor(sv): retain semantic return types through binding by @tignear in https://github.com/celox-sim/celox/pull/2889
* ci: avoid duplicate workflow lockfile checks after merge by @tignear in https://github.com/celox-sim/celox/pull/2906
* perf(scheduler): resume RTL-driven waits in native slots by @tignear in https://github.com/celox-sim/celox/pull/2886
* chore(deps): update dependency fusesoc to v2.4.7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2904
* fix(sv): share package variables with interface instances bound in a module by @tignear in https://github.com/celox-sim/celox/pull/2890
* feat(sv): shuffle dynamic arrays and queues by @tignear in https://github.com/celox-sim/celox/pull/2910
* fix(sv): accept non-UTF-8 comments by default by @tignear in https://github.com/celox-sim/celox/pull/2913
* chore(deps): update dependency edalize to v0.6.8 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2903
* refactor(sv): retain declaration semantic types through binding by @tignear in https://github.com/celox-sim/celox/pull/2918
* feat(cli): add VCD recording without disabling compiled schedulers by @tignear in https://github.com/celox-sim/celox/pull/2919
* fix(sv): bind unpacked structure clocks with scope types by @tignear in https://github.com/celox-sim/celox/pull/2921
* feat(sv): assign the results of functions returning containers by @tignear in https://github.com/celox-sim/celox/pull/2900
* fix(sv): round real values stored to integral class properties by @tignear in https://github.com/celox-sim/celox/pull/2893
* feat(sv): evaluate time literals in expressions by @tignear in https://github.com/celox-sim/celox/pull/2894
* refactor(sv): type casts and conditional value expressions by @tignear in https://github.com/celox-sim/celox/pull/2927
* feat(sv): support $system host commands by @tignear in https://github.com/celox-sim/celox/pull/2931
* fix(sv): preserve wide parameter literals in selection bounds by @tignear in https://github.com/celox-sim/celox/pull/2933
* fix(sv): bind constants from inline enum declarations by @tignear in https://github.com/celox-sim/celox/pull/2922
* fix(sv): avoid false latch inference after local muxes by @tignear in https://github.com/celox-sim/celox/pull/2923
* fix(sv): support single-cycle Boolean overlapped implication by @tignear in https://github.com/celox-sim/celox/pull/2934
* fix(sv): lower bit logic and reg scalar casts by @tignear in https://github.com/celox-sim/celox/pull/2935
* chore(deps): update rust crate cranelift to v0.137.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2944
* feat(sv): support real and shortreal class properties by @tignear in https://github.com/celox-sim/celox/pull/2895
* feat(sv): support inertial continuous assignment delays by @tignear in https://github.com/celox-sim/celox/pull/2952
* fix(sv): honor delays on net declaration assignments by @tignear in https://github.com/celox-sim/celox/pull/2956
* refactor(sv): resolve enum constants from declaration bindings by @tignear in https://github.com/celox-sim/celox/pull/2945
* fix(sv): apply independent net delays per bit by @tignear in https://github.com/celox-sim/celox/pull/2958
* perf(x86): pack four-state lanes and reuse bit transpose tiles by @tignear in https://github.com/celox-sim/celox/pull/2959
* perf(scheduler): keep RTL-driven slots on the native scheduler by @tignear in https://github.com/celox-sim/celox/pull/2917
* test(sv): cover one-bit casts of rand_mode() by @tignear in https://github.com/celox-sim/celox/pull/2939
* feat(sv): share the static formals of package and $unit timed tasks by @tignear in https://github.com/celox-sim/celox/pull/2940
* feat(sv): randomize enumeration and randc properties under constraints by @tignear in https://github.com/celox-sim/celox/pull/2941
* perf(scheduler): avoid redundant combinational reevaluation by @tignear in https://github.com/celox-sim/celox/pull/2961
* refactor(sv): retain scalar parameter declaration types by @tignear in https://github.com/celox-sim/celox/pull/2964
* feat(sv): read file lines into strings by @tignear in https://github.com/celox-sim/celox/pull/2967
* feat(sv): report file I/O error status by @tignear in https://github.com/celox-sim/celox/pull/2968
* feat(sv): iterate the characters of a string in foreach by @tignear in https://github.com/celox-sim/celox/pull/2896
* feat(sv): sort and rsort arrays with a with expression by @tignear in https://github.com/celox-sim/celox/pull/2897
* feat(sv): keep the enumeration type of a foreach index of an associative array by @tignear in https://github.com/celox-sim/celox/pull/2898
* test(suite): retain external failures of four-state container equality by @tignear in https://github.com/celox-sim/celox/pull/2981
* chore(sv): report the recursive call cycles of lowered modules by @tignear in https://github.com/celox-sim/celox/pull/2980
* feat(sv): cast values to enumerated types with $cast by @tignear in https://github.com/celox-sim/celox/pull/2847
* feat(sv): build enumeration domains for bases up to 128 bits by @tignear in https://github.com/celox-sim/celox/pull/2987
* feat(sir): add function units and calls that run them in frames by @tignear in https://github.com/celox-sim/celox/pull/2984
* feat(sir): run function calls in the Cranelift and wasm backends by @tignear in https://github.com/celox-sim/celox/pull/2988
* refactor(sv): bind selects of class properties and container elements on their values by @tignear in https://github.com/celox-sim/celox/pull/2954
* feat(sir): run function calls in the native backends by @tignear in https://github.com/celox-sim/celox/pull/2992
* refactor(sv): assign class properties and container elements as places of the ordinary target by @tignear in https://github.com/celox-sim/celox/pull/2979
* feat(sv): read and write members of packed structure class properties by @tignear in https://github.com/celox-sim/celox/pull/2928
* feat(sv): read part-selects of container elements by @tignear in https://github.com/celox-sim/celox/pull/2929
* feat(sv): scan into class properties with $sscanf and $fscanf by @tignear in https://github.com/celox-sim/celox/pull/2930
* fix(sv): select members through packed structure array indices by @tignear in https://github.com/celox-sim/celox/pull/2936
* fix(sv): schedule always_latch nonblocking assignments through processes by @tignear in https://github.com/celox-sim/celox/pull/2937
* ci(release): start releases only by manual dispatch by @tignear in https://github.com/celox-sim/celox/pull/3008
* perf(scheduler): skip unchanged settle partitions and inactive waits by @tignear in https://github.com/celox-sim/celox/pull/3003
* perf(x86): reduce four-state plane operations by @tignear in https://github.com/celox-sim/celox/pull/3004
* perf(x86): align large JIT images for huge pages by @tignear in https://github.com/celox-sim/celox/pull/3005


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.12.0...v0.13.0

## 0.12.0 (2026-10-28)

## What's Changed
* fix(sv): parse virtual method qualifiers before visibility in https://github.com/celox-sim/celox/pull/2276
* feat(sv)!: support per-object constraint modes in https://github.com/celox-sim/celox/pull/2271
* ci: default pull request Rust tests to native serial execution in https://github.com/celox-sim/celox/pull/2283
* feat(sv): support dynamic container class properties in https://github.com/celox-sim/celox/pull/2278
* feat(sv): forward interface header ports through modports in https://github.com/celox-sim/celox/pull/2277
* test(xezim): update xezim digest to 5eab5f2 in https://github.com/celox-sim/celox/pull/2286
* fix(sv): retain conditional types in constant functions in https://github.com/celox-sim/celox/pull/2289
* ci: separate external comparisons from release qualification in https://github.com/celox-sim/celox/pull/2288
* chore(deps): update dependency lefthook to v2.2.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1282
* chore(deps): update dependency @playwright/test to v1.64.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1284
* fix(release): publish SV analyzer before the test suite by @tignear in https://github.com/celox-sim/celox/pull/1287
* chore(deps): update rust toolchain to e9d12b9 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1288
* ci: migrate merge automation to Mergify by @tignear in https://github.com/celox-sim/celox/pull/1290
* chore(deps): update dependency lefthook to v2.2.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1292
* chore(deps): update dependency vite to v8.3.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1296
* ci: restore focused PR checks and parallel merge validation by @tignear in https://github.com/celox-sim/celox/pull/1300
* ci: pin the published Mergify CLI setup action by @tignear in https://github.com/celox-sim/celox/pull/1305
* fix(deps): update rust crate syn to v3.0.9 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1299
* chore(deps): update rust crate sha2 to v0.11.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1310
* ci: batch queued PRs and defer lydite gates to the queue by @tignear in https://github.com/celox-sim/celox/pull/1308
* ci: ignore obsolete merge queue title checks by @tignear in https://github.com/celox-sim/celox/pull/1319
* ci: move full CodeQL analysis to weekly and manual runs by @tignear in https://github.com/celox-sim/celox/pull/1311
* chore(deps): update rust toolchain to 87ffb7f by @renovate[bot] in https://github.com/celox-sim/celox/pull/1302
* ci: separate merge queues by category and independent scope by @tignear in https://github.com/celox-sim/celox/pull/1318
* ci: preserve shared Rust caches and avoid unnecessary product builds by @tignear in https://github.com/celox-sim/celox/pull/1324
* chore(deps): update pnpm to v12.11.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1332
* feat(sv): implement $urandom by @tignear in https://github.com/celox-sim/celox/pull/1294
* chore(deps): update rust to v1.99.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1303
* chore(deps): update rust toolchain to ec07af4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1335
* ci: run Veryl proof gates only weekly or manually by @tignear in https://github.com/celox-sim/celox/pull/1337
* fix(backend): zero logical shifts by counts at or above the width by @tignear in https://github.com/celox-sim/celox/pull/1340
* chore(claude): remove the overlayfs worktree hooks by @tignear in https://github.com/celox-sim/celox/pull/1341
* chore(deps): update mbx to 1.24.0 and automate updates by @tignear in https://github.com/celox-sim/celox/pull/1346
* fix(vite-plugin): retry transient ticket read failures by @tignear in https://github.com/celox-sim/celox/pull/1348
* fix(native): mask rounded partial-chunk loads by @tignear in https://github.com/celox-sim/celox/pull/1351
* chore(lydite): drop unreferenced audit evidence and translate docs to English by @tignear in https://github.com/celox-sim/celox/pull/1291
* test(suite): add timed script operations and the timing oracle by @tignear in https://github.com/celox-sim/celox/pull/1354
* chore(deps): update pnpm to v12.11.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1358
* chore(deps): update pnpm to v12.11.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1363
* fix(lydite): sync Veryl conformance lockfile with syn pin by @tignear in https://github.com/celox-sim/celox/pull/1366
* fix(native): preserve loaded slices across state writes by @tignear in https://github.com/celox-sim/celox/pull/1367
* fix(sim): fire derived clock domains at their real edges by @tignear in https://github.com/celox-sim/celox/pull/1364
* test(testbench): cover Veryl string variable semantics by @tignear in https://github.com/celox-sim/celox/pull/1370
* ci(lydite): select tests by dependencies and weekly cadence by @tignear in https://github.com/celox-sim/celox/pull/1362
* fix(sim): keep clock copies sensed at the opposite edge apart by @tignear in https://github.com/celox-sim/celox/pull/1373
* test(runtime): isolate parallel trace capture from concurrent tests by @tignear in https://github.com/celox-sim/celox/pull/1375
* feat(testbench)!: support basic Veryl string values by @tignear in https://github.com/celox-sim/celox/pull/1371
* ci: check dependencies with cargo-deny by @tignear in https://github.com/celox-sim/celox/pull/1380
* refactor(sv): identify system tasks and functions by type by @tignear in https://github.com/celox-sim/celox/pull/1381
* ci: move non-required PR checks to required lint or schedules by @tignear in https://github.com/celox-sim/celox/pull/1382
* fix(deps): update rust crate wasm-encoder to 0.262 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1383
* feat(sv): support both edges of one clock or reset by @tignear in https://github.com/celox-sim/celox/pull/1384
* feat(runtime)!: wake processes on events in front of the batch step by @tignear in https://github.com/celox-sim/celox/pull/1361
* ci: select PR checks from the merge base, not the base tip by @tignear in https://github.com/celox-sim/celox/pull/1393
* feat(sv): know every system task and function IEEE 1800-2023 names by @tignear in https://github.com/celox-sim/celox/pull/1392
* docs: remove ADRs moved to Claude Docs by @tignear in https://github.com/celox-sim/celox/pull/1397
* ci(lydite): retire the stale workflow path and skip release PRs by @tignear in https://github.com/celox-sim/celox/pull/1403
* ci: support Mergify stacked pull requests by @tignear in https://github.com/celox-sim/celox/pull/1406
* fix(veryl): reject string and float values Celox cannot represent by @tignear in https://github.com/celox-sim/celox/pull/1405
* feat(sv): support $random and $urandom_range by @tignear in https://github.com/celox-sim/celox/pull/1396
* refactor(runtime)!: split the simulation scheduler into modules by @tignear in https://github.com/celox-sim/celox/pull/1408
* chore(deps): update rust toolchain to a110337 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1416
* chore(deps): update rust crate codspeed-criterion-compat to v5.0.3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1421
* chore(deps): update mbx to 1.24.1 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/1420
* chore(deps): update cachix/install-nix-action digest to 448cde7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1424
* refactor(runtime)!: derive the schedule model once per program by @tignear in https://github.com/celox-sim/celox/pull/1417
* ci(lydite): scope Cargo.lock edits by the lockfile dependency graph by @tignear in https://github.com/celox-sim/celox/pull/1427
* chore(deps): update pnpm to v12.12.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1428
* feat(sim): add explicit clock edges to the simulator by @tignear in https://github.com/celox-sim/celox/pull/1391
* refactor(runtime)!: mark periodic clock events on the queue by @tignear in https://github.com/celox-sim/celox/pull/1431
* ci: keep conflicting pull requests out of the merge queue by @tignear in https://github.com/celox-sim/celox/pull/1447
* fix(sv): preserve typedef variable declaration initializers by @tignear in https://github.com/celox-sim/celox/pull/1443
* fix(sv): preserve generate-local array element widths in size queries by @tignear in https://github.com/celox-sim/celox/pull/1450
* fix(sv)!: bind type parameters without rewriting source by @tignear in https://github.com/celox-sim/celox/pull/1451
* feat(sim): expose four-state reads in timed simulations by @tignear in https://github.com/celox-sim/celox/pull/1454
* feat(sim): build timed simulations on explicit backends by @tignear in https://github.com/celox-sim/celox/pull/1455
* feat(sv)!: support $stop and $exit by @tignear in https://github.com/celox-sim/celox/pull/1402
* feat(sv): support generate-block imports of types and subroutines that differ from the module's by @tignear in https://github.com/celox-sim/celox/pull/1440
* fix(sv)!: name Veryl declarations by their emitted SV names by @tignear in https://github.com/celox-sim/celox/pull/1469
* fix(sim)!: preserve global precision in VCD and VPI by @tignear in https://github.com/celox-sim/celox/pull/1471
* feat(sv)!: run delays, event and level waits in initial processes by @tignear in https://github.com/celox-sim/celox/pull/1459
* refactor(sv)!: bind assignment patterns without rewriting source by @tignear in https://github.com/celox-sim/celox/pull/1467
* fix(sim): judge four-state edges of internally driven clocks by IEEE 1800 Table 9-2 by @tignear in https://github.com/celox-sim/celox/pull/1414
* ci: scope pull request Rust builds and checks by @tignear in https://github.com/celox-sim/celox/pull/1478
* feat(sv)!: run always blocks with timing controls as processes by @tignear in https://github.com/celox-sim/celox/pull/1475
* chore(deps): update rust toolchain to 42076d1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1487
* fix(sv)!: keep Veryl module identity across mixed hierarchy links by @tignear in https://github.com/celox-sim/celox/pull/1488
* fix(sim)!: commit blocking flip-flop stores in the active region by @tignear in https://github.com/celox-sim/celox/pull/1480
* fix(js): return normally when bounded simulations stop by @tignear in https://github.com/celox-sim/celox/pull/1490
* refactor(sv)!: lower interfaces from original syntax by @tignear in https://github.com/celox-sim/celox/pull/1468
* fix(sv): preserve interface bindings during declaration analysis by @tignear in https://github.com/celox-sim/celox/pull/1474
* fix(sv): check native module indices and visit Veryl bodies once by @tignear in https://github.com/celox-sim/celox/pull/1500
* feat(sv): run nonblocking assignments in processes by @tignear in https://github.com/celox-sim/celox/pull/1501
* chore(deps): update pnpm to v12.12.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1504
* feat(sv)!: support $time and $stime by @tignear in https://github.com/celox-sim/celox/pull/1462
* fix(runtime): settle host writes before resuming from $stop by @tignear in https://github.com/celox-sim/celox/pull/1506
* ci(lydite): skip unrelated sv-tests corpus changes by @tignear in https://github.com/celox-sim/celox/pull/1513
* test(sv-tests): track two-state and four-state results separately by @tignear in https://github.com/celox-sim/celox/pull/1509
* fix(sv): allow arbitrary bounds in multidimensional packed arrays by @tignear in https://github.com/celox-sim/celox/pull/1510
* fix(sv): resolve implicit named interface port connections by @tignear in https://github.com/celox-sim/celox/pull/1511
* fix(sv-tests): use the upstream runner for conformance verdicts by @tignear in https://github.com/celox-sim/celox/pull/1516
* feat(sv)!: report SystemVerilog compilation warnings by @tignear in https://github.com/celox-sim/celox/pull/1432
* fix(sv-tests): run parsing-only conformance cases by @tignear in https://github.com/celox-sim/celox/pull/1521
* chore(deps): update rust toolchain to v1.99.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1527
* docs(sv): separate the getting-started guide from the language reference by @tignear in https://github.com/celox-sim/celox/pull/1522
* chore(deps): update rust toolchain to 550547c by @renovate[bot] in https://github.com/celox-sim/celox/pull/1539
* fix(sv): handle Veryl-emitted interface declarations and calls by @tignear in https://github.com/celox-sim/celox/pull/1525
* fix(sv): preserve parameter overrides with explicit analysis state by @tignear in https://github.com/celox-sim/celox/pull/1526
* refactor(native): own emission state in the assembler by @tignear in https://github.com/celox-sim/celox/pull/1530
* refactor(veryl)!: pass lowering state and dependency collectors explicitly by @tignear in https://github.com/celox-sim/celox/pull/1531
* refactor(vpi)!: own runtime state in an explicit VPI session by @tignear in https://github.com/celox-sim/celox/pull/1532
* ci: narrow Rust builds and shared-case tests per pull request by @tignear in https://github.com/celox-sim/celox/pull/1542
* ci: freeze release PR updates while queued by @tignear in https://github.com/celox-sim/celox/pull/1551
* chore(deps): update vitest monorepo to v5.0.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1545
* feat(sv-syntax): add the native SystemVerilog syntax layer by @tignear in https://github.com/celox-sim/celox/pull/1555
* refactor(sv)!: replace sv-parser with native syntax consumers by @tignear in https://github.com/celox-sim/celox/pull/1556
* refactor(sv): organize syntax binding modules by responsibility by @tignear in https://github.com/celox-sim/celox/pull/1560
* chore(deps): update compatible Python and cocotb versions by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/1554
* fix(lydite): execute derived clock events in proof conformance by @tignear in https://github.com/celox-sim/celox/pull/1559
* chore(lint): prohibit thread-local state outside unit tests by @tignear in https://github.com/celox-sim/celox/pull/1533
* feat(cli): run SystemVerilog and Veryl native testbenches by @tignear in https://github.com/celox-sim/celox/pull/1564
* feat(vpi): run SystemVerilog designs with cocotb by @tignear in https://github.com/celox-sim/celox/pull/1441
* feat(vpi): compile multiple SystemVerilog sources for cocotb by @tignear in https://github.com/celox-sim/celox/pull/1442
* perf(sir): optimize resumable process kernels by @tignear in https://github.com/celox-sim/celox/pull/1565
* fix(vpi): notify clock edges before flip-flop updates by @tignear in https://github.com/celox-sim/celox/pull/1570
* perf(cli): default HDL testbench runs to O2 by @tignear in https://github.com/celox-sim/celox/pull/1580
* chore(deps): update rust crate libc to v0.2.191 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1583
* chore(deps): update pnpm to v12.13.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1594
* chore(deps): update rust toolchain to 5424e1e by @renovate[bot] in https://github.com/celox-sim/celox/pull/1593
* ci(mergify): resolve URL-form stack dependencies by @tignear in https://github.com/celox-sim/celox/pull/1604
* fix(veryl): preserve one-bit struct and union member selects by @tignear in https://github.com/celox-sim/celox/pull/1597
* chore(deps): update mbx to 1.25.0 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/1601
* feat(sv): support task calls with timing controls by @tignear in https://github.com/celox-sim/celox/pull/1592
* feat(cli)!: roll up native testbench and simulator configuration by @tignear in https://github.com/celox-sim/celox/pull/1605
* fix(macros): write array elements through generated IO setters by @tignear in https://github.com/celox-sim/celox/pull/1613
* test(sv): cover bits queries in module parameter overrides by @tignear in https://github.com/celox-sim/celox/pull/1620
* fix(bench): enable parallel execution in the Heliodor tiered runner by @tignear in https://github.com/celox-sim/celox/pull/1625
* fix(runtime): recheck abort after parallel dependency waits by @tignear in https://github.com/celox-sim/celox/pull/1623
* fix(sv): resolve constant functions in unpacked array dimensions by @tignear in https://github.com/celox-sim/celox/pull/1612
* feat(sv)!: add distribution and plusarg system functions by @tignear in https://github.com/celox-sim/celox/pull/1608
* chore(deps): update dependency lefthook to v2.2.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1599
* feat(sv)!: support runtime file output and memory dumps by @tignear in https://github.com/celox-sim/celox/pull/1609
* feat(sv)!: add source-driven VCD controls by @tignear in https://github.com/celox-sim/celox/pull/1610
* fix(deps): update rust crate wasmtime to v50 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1636
* fix(native): ignore padding in wide reduction AND by @tignear in https://github.com/celox-sim/celox/pull/1638
* fix(vite): resolve cached Veryl imports inside and outside the root by @tignear in https://github.com/celox-sim/celox/pull/1596
* feat(cli)!: unify benchmark execution under celox run by @tignear in https://github.com/celox-sim/celox/pull/1633
* fix(sv): resolve Taxi elaboration and cocotb visibility failures by @tignear in https://github.com/celox-sim/celox/pull/1645
* feat(sv): support time literals in delay controls by @tignear in https://github.com/celox-sim/celox/pull/1639
* chore(deps): update dependency typedoc-plugin-markdown to v4.14.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1651
* fix(sv): validate module and interface names without interface ports by @tignear in https://github.com/celox-sim/celox/pull/1621
* fix(sv): distribute interface array port connections by @tignear in https://github.com/celox-sim/celox/pull/1622
* fix(wasm): preserve WASM loading with emnapi 2 beta by @renovate[bot] in https://github.com/celox-sim/celox/pull/1536
* feat(sv): support unconstrained scope randomization by @tignear in https://github.com/celox-sim/celox/pull/1640
* ci(sv): run upstream popcount testbench in daily checks by @tignear in https://github.com/celox-sim/celox/pull/1641
* feat(cli): configure SV parameter literals and simulation state modes by @tignear in https://github.com/celox-sim/celox/pull/1646
* fix(sir-opt): preserve memory and type semantics when hoisting loads by @tignear in https://github.com/celox-sim/celox/pull/1665
* fix(sv): preserve Taxi aggregates and sparse FF memories by @tignear in https://github.com/celox-sim/celox/pull/1659
* fix(vpi): expose unpacked arrays through lazy element handles by @tignear in https://github.com/celox-sim/celox/pull/1660
* fix(cli): reject empty cocotb test selections by @tignear in https://github.com/celox-sim/celox/pull/1661
* perf(vpi): avoid metadata cloning during value access by @tignear in https://github.com/celox-sim/celox/pull/1668
* perf(sv): reuse constants when expression contexts match by @tignear in https://github.com/celox-sim/celox/pull/1669
* feat(simulator): support scalar floating-point arithmetic and formatting by @tignear in https://github.com/celox-sim/celox/pull/1666
* feat(cli): configure SV simulation timescales by @tignear in https://github.com/celox-sim/celox/pull/1672
* fix(sv): fill unbased parameter overrides to their declared width by @tignear in https://github.com/celox-sim/celox/pull/1677
* fix(sv): defer indexed part-select base binding until types are available by @tignear in https://github.com/celox-sim/celox/pull/1678
* feat(sim)!: support blocking latches and configurable inference diagnostics by @tignear in https://github.com/celox-sim/celox/pull/1627
* feat(bindings): apply common latch diagnostics across simulator entry points by @tignear in https://github.com/celox-sim/celox/pull/1628
* chore(deps): update rust toolchain to 83dd732 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1693
* chore(deps): update mbx to 1.26.0 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/1691
* fix(sv): preserve inherited constants in parameter alias evaluation by @tignear in https://github.com/celox-sim/celox/pull/1682
* chore(deps): update dependency @playwright/test to v1.64.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1698
* feat(sv)!: support binary input and file positioning by @tignear in https://github.com/celox-sim/celox/pull/1662
* feat(sv)!: support integral formatted input by @tignear in https://github.com/celox-sim/celox/pull/1663
* feat(sv)!: add Annex N integer distribution functions by @tignear in https://github.com/celox-sim/celox/pull/1664
* feat(sv): lower expression events and @* in processes by @tignear in https://github.com/celox-sim/celox/pull/1681
* perf(backend): lower scalar floating-point operations to native instructions by @tignear in https://github.com/celox-sim/celox/pull/1704
* docs(sv): document timing controls in processes by @tignear in https://github.com/celox-sim/celox/pull/1716
* feat(testbench)!: keep strings in a heap referenced from state memory by @tignear in https://github.com/celox-sim/celox/pull/1679
* fix(sv): scale and round real-valued delays by @tignear in https://github.com/celox-sim/celox/pull/1723
* perf(sv)!: stage nonblocking updates of a target with one writer by @tignear in https://github.com/celox-sim/celox/pull/1684
* fix(sv): preserve signedness of power expressions by @tignear in https://github.com/celox-sim/celox/pull/1718
* feat(sv)!: support string variables and their operations by @tignear in https://github.com/celox-sim/celox/pull/1688
* chore(deps): update napi-rs and emnapi compatibility to v2.0.0-beta.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1725
* fix(sv): lower always @(list) like always @* when it lists every read by @tignear in https://github.com/celox-sim/celox/pull/1722
* chore(deps): update rust toolchain to 5f267b7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1740
* fix(deps): update wasmtime to v49.0.4 by @tignear in https://github.com/celox-sim/celox/pull/1741
* fix(sv): forward named unpacked array parameters by @tignear in https://github.com/celox-sim/celox/pull/1744
* test(sv): timed scheduler boundary cases and scheduler bench by @tignear in https://github.com/celox-sim/celox/pull/1717
* feat(sv)!: implement $realtime by @tignear in https://github.com/celox-sim/celox/pull/1734
* chore(deps): update rust crate codspeed-criterion-compat to v6 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1703
* feat(sv)!: implement real math system functions by @tignear in https://github.com/celox-sim/celox/pull/1735
* feat(sv): support real formatted input by @tignear in https://github.com/celox-sim/celox/pull/1736
* fix(sim)!: reject designs with processes in untimed Simulator builds by @tignear in https://github.com/celox-sim/celox/pull/1720
* fix(sv): lower loop-indexed packed word part-selects by @tignear in https://github.com/celox-sim/celox/pull/1752
* feat(sv)!: support classes with properties, methods and constructors by @tignear in https://github.com/celox-sim/celox/pull/1689
* chore(deps): update vitest monorepo to v5.0.5 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1760
* chore(deps): update pnpm to v12.14.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1761
* ci: skip closed PR title checks and repair script validation by @tignear in https://github.com/celox-sim/celox/pull/1753
* fix(sv): preserve guarded division types through casts by @tignear in https://github.com/celox-sim/celox/pull/1766
* feat(sv)!: retain static function locals and results between calls by @tignear in https://github.com/celox-sim/celox/pull/1737
* refactor(runtime)!: run timed slots from SimulationState with a differential scheduler harness by @tignear in https://github.com/celox-sim/celox/pull/1756
* fix(sv): count intermediate writes of combinational and flip-flop units by @tignear in https://github.com/celox-sim/celox/pull/1726
* feat(sv): support static class members, super and base constructors by @tignear in https://github.com/celox-sim/celox/pull/1696
* feat(sv): support virtual methods, abstract classes and interface class implementation by @tignear in https://github.com/celox-sim/celox/pull/1724
* feat(sv)!: support interface class handles and $cast of class handles by @tignear in https://github.com/celox-sim/celox/pull/1758
* fix(sv): resolve input port patterns after specialization by @tignear in https://github.com/celox-sim/celox/pull/1772
* fix(sv): run waiters of blocking edge writes before the NBA region by @tignear in https://github.com/celox-sim/celox/pull/1783
* feat(sim)!: make a tick one clock period and run every clock generator by @tignear in https://github.com/celox-sim/celox/pull/1776
* feat(sv)!: support opt-in interface-array size queries by @tignear in https://github.com/celox-sim/celox/pull/1791
* fix(sv): write the bits inside a part-select partly outside its vector by @tignear in https://github.com/celox-sim/celox/pull/1782
* feat(runtime)!: compiled time-slot loop for delay-only kernels by @tignear in https://github.com/celox-sim/celox/pull/1777
* feat(sv): run plain always blocks without the flip-flop shape as processes by @tignear in https://github.com/celox-sim/celox/pull/1729
* perf(runtime)!: compare counted wait baselines in place by @tignear in https://github.com/celox-sim/celox/pull/1803
* perf(sim): keep the inactive edge of a tick cheap when nothing can fire on it by @tignear in https://github.com/celox-sim/celox/pull/1789
* fix(sv): bind interface-array slices to module ports by @tignear in https://github.com/celox-sim/celox/pull/1807
* fix(sim)!: wake processes on value changes that are undone in the same time slot by @tignear in https://github.com/celox-sim/celox/pull/1764
* chore(deps): update rust toolchain to b0829a2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1814
* feat(sv): run nonblocking assignments in processes to array elements and run-time positions by @tignear in https://github.com/celox-sim/celox/pull/1773
* fix(sv): name event declarations and disable fork, and reject timing in functions as illegal by @tignear in https://github.com/celox-sim/celox/pull/1818
* feat(sv)!: support unconstrained class randomization by @tignear in https://github.com/celox-sim/celox/pull/1767
* feat(sv)!: support inherited randomization and rand_mode by @tignear in https://github.com/celox-sim/celox/pull/1787
* feat(runtime)!: fuse clock-waiting kernels into compiled slots by @tignear in https://github.com/celox-sim/celox/pull/1801
* feat(runtime)!: host periodic clocks in the compiled loop by @tignear in https://github.com/celox-sim/celox/pull/1805
* perf(runtime)!: skip compiled settles whose inputs did not change by @tignear in https://github.com/celox-sim/celox/pull/1811
* perf(runtime)!: compiled slots for designs without process kernels by @tignear in https://github.com/celox-sim/celox/pull/1815
* fix(deps): update cranelift crates to 0.137.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1833
* fix(sv): accept real-valued module parameter overrides by @tignear in https://github.com/celox-sim/celox/pull/1839
* perf(sv): skip scope discovery for literal query operands by @tignear in https://github.com/celox-sim/celox/pull/1843
* feat(sv): run always @* calls of timed tasks as processes and accept package delays by @tignear in https://github.com/celox-sim/celox/pull/1816
* fix(sv): report %t in the smallest time precision of the design by @tignear in https://github.com/celox-sim/celox/pull/1825
* feat(sv): support soft packed unions by @tignear in https://github.com/celox-sim/celox/pull/1837
* fix(sv): lex a colon before a comment as a colon by @tignear in https://github.com/celox-sim/celox/pull/1851
* refactor(runtime): generate the edges of derived clock domains as SIR by @tignear in https://github.com/celox-sim/celox/pull/1844
* fix(sv): join based-literal digits supplied by a macro by @tignear in https://github.com/celox-sim/celox/pull/1852
* fix(sv): bind unpacked array values in module parameter arguments by @tignear in https://github.com/celox-sim/celox/pull/1857
* chore(deps): update rust toolchain to 5b9f119 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1868
* perf(sv): publish only written elements of nonblocking array writes by @tignear in https://github.com/celox-sim/celox/pull/1869
* test(vtest): accelerate shared cases and report CI timings by @tignear in https://github.com/celox-sim/celox/pull/1867
* fix(sir-opt): keep the And width in the bit-extract peephole by @tignear in https://github.com/celox-sim/celox/pull/1887
* perf(runtime)!: run the derived clock edges of ticks in generated units by @tignear in https://github.com/celox-sim/celox/pull/1888
* fix(sv): keep other processes' writes made before an edge's NBA region by @tignear in https://github.com/celox-sim/celox/pull/1884
* feat(sv): support class enum randomization by @tignear in https://github.com/celox-sim/celox/pull/1838
* feat(sv)!: support scope enum randomization by @tignear in https://github.com/celox-sim/celox/pull/1847
* ci: move compilation scaling checks to CodSpeed by @tignear in https://github.com/celox-sim/celox/pull/1907
* chore(deps): update github/codeql-action digest to 9b32540 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1911
* perf(sv): skip the always_comb alternative nothing can watch by @tignear in https://github.com/celox-sim/celox/pull/1859
* feat(sv): support tagged packed unions by @tignear in https://github.com/celox-sim/celox/pull/1883
* fix(sim): judge derived clocks after several nonblocking updates by @tignear in https://github.com/celox-sim/celox/pull/1901
* ci: reuse cocotb builds and warm CI dependency caches by @tignear in https://github.com/celox-sim/celox/pull/1916
* fix(native): preserve array boundaries and store ordering in merged optimization by @tignear in https://github.com/celox-sim/celox/pull/1913
* ci: remove Lydite bridge from routine validation by @tignear in https://github.com/celox-sim/celox/pull/1924
* perf(runtime): defer settles in compiled slots until their results are read by @tignear in https://github.com/celox-sim/celox/pull/1923
* chore(deps): update rust crate memmap2 to v0.9.12 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1925
* feat(runtime)!: run program kernels in the reactive region set by @tignear in https://github.com/celox-sim/celox/pull/1896
* ci: measure compilation benchmarks on CodSpeed walltime runners by @tignear in https://github.com/celox-sim/celox/pull/1930
* feat(sim)!: show the regions of a time slot to a slot observer by @tignear in https://github.com/celox-sim/celox/pull/1934
* feat(sv): simulate program blocks by @tignear in https://github.com/celox-sim/celox/pull/1897
* feat(sim)!: report severity, location and assertion of severity tasks by @tignear in https://github.com/celox-sim/celox/pull/1875
* test: add xezim as an external reference simulator by @tignear in https://github.com/celox-sim/celox/pull/1918
* fix(sv): forward packed pattern parameters as integral values by @tignear in https://github.com/celox-sim/celox/pull/1914
* fix(sv): accept undriven nets with high impedance defaults by @tignear in https://github.com/celox-sim/celox/pull/1915
* feat(sv): support hierarchical references by @tignear in https://github.com/celox-sim/celox/pull/1893
* feat(sv): declare implicit nets for continuous assignment targets by @tignear in https://github.com/celox-sim/celox/pull/1952
* perf(sv): analyze each scope in time proportional to its own size by @tignear in https://github.com/celox-sim/celox/pull/1957
* feat(sv)!: support bounded class randc randomization by @tignear in https://github.com/celox-sim/celox/pull/1959
* feat(sim): fail runs on reports and drain runtime events in TypeScript by @tignear in https://github.com/celox-sim/celox/pull/1876
* test(sim): check coincident ticks of a commit-raised gate against Icarus by @tignear in https://github.com/celox-sim/celox/pull/1936
* feat(sim): build a timed Simulation from a native image by @tignear in https://github.com/celox-sim/celox/pull/1926
* fix(sv): keep the bit ranges a child reads from its input ports by @tignear in https://github.com/celox-sim/celox/pull/1960
* feat(runtime): run slots of designs with derived clocks in the compiled scheduler by @tignear in https://github.com/celox-sim/celox/pull/1967
* fix(sv)!: defer the actions of deferred assertions in processes by @tignear in https://github.com/celox-sim/celox/pull/1877
* chore(deps): update napi-rs and emnapi compatibility by @renovate[bot] in https://github.com/celox-sim/celox/pull/1966
* chore(deps): update rust toolchain to fad80eb by @renovate[bot] in https://github.com/celox-sim/celox/pull/1974
* chore(deps): update updatecli/updatecli-action action to v3.10.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1978
* feat(sv): support non-ANSI module port declarations by @tignear in https://github.com/celox-sim/celox/pull/1954
* fix(native): retain FF publication from other lane tasks by @tignear in https://github.com/celox-sim/celox/pull/1976
* fix(sir-opt): preserve live operands of shared bit-extract shifts by @tignear in https://github.com/celox-sim/celox/pull/1981
* feat(sv): accept loop temporaries shared by plain edge always blocks by @tignear in https://github.com/celox-sim/celox/pull/1950
* chore(deps): update mbx to 1.27.0 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/1985
* feat(sv)!: defer assertions of combinational processes, module items and covers by @tignear in https://github.com/celox-sim/celox/pull/1878
* fix(sv): return typed defaults for invalid unpacked array reads by @tignear in https://github.com/celox-sim/celox/pull/1977
* perf(sv): find multi-store candidates through condensed block components by @tignear in https://github.com/celox-sim/celox/pull/1990
* feat(cli): default celox run to the tiered backend by @tignear in https://github.com/celox-sim/celox/pull/2002
* perf(native): reuse clobber walks across reload queries of one byte range by @tignear in https://github.com/celox-sim/celox/pull/1997
* perf(sir-opt): share one state SSA analysis among sink recipe stability checks by @tignear in https://github.com/celox-sim/celox/pull/2003
* feat(sv): support inout ports with a single tristate driver by @tignear in https://github.com/celox-sim/celox/pull/1951
* build!: make optional simulator backends opt-in by @tignear in https://github.com/celox-sim/celox/pull/1989
* perf(native): deduplicate compile tasks by unit classes instead of deep comparison by @tignear in https://github.com/celox-sim/celox/pull/1995
* perf(sv): instrument each unit once for all probes by @tignear in https://github.com/celox-sim/celox/pull/2000
* perf(sv)!: run initial loops without a static trip count as a process by @tignear in https://github.com/celox-sim/celox/pull/2014
* feat(cli): save state files at chosen cycles or times of a SystemVerilog run by @tignear in https://github.com/celox-sim/celox/pull/2030
* perf(sv): publish only the written elements of an array another process writes by @tignear in https://github.com/celox-sim/celox/pull/2024
* perf(runtime): share the interpreter's combinational program across clocks by @tignear in https://github.com/celox-sim/celox/pull/2009
* fix(native): bound the memory of memoized reload clobber walks by @tignear in https://github.com/celox-sim/celox/pull/2037
* perf(runtime): snapshot only a group's own trigger addresses in the interpreter by @tignear in https://github.com/celox-sim/celox/pull/2018
* perf(runtime): share the combinational program between the ticks of many clocks by @tignear in https://github.com/celox-sim/celox/pull/2034
* perf(sv): find the drivers of a module in one walk of its processes by @tignear in https://github.com/celox-sim/celox/pull/2043
* perf(native): find a stack slot for a spill home from per-row occupancy bits by @tignear in https://github.com/celox-sim/celox/pull/2046
* perf(sv): elaborate generate scopes once per module specialization by @tignear in https://github.com/celox-sim/celox/pull/2051
* perf(native): share anticipated sets and index frozen next-use rows finely by @tignear in https://github.com/celox-sim/celox/pull/2054
* perf(runtime): handle element-strided unpacked arrays per element by @tignear in https://github.com/celox-sim/celox/pull/2048
* perf(sv): follow transparent copies to sensitivity probes through an index by @tignear in https://github.com/celox-sim/celox/pull/2057
* fix(sv-tests): execute preprocessing cases instead of skipping them by @tignear in https://github.com/celox-sim/celox/pull/2059
* fix(native): version reload snapshots per byte range instead of walking clobbers by @tignear in https://github.com/celox-sim/celox/pull/2058
* perf(sv): fold constants without big-integer parsing or needless copies by @tignear in https://github.com/celox-sim/celox/pull/2066
* fix(sv): reject pragmas without a name by @tignear in https://github.com/celox-sim/celox/pull/2061
* fix(sv-tests): match the dashboard main-test corpus by @tignear in https://github.com/celox-sim/celox/pull/2067
* feat(cli): pass plusargs to SystemVerilog runs by @tignear in https://github.com/celox-sim/celox/pull/2077
* fix(sv)!: enforce class member access visibility by @tignear in https://github.com/celox-sim/celox/pull/2074
* fix(sv)!: settle two-state sensitivity-list processes at time zero, like Verilator by @tignear in https://github.com/celox-sim/celox/pull/2078
* fix(sv): validate enum initializer declarations by @tignear in https://github.com/celox-sim/celox/pull/2075
* fix(sv)!: read heap strings in formatted scans by @tignear in https://github.com/celox-sim/celox/pull/2076
* chore(deps): update cachix/install-nix-action digest to 75c75c3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2091
* feat(sv)!: support parameterized classes by @tignear in https://github.com/celox-sim/celox/pull/1796
* fix(sv): bind derived classes in their own declaration scopes by @tignear in https://github.com/celox-sim/celox/pull/2093
* feat(sv)!: support shallow copies of class objects by @tignear in https://github.com/celox-sim/celox/pull/1810
* feat(sv): support classes declared in packages and the compilation unit by @tignear in https://github.com/celox-sim/celox/pull/1850
* feat(sv): let processes of several instances write a package variable by @tignear in https://github.com/celox-sim/celox/pull/1872
* revert: find stack slots by a scan of the slots again by @tignear in https://github.com/celox-sim/celox/pull/2085
* perf(sv): run a combinational process unit once per schedule by @tignear in https://github.com/celox-sim/celox/pull/2082
* fix(frontend): give clocks wired through vector bits the domain of their source by @tignear in https://github.com/celox-sim/celox/pull/2073
* fix(sv): preserve wide constant-function return values by @tignear in https://github.com/celox-sim/celox/pull/2101
* perf(sv): simulate sensitivity lists that cover their reads like always @* by @tignear in https://github.com/celox-sim/celox/pull/2087
* fix(sv): keep task arguments read before a suspending call by @tignear in https://github.com/celox-sim/celox/pull/2089
* fix(sv): defer interface-dependent declarations before binding array ports by @tignear in https://github.com/celox-sim/celox/pull/2107
* feat(runtime)!: opt-in partitioned combinational settle by @tignear in https://github.com/celox-sim/celox/pull/2106
* feat(sv)!: execute final procedures at simulation termination by @tignear in https://github.com/celox-sim/celox/pull/2088
* feat(vpi): run SV processes in cocotb images by @tignear in https://github.com/celox-sim/celox/pull/1938
* perf(runtime): mark settle partitions only when their inputs change by @tignear in https://github.com/celox-sim/celox/pull/2113
* feat(sv)!: execute fork branches as dynamic process activations by @tignear in https://github.com/celox-sim/celox/pull/2112
* feat(sv)!: support wait fork and disable fork process control by @tignear in https://github.com/celox-sim/celox/pull/2116
* chore(deps): update rust toolchain to 0d05759 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2122
* perf(runtime): run every partition of a busy settle without comparing by @tignear in https://github.com/celox-sim/celox/pull/2124
* fix(sv): coalesce repeated direct net aliases by @tignear in https://github.com/celox-sim/celox/pull/2127
* feat(runtime)!: separate core snapshots from host effect state by @tignear in https://github.com/celox-sim/celox/pull/2121
* feat(sv): support $sampled in processes by @tignear in https://github.com/celox-sim/celox/pull/1879
* feat(sv): support $past and the value change functions in clocked processes by @tignear in https://github.com/celox-sim/celox/pull/1880
* fix(sv)!: give reports the time they are issued at by @tignear in https://github.com/celox-sim/celox/pull/1881
* feat(sv): extend sampled values and inferred history clocks by @tignear in https://github.com/celox-sim/celox/pull/1882
* fix(sv): parse array method with expressions by @tignear in https://github.com/celox-sim/celox/pull/2141
* fix(sv): resolve upward references to interface members by @tignear in https://github.com/celox-sim/celox/pull/2143
* feat(runtime)!: store dynamic containers in the simulator heap by @tignear in https://github.com/celox-sim/celox/pull/2148
* feat(cli)!: checkpoint SystemVerilog runs on SIGUSR1 by @tignear in https://github.com/celox-sim/celox/pull/2134
* feat(sv)!: support dynamic arrays queues and associative arrays by @tignear in https://github.com/celox-sim/celox/pull/2149
* feat(sv): allow detached forks in functions and final procedures by @tignear in https://github.com/celox-sim/celox/pull/2146
* fix(frontend): refine bit dependencies through chained copies by @tignear in https://github.com/celox-sim/celox/pull/2158
* fix(sv): evaluate wide constant function values by @tignear in https://github.com/celox-sim/celox/pull/2161
* feat(runtime)!: persist dynamic fork activations in portable state files by @tignear in https://github.com/celox-sim/celox/pull/2152
* feat(sv)!: evaluate compound events on fork-shared automatic locals by @tignear in https://github.com/celox-sim/celox/pull/2153
* feat(aplite): extract an independently versioned finite solver by @tignear in https://github.com/celox-sim/celox/pull/2167
* feat(aplite): add reproducible seeded scalar model search by @tignear in https://github.com/celox-sim/celox/pull/2168
* test(aplite): keep exhaustive sampling as a small-domain oracle by @tignear in https://github.com/celox-sim/celox/pull/2170
* feat(aplite): reuse compiled queries with temporary assumptions by @tignear in https://github.com/celox-sim/celox/pull/2175
* perf(sv): avoid redundant scope discovery and constant evaluation by @tignear in https://github.com/celox-sim/celox/pull/2171
* chore(deps): update actions/setup-node action to v7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2181
* chore(deps): update dependency typedoc-plugin-markdown to v4.14.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2182
* fix(sv): allow bare zero-argument container methods by @tignear in https://github.com/celox-sim/celox/pull/2178
* feat(aplite): query bounded projected XOR cells by @tignear in https://github.com/celox-sim/celox/pull/2177
* perf(aplite): reduce affine XOR rows before bit blasting by @tignear in https://github.com/celox-sim/celox/pull/2183
* feat(aplite): generate approximate projected sample batches by @tignear in https://github.com/celox-sim/celox/pull/2184
* ci(mergify): add an emergency merge request label by @tignear in https://github.com/celox-sim/celox/pull/2211
* feat(sv-syntax)!: retain named constraint expressions by @tignear in https://github.com/celox-sim/celox/pull/2193
* test(sv-tests): classify failures and summarize diagnostics by @tignear in https://github.com/celox-sim/celox/pull/2196
* fix(sv): preserve disjoint single-bit array driver windows by @tignear in https://github.com/celox-sim/celox/pull/2188
* fix(sv): bound recursive specialization along instance paths by @tignear in https://github.com/celox-sim/celox/pull/2189
* perf(vpi): compile process-capable images without retrying by @tignear in https://github.com/celox-sim/celox/pull/2190
* chore(deps): update zstd-sys to fix the wasm allocator advisory by @tignear in https://github.com/celox-sim/celox/pull/2200
* ci(mergify): add queue priority without interrupting checks by @tignear in https://github.com/celox-sim/celox/pull/2214
* feat(sv)!: solve scalar class constraints during randomize by @tignear in https://github.com/celox-sim/celox/pull/2194
* feat(sv)!: support unindexed interface array value parameters by @tignear in https://github.com/celox-sim/celox/pull/2204
* perf(x86): validate wide values at mutation boundaries by @tignear in https://github.com/celox-sim/celox/pull/2205
* ci: coalesce outdated pull request title checks by @tignear in https://github.com/celox-sim/celox/pull/2217
* feat(sv): support logical implication expressions by @tignear in https://github.com/celox-sim/celox/pull/2201
* ci(aplite): avoid product builds for release metadata updates by @tignear in https://github.com/celox-sim/celox/pull/2208
* fix(test-suite): avoid rebuilding for absent Cargo configuration by @tignear in https://github.com/celox-sim/celox/pull/2218
* test(xezim): update xezim digest to 44f5c88 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2195
* test(xezim): update xezim digest to 8082de3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2227
* feat(sv): isolate deferred assertions in fork processes by @tignear in https://github.com/celox-sim/celox/pull/2154
* ci: let required title checks finish independently by @tignear in https://github.com/celox-sim/celox/pull/2246
* feat(sv): support logical equivalence expressions by @tignear in https://github.com/celox-sim/celox/pull/2207
* feat(sv)!: support integral array reduction methods by @tignear in https://github.com/celox-sim/celox/pull/2174
* perf(sir-opt): verify coupled rewrites once per batch by @tignear in https://github.com/celox-sim/celox/pull/2229
* perf(sv): avoid copying published discovery prefixes by @tignear in https://github.com/celox-sim/celox/pull/2230
* test(xezim): update xezim digest to 8082de3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2243
* feat(sv): support scalar set membership constraints by @tignear in https://github.com/celox-sim/celox/pull/2238
* feat(sv): solve conditional class constraints bidirectionally by @tignear in https://github.com/celox-sim/celox/pull/2239
* chore(deps): update rust toolchain to c7e9d51 by @renovate[bot] in https://github.com/celox-sim/celox/pull/2253
* feat(sv): support per-call inline class constraints by @tignear in https://github.com/celox-sim/celox/pull/2240
* feat(heap): accept per-call scalar constraint inputs by @tignear in https://github.com/celox-sim/celox/pull/2250
* feat(sv): bind caller scalar inputs in inline constraints by @tignear in https://github.com/celox-sim/celox/pull/2251
* ci: protect Rust test caches from release target churn by @tignear in https://github.com/celox-sim/celox/pull/2262
* refactor(native): support state-aware outlined kernel calls by @tignear in https://github.com/celox-sim/celox/pull/2231
* perf(native): share large comb kernels and bound compilation memory by @tignear in https://github.com/celox-sim/celox/pull/2232
* fix(vpi): expose interface parameters and lexical generate scopes by @tignear in https://github.com/celox-sim/celox/pull/2233
* perf(settle): bound estimated partitions and preserve sparse updates by @tignear in https://github.com/celox-sim/celox/pull/2234
* ci: qualify release-branch candidates outside the master queue by @tignear in https://github.com/celox-sim/celox/pull/2265
* chore(deps): update mbx to 1.28.0 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/2264
* fix(sv): preserve X/Z in two-state exact comparisons by @tignear in https://github.com/celox-sim/celox/pull/2244
* feat(sv)!: support dynamic array and queue ordering by @tignear in https://github.com/celox-sim/celox/pull/2272
* fix(sv): preserve fixed selector bits in case coverage by @tignear in https://github.com/celox-sim/celox/pull/2275
* perf(sv): evaluate stateless constant functions once by @tignear in https://github.com/celox-sim/celox/pull/2235
* chore(master): release aplite 0.1.0 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/2180


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.11.0...v0.12.0

## 0.11.0 (2026-10-14)

## What's Changed
* chore(deps): update napi-rs and emnapi compatibility by @renovate[bot] in https://github.com/celox-sim/celox/pull/1005
* fix(ts-gen): emit type-checkable declarations for multi-module files and nested blocks by @tignear in https://github.com/celox-sim/celox/pull/1010
* chore(deps): update rust toolchain to dbc715a by @renovate[bot] in https://github.com/celox-sim/celox/pull/1011
* feat(sv): support a synthesizable SystemVerilog subset by @tignear in https://github.com/celox-sim/celox/pull/1009
* ci: type-check test files and playground by @tignear in https://github.com/celox-sim/celox/pull/1012
* chore(deps): update rust toolchain to v1.99.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1014
* fix(sv): share tie-off constants across instance array elements; mark single-element arrays as indexed by @tignear in https://github.com/celox-sim/celox/pull/1017
* feat(test-suite): describe Veryl suite cases as scripts and verify them as SV testbenches by @tignear in https://github.com/celox-sim/celox/pull/1015
* fix(api): return Veryl syntax errors instead of panicking by @tignear in https://github.com/celox-sim/celox/pull/1019
* fix(sv): point unsupported diagnostics at their tracking issues by @tignear in https://github.com/celox-sim/celox/pull/1020
* fix(playground): drop non-public evalComb from 4-state example by @tignear in https://github.com/celox-sim/celox/pull/1024
* docs: match CLAUDE.md project fields to the GitHub project by @tignear in https://github.com/celox-sim/celox/pull/1028
* fix(ts): evaluate pending combinational logic in fourState() by @tignear in https://github.com/celox-sim/celox/pull/1025
* fix(veryl): classify unsupported diagnostics and implement the missing lowerings by @tignear in https://github.com/celox-sim/celox/pull/1018
* feat(test-suite): rename to celox-test-suite and verify the SystemVerilog tests with Verilator and Icarus by @tignear in https://github.com/celox-sim/celox/pull/1029
* feat(api): checkpoint and restore simulation state by @tignear in https://github.com/celox-sim/celox/pull/1023
* feat(lydite): import hwverify as the Lydite refinement checker by @tignear in https://github.com/celox-sim/celox/pull/1031
* chore(deps): update rust crate parol to v5.0.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1035
* chore(deps): update rust crate parol_runtime to v5.0.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1036
* fix(deps): update rust crate scnr2 to v0.5.3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1038
* chore(deps): update dependency @vscode/test-electron to v3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1039
* fix(deps): update dependency vscode-languageclient to v10 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1041
* fix(lydite): identify suite cases by their script text by @tignear in https://github.com/celox-sim/celox/pull/1043
* chore(deps): update dependency ubuntu to v26 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1040
* feat(sv): lower procedural SystemVerilog and complete Veryl-emitted SV support by @tignear in https://github.com/celox-sim/celox/pull/1045
* ci: run Rust tests with nextest and optimize dev dependencies by @tignear in https://github.com/celox-sim/celox/pull/1047
* chore(deps): update pnpm to v12.8.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1049
* chore(deps): update rust toolchain to e600293 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1051
* fix(veryl): update veryl crates to v0.22.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1054
* chore(deps): update pnpm to v12.8.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1056
* chore(deps): update rust crate libc to v0.2.190 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1057
* fix(deps): update rust crate wasm-encoder to 0.261 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1059
* fix(veryl): keep unfolded constant values and recompute constant case targets by @tignear in https://github.com/celox-sim/celox/pull/1052
* test(sv-suite): convert the remaining SystemVerilog tests to scripts by @tignear in https://github.com/celox-sim/celox/pull/1037
* fix(sv): keep the signedness of elements of signed named types by @tignear in https://github.com/celox-sim/celox/pull/1044
* chore(deps): update rust crate insta to v1.49.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1065
* fix: ignore out-of-range dynamic writes and read X on every lowering path by @tignear in https://github.com/celox-sim/celox/pull/1050
* feat(simulator): add multi-threaded lane-partitioned simulation by @tignear in https://github.com/celox-sim/celox/pull/1033
* feat(sv): check system function calls against a catalog where they are converted by @tignear in https://github.com/celox-sim/celox/pull/1069
* fix(sv): lower runtime indices into multidimensional packed arrays by @tignear in https://github.com/celox-sim/celox/pull/1046
* chore(deps): update rust toolchain to b962e5d by @renovate[bot] in https://github.com/celox-sim/celox/pull/1071
* ci(bench): measure multi-threaded Celox in the Heliodor suite by @tignear in https://github.com/celox-sim/celox/pull/1072
* refactor(sir): classify instruction effects in one place by @tignear in https://github.com/celox-sim/celox/pull/1074
* build(nix): provide Z3 in the development environment by @tignear in https://github.com/celox-sim/celox/pull/1076
* chore(deps): update dependency lefthook to v2.1.15 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1078
* chore(deps): update pnpm to v12.8.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1087
* fix(sv): record comb path bit boundaries so disjoint feedback is not a loop by @tignear in https://github.com/celox-sim/celox/pull/1081
* feat(sv): call DPI-C imports from always_ff on every backend by @tignear in https://github.com/celox-sim/celox/pull/1080
* fix(sv): evaluate `&&` right operands for unknown left operands and bound constant evaluation by @tignear in https://github.com/celox-sim/celox/pull/1082
* chore(deps): update rust toolchain to 8afee9f by @renovate[bot] in https://github.com/celox-sim/celox/pull/1096
* feat(test-suite): add incremental verification and daily full CI by @tignear in https://github.com/celox-sim/celox/pull/1094
* chore(deps): update vitest monorepo to v5.0.3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1099
* fix(veryl): lower interface members captured by modport-imported functions by @tignear in https://github.com/celox-sim/celox/pull/1090
* fix(veryl): reject generic interface ports instead of failing on unknown connections by @tignear in https://github.com/celox-sim/celox/pull/1091
* test(veryl): broaden interface coverage in the shared suite by @tignear in https://github.com/celox-sim/celox/pull/1093
* chore(deps): update dependency @biomejs/biome to v2.5.15 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1100
* fix(sir): keep identity-alias homes consistent for lane-partitioned kernels by @tignear in https://github.com/celox-sim/celox/pull/1098
* perf(native): save only live registers across extern calls and share extern ABI lowering by @tignear in https://github.com/celox-sim/celox/pull/1103
* ci: report full CI failures and gate release merge groups on external suites by @tignear in https://github.com/celox-sim/celox/pull/1105
* fix(sv): nested default patterns, int parameter sign, loop bound width, and $size dimensions by @tignear in https://github.com/celox-sim/celox/pull/1083
* fix(sv): widen branchless mux arms and reject incompatible unpacked arrays by @tignear in https://github.com/celox-sim/celox/pull/1084
* test: record why each SystemVerilog arm is ignored and drop stale ignores by @tignear in https://github.com/celox-sim/celox/pull/1085
* perf(sv-analyzer): share scope metadata and reuse parsed sources by @tignear in https://github.com/celox-sim/celox/pull/1107
* chore(deps): update updatecli/updatecli-action action to v3.9.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1114
* chore(deps): update rust toolchain to f91010a by @renovate[bot] in https://github.com/celox-sim/celox/pull/1113
* chore(deps): update dependency vite to v8.3.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1117
* feat(sv): elaborate interfaces by flattening them into modules by @tignear in https://github.com/celox-sim/celox/pull/1108
* chore(deps): update napi-rs and emnapi compatibility by @renovate[bot] in https://github.com/celox-sim/celox/pull/1116
* ci: run the sv-tests elaboration tests against the SystemVerilog frontend by @tignear in https://github.com/celox-sim/celox/pull/1119
* ci: validate each change once instead of three times by @tignear in https://github.com/celox-sim/celox/pull/1109
* chore(deps): update dependency lefthook to v2.1.16 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1129
* chore(deps): update dependency @types/node to v25.9.9 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1130
* chore(deps): update dependency @napi-rs/cli to v3.10.6 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1126
* perf(sv-analyzer): reuse scope types for bits and size queries by @tignear in https://github.com/celox-sim/celox/pull/1112
* feat(runtime)!: compile delayed processes into resumable kernels by @tignear in https://github.com/celox-sim/celox/pull/1127
* feat(sv): support variable declaration initializers by @tignear in https://github.com/celox-sim/celox/pull/1124
* fix(deps): update rust crate syn to v3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1042
* feat(sv)!: run-time loop events and calls in select indices by @tignear in https://github.com/celox-sim/celox/pull/1128
* fix(sv): keep run-time inner bit selects inside their unpacked element by @tignear in https://github.com/celox-sim/celox/pull/1133
* chore(deps): update rust toolchain to 4ca2963 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1137
* chore(deps): update napi-rs and emnapi compatibility by @renovate[bot] in https://github.com/celox-sim/celox/pull/1139
* ci(lydite): do not rerun the gates after merging into master by @tignear in https://github.com/celox-sim/celox/pull/1141
* fix(sv): count writes through called subroutines as process drivers by @tignear in https://github.com/celox-sim/celox/pull/1135
* ci(bench): move measurements to daily runs by @tignear in https://github.com/celox-sim/celox/pull/1142
* test(sv): run sv-tests simulation tests and check their assertions by @tignear in https://github.com/celox-sim/celox/pull/1136
* chore(test-suite): keep run metadata out of retained reports by @tignear in https://github.com/celox-sim/celox/pull/1140
* perf(sv-analyzer): reuse parameter environments and scope snapshots by @tignear in https://github.com/celox-sim/celox/pull/1144
* feat(sv): run initial blocks as process kernels from time zero by @tignear in https://github.com/celox-sim/celox/pull/1138
* fix(sv): reject package variables and nets by @tignear in https://github.com/celox-sim/celox/pull/1148
* refactor(sv-analyzer): collect declarations from modules and packages alike by @tignear in https://github.com/celox-sim/celox/pull/1149
* fix(sv): evaluate select indices at their self-determined width by @tignear in https://github.com/celox-sim/celox/pull/1134
* ci: run only the Rust tests a pull request can affect by @tignear in https://github.com/celox-sim/celox/pull/1147
* fix(sv): size display arguments as IEEE 1800-2023 specifies by @tignear in https://github.com/celox-sim/celox/pull/1143
* fix(veryl): format display arguments as Veryl's simulator does by @tignear in https://github.com/celox-sim/celox/pull/1145
* ci: run CodSpeed daily and report regressions by @tignear in https://github.com/celox-sim/celox/pull/1155
* feat(sv)!: resolve packages as scopes instead of inlining them by @tignear in https://github.com/celox-sim/celox/pull/1151
* feat(sv): implement $countbits system function by @tignear in https://github.com/celox-sim/celox/pull/1153
* chore(deps): update pnpm to v12.9.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1162
* perf(test-suite): reuse local verification and stabilize proof identities by @tignear in https://github.com/celox-sim/celox/pull/1156
* perf(sv-analyzer): eliminate quadratic generate-scope work by @tignear in https://github.com/celox-sim/celox/pull/1150
* test: fix the Veryl reference adapter's resets and refresh Veryl exclusions by @tignear in https://github.com/celox-sim/celox/pull/1152
* perf(sv-analyzer): reuse parameter prefix and range environments by @tignear in https://github.com/celox-sim/celox/pull/1166
* fix(runtime): return errors and fatal events without panicking by @tignear in https://github.com/celox-sim/celox/pull/1154
* fix(sim): preserve sign bits in partial-word arithmetic shifts by @tignear in https://github.com/celox-sim/celox/pull/1164
* perf(sv-analyzer): look up constant-folding types on demand by @tignear in https://github.com/celox-sim/celox/pull/1168
* feat(sv)!: share package variables between modules by @tignear in https://github.com/celox-sim/celox/pull/1167
* fix(sv): handle system function and readmem edge cases by @tignear in https://github.com/celox-sim/celox/pull/1157
* chore(deps): update rust toolchain to e90fafa by @renovate[bot] in https://github.com/celox-sim/celox/pull/1172
* perf(sv-analyzer): avoid repeated four-state environment copies by @tignear in https://github.com/celox-sim/celox/pull/1170
* fix(sv): preserve declared dimensions in array queries by @tignear in https://github.com/celox-sim/celox/pull/1169
* perf(sv-analyzer): retain resolved parameter literals by @tignear in https://github.com/celox-sim/celox/pull/1174
* fix(test-suite): split retained verification reports by group by @tignear in https://github.com/celox-sim/celox/pull/1177
* feat(sv): support package export declarations by @tignear in https://github.com/celox-sim/celox/pull/1178
* fix(sv): give static procedural locals a static lifetime by @tignear in https://github.com/celox-sim/celox/pull/1179
* feat(sv): resolve compilation-unit declarations as the $unit scope by @tignear in https://github.com/celox-sim/celox/pull/1180
* perf(sv-analyzer): borrow constant function environments by @tignear in https://github.com/celox-sim/celox/pull/1175
* perf(sv-analyzer): avoid cloning scalar call contexts by @tignear in https://github.com/celox-sim/celox/pull/1182
* feat(sv): support $isunbounded parameter queries by @tignear in https://github.com/celox-sim/celox/pull/1176
* chore(deps): update pnpm to v12.9.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1195
* chore(deps): update rust crate toml to v1.1.7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1196
* perf(sv-analyzer): materialize referenced parameter values by @tignear in https://github.com/celox-sim/celox/pull/1193
* perf(native): place state and lane code in access order by @tignear in https://github.com/celox-sim/celox/pull/1197
* ci(bench): keep one benchmark series per runner CPU model by @tignear in https://github.com/celox-sim/celox/pull/1199
* fix(native): never match a Perm row to the reserved state-base register by @tignear in https://github.com/celox-sim/celox/pull/1200
* feat(sv)!: accept constant part-selects as array indices by @tignear in https://github.com/celox-sim/celox/pull/1201
* chore(deps): update rust toolchain to c1cfa7a by @renovate[bot] in https://github.com/celox-sim/celox/pull/1203
* perf(sir): publish lane FF next state directly when no later unit needs it by @tignear in https://github.com/celox-sim/celox/pull/1205
* feat(sv): implement $timeunit and $timeprecision by @tignear in https://github.com/celox-sim/celox/pull/1183
* fix(sv): scope imports in generate blocks to their block by @tignear in https://github.com/celox-sim/celox/pull/1184
* fix(sv): let loop genvars shadow outer parameters by @tignear in https://github.com/celox-sim/celox/pull/1207
* perf(sv-analyzer): skip RTL-only literals during subroutine discovery by @tignear in https://github.com/celox-sim/celox/pull/1209
* fix(sv)!: preserve nested array index selections by @tignear in https://github.com/celox-sim/celox/pull/1198
* feat(sv): implement $printtimescale by @tignear in https://github.com/celox-sim/celox/pull/1210
* feat(frontend): support bidirectional SV and Veryl interfaces by @tignear in https://github.com/celox-sim/celox/pull/1204
* feat(simulator): coincident clock edges, DSE-safe signal resolution and a Rica benchmark by @tignear in https://github.com/celox-sim/celox/pull/1212
* fix(sv): preserve unpacked dimensions across scopes by @tignear in https://github.com/celox-sim/celox/pull/1171
* fix(deps): update rust crate object to 0.40 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1225
* feat(sv)!: support packed unions, scoped locals and named calls by @tignear in https://github.com/celox-sim/celox/pull/1211
* fix(vcd): retain external signals across waveform restarts by @tignear in https://github.com/celox-sim/celox/pull/1227
* fix(js): bound scalar accesses to simulation memory by @tignear in https://github.com/celox-sim/celox/pull/1228
* chore(deps): update rust crate toml to v1.1.8 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1229
* fix(native): normalize unknown wildcard comparison results by @tignear in https://github.com/celox-sim/celox/pull/1230
* fix(sv): preserve eager Veryl assertion message effects by @tignear in https://github.com/celox-sim/celox/pull/1231
* fix(sv): retain parameter part-selects in constant initializers by @tignear in https://github.com/celox-sim/celox/pull/1214
* fix(js): dispose factory handles when construction fails by @tignear in https://github.com/celox-sim/celox/pull/1232
* fix(api): truncate scalar writes to the signal width by @tignear in https://github.com/celox-sim/celox/pull/1240
* chore(deps): update dependency lefthook to v2.1.17 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1244
* chore(deps): update napi-rs and emnapi compatibility by @renovate[bot] in https://github.com/celox-sim/celox/pull/1234
* fix(simulation): preserve aliased and cascaded clock edges by @tignear in https://github.com/celox-sim/celox/pull/1243
* chore(deps): update rust toolchain to 404b7a5 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1246
* chore(deps): update dependency @napi-rs/cli to v3.10.7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1247
* fix(sv): consume time arguments and format packed strings by @tignear in https://github.com/celox-sim/celox/pull/1241
* fix(api)!: preserve HDL output as bytes by @tignear in https://github.com/celox-sim/celox/pull/1245
* fix(vite): refresh generated data when project configuration changes by @tignear in https://github.com/celox-sim/celox/pull/1233
* fix(sv): treat unknown casex selector bits as wildcards by @tignear in https://github.com/celox-sim/celox/pull/1238
* fix(deps): update rust crate syn to v3.0.7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1253
* chore(deps): update dependency vite to v8.3.3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1254
* chore(deps): update pnpm to v12.10.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1255
* fix(docs): keep CPU legends outside benchmark plots by @tignear in https://github.com/celox-sim/celox/pull/1256
* chore(deps): update rust toolchain to 3100cf6 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1257
* chore(deps): update dependency @napi-rs/cli to v3.10.8 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1258
* chore(deps): update rust crate parol to v5.0.3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1261
* fix(ci): restore scheduled Heliodor benchmark measurements by @tignear in https://github.com/celox-sim/celox/pull/1263
* fix(simulation): preserve clock periods and monotonic time by @tignear in https://github.com/celox-sim/celox/pull/1235
* fix(sv): mask unresolved imports and preserve parameter short circuits by @tignear in https://github.com/celox-sim/celox/pull/1251
* fix(sv): extend nested operands before evaluating expressions by @tignear in https://github.com/celox-sim/celox/pull/1236
* fix(slt): preserve concatenated constants during inlining by @tignear in https://github.com/celox-sim/celox/pull/1264
* feat(debug): visualize parallel scheduler decisions and waits by @tignear in https://github.com/celox-sim/celox/pull/1259
* perf(scheduler): publish cross-lane results at finer task boundaries by @tignear in https://github.com/celox-sim/celox/pull/1260
* chore(deps): update rust crate serde_json to v1.0.152 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1265
* chore(deps): update pnpm to v12.10.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1267
* fix(sv): honor display field alignment and zero padding by @tignear in https://github.com/celox-sim/celox/pull/1266
* chore(deps): update rust crate proptest to v1.12.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1268
* fix(vite): track source and project changes in module graphs by @tignear in https://github.com/celox-sim/celox/pull/1237
* fix(ci): gate merges on ARM64 backend checks by @tignear in https://github.com/celox-sim/celox/pull/1269
* feat(visualizer): compare scheduler estimates and focus task dependencies by @tignear in https://github.com/celox-sim/celox/pull/1270
* fix(runtime): align simulation storage and lane boundaries to 256 bytes by @tignear in https://github.com/celox-sim/celox/pull/1271
* fix(vite): remove orphan declarations after startup generation by @tignear in https://github.com/celox-sim/celox/pull/1239
* chore(deps): update rust toolchain to 8111d2b by @renovate[bot] in https://github.com/celox-sim/celox/pull/1275
* perf(parallel): track dependencies by bit and loop large sparse commits by @tignear in https://github.com/celox-sim/celox/pull/1277
* chore(deps): update rust crate serde_json to v1.0.153 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1278
* chore(deps): update rust crate serde_json to v1.0.154 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1279
* fix(release): update lydite lockfile versions in release PRs by @tignear in https://github.com/celox-sim/celox/pull/1281
* fix(deps): update rust crate syn to v3.0.8 by @renovate[bot] in https://github.com/celox-sim/celox/pull/1280


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.10.1...v0.11.0

## 0.10.1 (2026-10-04)

## What's Changed
* fix(release): reject unsafe package includes in CI by @tignear in https://github.com/celox-sim/celox/pull/1002


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.10.0...v0.10.1

## 0.10.0 (2026-10-03)

## What's Changed
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/739
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/740
* fix(veryl): roll HEAD to 36172f6ac944 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/741
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/744
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/745
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/749
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/752
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/754
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/756
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/757
* fix(veryl): roll HEAD to 082f3f56475f by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/746
* fix(veryl): roll HEAD to 10c4d89a548d by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/758
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/761
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/763
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/765
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/768
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/769
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/773
* fix(veryl): roll HEAD to 6df50ce79ec3 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/774
* fix(veryl): roll HEAD to 4d821887fcd1 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/775
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/778
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/781
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/782
* feat(veryl)!: support hierarchical readmemh on Veryl HEAD by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/776
* fix(veryl): roll HEAD to 06eaa3c9f53c by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/784
* fix(veryl): roll HEAD to d6d47fe0b286 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/785
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/786
* fix(veryl): roll HEAD to a6ef6af4621b by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/788
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/789
* fix(veryl): roll HEAD to 5e5a136b2201 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/791
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/792
* fix(veryl): roll HEAD to 59e6c50d9ec4 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/793
* fix(veryl): roll HEAD to afe81dc8d168 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/794
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/796
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/801
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/805
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/807
* fix(veryl): roll HEAD to 3945e9db61a8 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/803
* fix(veryl): roll HEAD to fea54da98ac2 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/809
* fix(veryl): roll HEAD to 63db6f87fa30 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/814
* fix(veryl): roll HEAD to 86e4bdaa355e by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/821
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/810
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/828
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/829
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/832
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/836
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/837
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/839
* fix(veryl): roll HEAD to e115c907ccc9 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/830
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/841
* fix(veryl): roll HEAD to ee754466ffad by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/843
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/844
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/846
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/849
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/852
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/853
* test: restore passing regressions with develop Veryl by @tignear in https://github.com/celox-sim/celox/pull/855
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/858
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/860
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/861
* fix(veryl): roll HEAD to 2bd44cba2eae by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/863
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/864
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/867
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/872
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/875
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/878
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/882
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/883
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/885
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/887
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/888
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/893
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/895
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/898
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/900
* fix(veryl): roll HEAD to 7b269d0fc6b1 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/891
* fix(veryl): roll HEAD to d1f7025898b9 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/905
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/906
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/913
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/923
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/925
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/926
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/927
* test(veryl): refresh inout cases and backend exclusions by @tignear in https://github.com/celox-sim/celox/pull/929
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/933
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/934
* fix(veryl): roll HEAD to 3799a119518a by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/928
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/937
* fix(veryl): roll HEAD to 46ffa40d940b by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/938
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/944
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/947
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/951
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/956
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/957
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/961
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/968
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/972
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/980
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/983
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/984
* fix(veryl)!: upgrade to Veryl 0.22.0 by @tignear in https://github.com/celox-sim/celox/pull/985
* fix(veryl): support hierarchical testbench assignments by @tignear in https://github.com/celox-sim/celox/pull/989
* chore(deps): update rust toolchain to 7470661 by @renovate[bot] in https://github.com/celox-sim/celox/pull/990
* test(veryl): restore 0.22 coverage and audit exclusions by @tignear in https://github.com/celox-sim/celox/pull/992
* perf(x86): lower wide runtime shifts with a barrel network by @tignear in https://github.com/celox-sim/celox/pull/995
* feat(testbench)!: support concurrent Veryl initial processes by @tignear in https://github.com/celox-sim/celox/pull/993
* perf(compiler): reduce retained IR and analysis memory by @tignear in https://github.com/celox-sim/celox/pull/996
* fix(deps): update rust crate wasm-encoder to 0.260 by @renovate[bot] in https://github.com/celox-sim/celox/pull/999


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.9.1...v0.10.0

## 0.9.1 (2026-10-02)

## What's Changed
* perf(sir): avoid repeated full scans in sparse-case discovery by @tignear in https://github.com/celox-sim/celox/pull/978
* perf(x86): avoid redundant ready-candidate invalidation by @tignear in https://github.com/celox-sim/celox/pull/981
* perf(sir): batch disjoint sparse-case rewrites by @tignear in https://github.com/celox-sim/celox/pull/982


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.9.0...v0.9.1

## 0.9.0 (2026-10-02)

## What's Changed
* fix(ci): reserve build time in Heliodor suite timeout by @tignear in https://github.com/celox-sim/celox/pull/935
* fix(sim): preserve four-state values through casts and concatenation by @tignear in https://github.com/celox-sim/celox/pull/939
* feat(sv): support bit vector predicate functions by @tignear in https://github.com/celox-sim/celox/pull/940
* fix(bench): unify Heliodor revision and publish successful runs by @tignear in https://github.com/celox-sim/celox/pull/943
* chore(deps): update pnpm to v12.6.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/946
* chore(deps): update dependency @napi-rs/cli to v3.10.5 by @renovate[bot] in https://github.com/celox-sim/celox/pull/945
* fix(release): publish created tags after batched merges by @tignear in https://github.com/celox-sim/celox/pull/948
* fix(bench): order Heliodor comparisons by measurement time by @tignear in https://github.com/celox-sim/celox/pull/949
* fix(wasm): preserve independent chunks in wide slices by @tignear in https://github.com/celox-sim/celox/pull/941
* feat(sv)!: support packed structs and member access by @tignear in https://github.com/celox-sim/celox/pull/942
* test(veryl): extend reusable suite with upstream behavioral cases by @tignear in https://github.com/celox-sim/celox/pull/952
* fix(ci): eliminate duplicate Rust tests and parallelize binding builds by @tignear in https://github.com/celox-sim/celox/pull/950
* ci: queue benchmark runs while preserving latest PR priority by @tignear in https://github.com/celox-sim/celox/pull/954
* ci(napi): validate publication sources and restrict cache writes by @tignear in https://github.com/celox-sim/celox/pull/955
* fix(simulator): preserve wide shift counts and packed constant storage by @tignear in https://github.com/celox-sim/celox/pull/953
* chore(deps): update updatecli/updatecli-action action to v3.8.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/958
* feat(test-suite): tag expectations beyond SystemVerilog requirements by @tignear in https://github.com/celox-sim/celox/pull/959
* chore(deps): update dependency vite to v8.3.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/962
* feat(bench): cache native Heliodor compilations by @tignear in https://github.com/celox-sim/celox/pull/960
* fix(veryl): preserve contextual widths and signed member reads by @tignear in https://github.com/celox-sim/celox/pull/966
* chore(deps): update rust crate blake3 to v1.8.7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/969
* chore(deps): update dependency monaco-editor to ^0.57.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/971
* perf(sir): simplify guarded scans and packed array updates by @tignear in https://github.com/celox-sim/celox/pull/970
* chore(deps): update vitest monorepo to v5.0.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/973
* chore(deps): update pnpm to v12.7.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/975
* fix(sir): preserve shared values across switch arms by @tignear in https://github.com/celox-sim/celox/pull/974
* fix(x86): keep Windows JIT state valid across preemption by @tignear in https://github.com/celox-sim/celox/pull/976
* feat(veryl): warn on side effects in array default and repeat items by @tignear in https://github.com/celox-sim/celox/pull/967
* chore(deps): update rust to v1.99.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/963
* feat(sv): support constant indexed selections emitted by Veryl by @tignear in https://github.com/celox-sim/celox/pull/964
* fix(deps): keep Rust and Nix overlay updates aligned by @tignear in https://github.com/celox-sim/celox/pull/977


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.8.2...v0.9.0

## 0.8.2 (2026-09-28)

## What's Changed
* fix(veryl): preserve signedness of $bits and $size by @tignear in https://github.com/celox-sim/celox/pull/930
* fix(frontend): refine cyclic connections at bit boundaries by @tignear in https://github.com/celox-sim/celox/pull/931


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.8.1...v0.8.2

## 0.8.1 (2026-09-27)

## What's Changed
* chore(deps): update rust crate thiserror to v2.0.21 by @renovate[bot] in https://github.com/celox-sim/celox/pull/908
* refactor(sv-analyzer): extract function analysis and tests by @tignear in https://github.com/celox-sim/celox/pull/909
* fix(ci): detect missing crates.io bootstrap before publishing by @tignear in https://github.com/celox-sim/celox/pull/911
* test(veryl): separate constant folding and zero-initialization checks by @tignear in https://github.com/celox-sim/celox/pull/917
* refactor(x86): split instruction selection by responsibility by @tignear in https://github.com/celox-sim/celox/pull/914
* fix(ci): validate stacked PRs in the target merge queue by @tignear in https://github.com/celox-sim/celox/pull/924
* refactor(x86): split MIR optimization passes by responsibility by @tignear in https://github.com/celox-sim/celox/pull/915
* refactor(x86): split machine code emission by responsibility by @tignear in https://github.com/celox-sim/celox/pull/916
* refactor(sv): split AST construction by responsibility by @tignear in https://github.com/celox-sim/celox/pull/918
* refactor(arm64): split instruction selection by responsibility by @tignear in https://github.com/celox-sim/celox/pull/919
* refactor(sir): split mux branchification by responsibility by @tignear in https://github.com/celox-sim/celox/pull/920
* refactor(slt): split symbolic tree lowering by responsibility by @tignear in https://github.com/celox-sim/celox/pull/922


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.8.0...v0.8.1

## 0.8.0 (2026-09-26)

## What's Changed
* fix(deps): update cranelift crates to 0.136.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/890
* chore(deps): update dependency @napi-rs/cli to v3.10.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/889
* chore(deps): update dependency typedoc-plugin-markdown to v4.13.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/894
* chore(deps): update pnpm to v12.5.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/897
* chore(deps): update dependency @types/node to v25.9.8 by @renovate[bot] in https://github.com/celox-sim/celox/pull/899
* feat(test-suite)!: add reusable Veryl tests and fix simulator semantics by @tignear in https://github.com/celox-sim/celox/pull/902
* chore(deps): update rust crate test-case to v3.4.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/903
* fix(ci): limit Heliodor suite concurrency by @tignear in https://github.com/celox-sim/celox/pull/904
* chore(deps): update napi-rs and emnapi compatibility by @renovate[bot] in https://github.com/celox-sim/celox/pull/896
* feat(sv): support $countones in expressions by @tignear in https://github.com/celox-sim/celox/pull/901
* fix(ci): catch Rust documentation failures before publication by @tignear in https://github.com/celox-sim/celox/pull/907


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.7.1...v0.8.0

## 0.7.1 (2026-09-24)

## What's Changed
* fix(veryl): distinguish value-generic module specializations by @tignear in https://github.com/celox-sim/celox/pull/884


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.7.0...v0.7.1

## 0.7.0 (2026-09-23)

## What's Changed
* perf(native): reduce large-design compile and execution overhead by @tignear in https://github.com/celox-sim/celox/pull/825
* perf(compiler): reduce compile overhead and tighten x86 lowering by @tignear in https://github.com/celox-sim/celox/pull/827
* chore(deps): update pnpm to v12.4.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/831
* chore(deps): update pnpm to v12.4.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/833
* chore(deps): update updatecli/updatecli-action action to v3.7.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/835
* chore(deps): update rust crate clap to v4.6.7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/834
* chore(deps): update dependency ubuntu to v26 by @renovate[bot] in https://github.com/celox-sim/celox/pull/838
* chore(deps): update napi-rs and emnapi compatibility by @renovate[bot] in https://github.com/celox-sim/celox/pull/797
* perf(compiler)!: reduce tiered startup and group benchmark comparisons by @tignear in https://github.com/celox-sim/celox/pull/840
* fix(dev): unify Nix and devcontainer environments with mbx by @tignear in https://github.com/celox-sim/celox/pull/842
* fix(nix): select pnpm from package.json with Corepack by @tignear in https://github.com/celox-sim/celox/pull/845
* chore(deps): update rust crate syn to v3.0.6 by @renovate[bot] in https://github.com/celox-sim/celox/pull/847
* build(nix): use mbx from shared nix-packages flake by @tignear in https://github.com/celox-sim/celox/pull/848
* chore(deps): update rust crate codspeed-criterion-compat to v5.0.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/851
* test: re-enable passing backend regression tests by @tignear in https://github.com/celox-sim/celox/pull/850
* fix(deps): update vitest monorepo to v5 by @renovate[bot] in https://github.com/celox-sim/celox/pull/806
* fix(bench): migrate benchmarks to Vitest 5 by @tignear in https://github.com/celox-sim/celox/pull/856
* feat(sv-frontend): lower enums and always_comb control flow by @tignear in https://github.com/celox-sim/celox/pull/662
* fix(sim): resolve known mismatches in four-state equality by @tignear in https://github.com/celox-sim/celox/pull/857
* feat(sv): expand generate construct support by @tignear in https://github.com/celox-sim/celox/pull/859
* fix(deps): update rust crate wasmtime to v49 by @renovate[bot] in https://github.com/celox-sim/celox/pull/862
* chore(deps): update pnpm to v12.4.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/865
* chore(deps): update dependency @tybys/wasm-util to v0.10.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/869
* chore(deps): update dependency @biomejs/biome to v2.5.13 by @renovate[bot] in https://github.com/celox-sim/celox/pull/868
* chore(deps): update dependency @types/node to v25.9.7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/870
* chore(deps): update dependency lefthook to v2.1.14 by @renovate[bot] in https://github.com/celox-sim/celox/pull/871
* chore(deps): update dependency typedoc-vitepress-theme to v1.1.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/873
* chore(deps): update vitest monorepo to v5.0.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/874
* chore(deps): update dependency @playwright/test to v1.63.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/876
* chore(deps): update dependency vite to v8.3.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/877
* chore(deps): update dependency @biomejs/biome to v2.5.14 by @renovate[bot] in https://github.com/celox-sim/celox/pull/881
* chore(deps): update rust crate rand to v0.10.3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/879
* chore(deps): update napi-rs and emnapi compatibility by @renovate[bot] in https://github.com/celox-sim/celox/pull/866
* build(nix): use mold for native development builds by @tignear in https://github.com/celox-sim/celox/pull/880


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.6.0...v0.7.0

## 0.6.0 (2026-09-14)

## What's Changed
* chore(deps): update pnpm to v12.3.3 by @renovate[bot] in https://github.com/celox-sim/celox/pull/815
* fix(compiler)!: reduce resource use for large Heliodor benchmarks by @tignear in https://github.com/celox-sim/celox/pull/817
* chore(deps): update pnpm to v12.3.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/818
* fix(ci): prevent duplicate npm platform publication by @tignear in https://github.com/celox-sim/celox/pull/820
* fix(compiler): reduce resource use for large Linux workloads by @tignear in https://github.com/celox-sim/celox/pull/819
* fix(compiler): correct large Linux boot and reduce compiler memory by @tignear in https://github.com/celox-sim/celox/pull/822
* fix(deps): update rust crate wasm-encoder to 0.259 by @renovate[bot] in https://github.com/celox-sim/celox/pull/823
* chore(deps): update rust crate toml to v1.1.6 by @renovate[bot] in https://github.com/celox-sim/celox/pull/824


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.5.0...v0.6.0

## 0.5.0 (2026-09-11)

## What's Changed
* chore(deps): update pnpm to v12.3.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/798
* perf(x86): reduce MIR optimization work and emitted code size by @tignear in https://github.com/celox-sim/celox/pull/804
* chore(deps): update pnpm to v12.3.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/802
* perf(vcd)!: track writes and specialize waveform output by @tignear in https://github.com/celox-sim/celox/pull/799
* fix(bench): restore standard Veryl execution for Heliodor by @tignear in https://github.com/celox-sim/celox/pull/808
* chore(deps): update pnpm to v12.3.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/812
* fix(bench): enable expanded Heliodor Linux benchmarks without false fold cycles by @tignear in https://github.com/celox-sim/celox/pull/813


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.4.8...v0.5.0

## 0.4.8 (2026-09-09)

## What's Changed
* chore(deps): update pnpm to v12.2.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/771
* chore(deps): update rust crate syn to v3.0.5 by @renovate[bot] in https://github.com/celox-sim/celox/pull/770
* fix(bench): measure Veryl-CC tiered Linux boot by @tignear in https://github.com/celox-sim/celox/pull/777
* fix(bench): accept tiered completion before C-code adoption by @tignear in https://github.com/celox-sim/celox/pull/780
* perf(arm64): reduce packed state and spill overhead by @tignear in https://github.com/celox-sim/celox/pull/779
* perf(arm64): reduce loop scans, spill loads, and state copy overhead by @tignear in https://github.com/celox-sim/celox/pull/783
* fix(simulator): type top-level parameter overrides before elaboration by @tignear in https://github.com/celox-sim/celox/pull/787
* fix(veryl): preserve function copyout types and four-state values by @tignear in https://github.com/celox-sim/celox/pull/790


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.4.7...v0.4.8

## 0.4.7 (2026-09-07)

## What's Changed
* chore(deps): update rust toolchain to v1.98.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/759
* fix(sir-opt): preserve memory order when splitting wide stores by @tignear in https://github.com/celox-sim/celox/pull/762
* perf(x86): accelerate Heliodor native execution by @tignear in https://github.com/celox-sim/celox/pull/764
* Revert "perf(x86): accelerate Heliodor native execution" by @tignear in https://github.com/celox-sim/celox/pull/766
* perf(x86): restore Heliodor speedups with padded array fixes by @tignear in https://github.com/celox-sim/celox/pull/767


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.4.6...v0.4.7

## 0.4.6 (2026-09-06)

## What's Changed
* chore(deps): update dependency lefthook to v2.1.12 by @renovate[bot] in https://github.com/celox-sim/celox/pull/747
* chore(deps): update pnpm to v11.25.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/751
* chore(deps): update rust crate toml to v1.1.5 by @renovate[bot] in https://github.com/celox-sim/celox/pull/750
* chore(deps): update pnpm to v12 by @renovate[bot] in https://github.com/celox-sim/celox/pull/753
* fix(sim): correct registered memory writes and reduce FF staging by @tignear in https://github.com/celox-sim/celox/pull/755


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.4.5...v0.4.6

## 0.4.5 (2026-09-04)

## What's Changed
* fix(veryl): roll HEAD to ee523841cb19 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/497
* fix(veryl): roll HEAD to ee523841cb19 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/500
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/507
* fix(veryl): roll HEAD to ee523841cb19 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/505
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/511
* fix(veryl): roll HEAD to ee523841cb19 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/513
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/519
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/523
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/527
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/529
* fix(veryl): roll HEAD to 46772906ecd8 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/532
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/533
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/534
* fix(veryl): roll HEAD to a3d5f25139f9 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/535
* fix(veryl): roll HEAD to e0746ec2ccdb by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/538
* fix(veryl): roll HEAD to 3226bad8ee56 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/539
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/542
* fix(veryl): roll HEAD to 6cb4b2228cd7 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/543
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/545
* fix(veryl): roll HEAD to 9e585f2b1de9 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/546
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/548
* fix(veryl): roll HEAD to 70f69ff93f34 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/549
* fix(veryl): roll HEAD to 9b4a104dafdb by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/550
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/552
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/554
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/557
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/558
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/562
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/563
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/566
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/567
* fix(veryl): roll HEAD to 3f5ca9c756f1 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/553
* fix(veryl): roll HEAD to 867deb152cf6 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/568
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/570
* fix(veryl): roll HEAD to f8f96d7a5512 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/571
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/573
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/575
* fix(veryl): roll HEAD to 01c9ce851966 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/576
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/577
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/583
* fix(veryl): roll HEAD to 35dca1f2b55d by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/599
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/601
* fix(veryl): roll HEAD to 3ec0967752db by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/604
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/605
* fix(veryl): roll HEAD to 4004f1c3860e by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/606
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/607
* fix(veryl): roll HEAD to bcaa99d33faa by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/608
* fix(veryl): roll HEAD to 1dffff1f9137 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/610
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/609
* fix(veryl): roll HEAD to f1ccdf82b1d3 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/612
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/613
* fix(veryl): roll HEAD to 495ced99187a by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/614
* fix(veryl): roll HEAD to 90a8f38240cb by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/616
* fix(veryl): roll HEAD to 13e9215863f3 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/622
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/618
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/625
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/629
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/630
* fix(veryl): roll HEAD to e97b88c03f41 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/631
* fix(veryl): roll HEAD to 3c8d5cfe3a58 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/632
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/633
* fix(veryl): roll HEAD to cab4176b94c0 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/634
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/638
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/639
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/643
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/646
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/648
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/650
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/652
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/654
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/658
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/660
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/661
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/665
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/667
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/670
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/672
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/673
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/677
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/679
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/683
* fix(veryl): roll HEAD to 0783385d7cf4 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/674
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/684
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/688
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/691
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/693
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/694
* fix(veryl): roll HEAD to 667701d13073 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/685
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/698
* fix(veryl): roll HEAD to 1b99b1fdc6a9 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/696
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/701
* fix(veryl): roll HEAD to 248d548c60ab by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/703
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/704
* fix(wasm): use lhs width for wide sar sign fill by @tignear in https://github.com/celox-sim/celox/pull/706
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/707
* fix(veryl): restore coverage and typed array arguments by @tignear in https://github.com/celox-sim/celox/pull/709
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/710
* fix(x86): preserve regalloc pressure for reload recipes by @tignear in https://github.com/celox-sim/celox/pull/712
* fix(veryl): roll HEAD to e3ab328e2ce5 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/711
* fix(veryl): roll HEAD to dbd45f50159a by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/713
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/714
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/717
* chore(deps): update pnpm to v11.24.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/728
* fix(napi): preserve shared memory view lifetime by @tignear in https://github.com/celox-sim/celox/pull/731
* fix(ci): discard incomplete Criterion cache by @tignear in https://github.com/celox-sim/celox/pull/732
* fix(veryl): roll HEAD to 4d7fb7f07c02 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/730
* fix(veryl): roll HEAD to 1917c9d884b4 by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/733
* fix(deps): update compatible Python and cocotb versions by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/727
* chore(deps): update dependency typedoc-plugin-markdown to v4.13.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/734
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/719
* chore(develop): sync master by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/735
* fix(veryl): roll HEAD to 2e32d53f056e by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/737
* fix(optimizer): preserve oversized concat slice runs by @tignear in https://github.com/celox-sim/celox/pull/736
* fix(veryl): update to 0.21.0 by @tignear in https://github.com/celox-sim/celox/pull/738
* chore(deps): update rust to v1.98.1 by @renovate[bot] in https://github.com/celox-sim/celox/pull/742


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.4.4...v0.4.5

## 0.4.4 (2026-08-31)

## What's Changed
* ci(napi): cache native build artifacts by @tignear in https://github.com/celox-sim/celox/pull/720
* perf(arm64): separate target instruction selection by @tignear in https://github.com/celox-sim/celox/pull/722
* fix(vite): invalidate generated types for every source change by @tignear in https://github.com/celox-sim/celox/pull/725
* fix(celox): harden simulation API boundaries by @tignear in https://github.com/celox-sim/celox/pull/723
* fix(celox): preserve simulation API contracts by @tignear in https://github.com/celox-sim/celox/pull/724
* chore(deps): update pnpm to v11.23.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/726


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.4.3...v0.4.4

## 0.4.3 (2026-08-29)

## What's Changed
* chore(deps): update dependency @biomejs/biome to v2.5.10 by @renovate[bot] in https://github.com/celox-sim/celox/pull/715
* fix(x86): preserve regalloc pressure for reload recipes by @tignear in https://github.com/celox-sim/celox/pull/718


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.4.2...v0.4.3

## 0.4.2 (2026-08-27)

## What's Changed
* chore(deps): update compatible Python and cocotb versions by @celox-bot[bot] in https://github.com/celox-sim/celox/pull/675
* feat(backend): tiered execution — interpret first, promote to JIT by @tignear in https://github.com/celox-sim/celox/pull/668
* chore(deps): update dependency @biomejs/biome to v2.5.9 by @renovate[bot] in https://github.com/celox-sim/celox/pull/681
* chore(deps): update vitest monorepo to v4.1.11 by @renovate[bot] in https://github.com/celox-sim/celox/pull/682
* feat(backend): tier promotion policy and Node tiered bindings by @tignear in https://github.com/celox-sim/celox/pull/680
* perf(tiered): add deterministic execution measurements by @tignear in https://github.com/celox-sim/celox/pull/686
* perf(interpreter): reuse tier-zero register storage by @tignear in https://github.com/celox-sim/celox/pull/687
* chore(deps): update rust crate syn to v3.0.4 by @renovate[bot] in https://github.com/celox-sim/celox/pull/690
* chore(deps): update dependency vite to v8.2.2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/692
* perf(interpreter): pack reusable register slots by @tignear in https://github.com/celox-sim/celox/pull/689
* fix(deps): centralize napi workspace versions by @tignear in https://github.com/celox-sim/celox/pull/695
* perf(interpreter): add u64 two-state fast path by @tignear in https://github.com/celox-sim/celox/pull/699
* fix(interpreter): handle 64-bit concat operands by @tignear in https://github.com/celox-sim/celox/pull/700
* perf(bench): benchmark Heliodor with tiered JIT by @tignear in https://github.com/celox-sim/celox/pull/697
* fix(docs): sync benchmark tabs with URL by @tignear in https://github.com/celox-sim/celox/pull/702
* perf(interpreter): reuse prepared register storage by @tignear in https://github.com/celox-sim/celox/pull/705
* fix(deps): update rust crate wasm-encoder to 0.258 by @renovate[bot] in https://github.com/celox-sim/celox/pull/708


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.4.1...v0.4.2

## 0.4.1 (2026-08-24)

## What's Changed
* perf(x86): eliminate dead loads and cut mir_opt compile time by @tignear in https://github.com/celox-sim/celox/pull/663
* feat(backend): add tier-0 SIR interpreter for tiered execution by @tignear in https://github.com/celox-sim/celox/pull/655
* fix(deps): update cranelift crates to 0.135.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/669
* chore(deps): update rust toolchain to v1.98.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/671


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.4.0...v0.4.1

## 0.4.0 (2026-08-22)

## What's Changed
* fix(release): avoid crates publish disk exhaustion by @tignear in https://github.com/celox-sim/celox/pull/635
* feat(backend)!: enable aarch64 natively by default by @tignear in https://github.com/celox-sim/celox/pull/636
* chore(deps): update ghcr.io/devcontainers/features/node docker tag to v2 by @renovate[bot] in https://github.com/celox-sim/celox/pull/640
* fix(ci): disable pnpm lifecycle scripts by @tignear in https://github.com/celox-sim/celox/pull/641
* refactor(arm64): lower SIR into target-owned MIR by @tignear in https://github.com/celox-sim/celox/pull/642
* feat(frontend-sv): support fixed unpacked arrays by @tignear in https://github.com/celox-sim/celox/pull/619
* refactor(runtime)!: normalize runtime design metadata by @tignear in https://github.com/celox-sim/celox/pull/645
* fix(release)!: preserve breaking pull request bumps by @tignear in https://github.com/celox-sim/celox/pull/644
* fix(ci): restore pull request merge checks by @tignear in https://github.com/celox-sim/celox/pull/651
* perf(arm64): optimize target MIR before register allocation by @tignear in https://github.com/celox-sim/celox/pull/649
* perf(arm64): eliminate identity MIR operations by @tignear in https://github.com/celox-sim/celox/pull/653
* fix(deps): update rust crate wasm-encoder to 0.257 by @renovate[bot] in https://github.com/celox-sim/celox/pull/656
* chore(deps): update pnpm to v11.22.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/657
* ci: gate npm publishing behind the npm environment by @tignear in https://github.com/celox-sim/celox/pull/659


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.3.1...v0.4.0

## 0.3.1 (2026-08-21)

## What's Changed
* chore(deps): update napi-rs and emnapi compatibility by @renovate[bot] in https://github.com/celox-sim/celox/pull/611
* feat(test): run Veryl output through SV frontend by @tignear in https://github.com/celox-sim/celox/pull/615
* feat(frontend): add external frontend SDK by @tignear in https://github.com/celox-sim/celox/pull/620
* perf(arm64): improve native code generation by @tignear in https://github.com/celox-sim/celox/pull/621
* feat(rust): publish crates and add frontend adapters by @tignear in https://github.com/celox-sim/celox/pull/623
* fix(release): correct Cargo.lock version selector by @tignear in https://github.com/celox-sim/celox/pull/624
* fix(release): automate trusted publisher bootstrap by @tignear in https://github.com/celox-sim/celox/pull/626
* fix(deps): update rust crate wasmtime to v48 by @renovate[bot] in https://github.com/celox-sim/celox/pull/628
* chore(deps): update rust to v1.98.0 by @renovate[bot] in https://github.com/celox-sim/celox/pull/627


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.3.0...v0.3.1

## 0.3.0 (2026-08-19)

## What's Changed
* feat(sv): support case statements in always_ff by @tignear in https://github.com/celox-sim/celox/pull/403
* fix(docs): categorize benchmark charts by architecture by @tignear in https://github.com/celox-sim/celox/pull/597
* chore(deps): coordinate Python and cocotb updates by @tignear in https://github.com/celox-sim/celox/pull/595
* fix(ci): preserve develop sync conflict resolutions by @tignear in https://github.com/celox-sim/celox/pull/596
* feat(veryl): support runtime power exponents by @tignear in https://github.com/celox-sim/celox/pull/600
* feat(sv): add experimental SystemVerilog frontend by @tignear in https://github.com/celox-sim/celox/pull/398
* refactor(frontend)!: split source-specific frontend crates by @tignear in https://github.com/celox-sim/celox/pull/602
* fix(x86): preserve nested output-array forwarding by @tignear in https://github.com/celox-sim/celox/pull/603


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.2.2...v0.3.0

## 0.2.2 (2026-08-18)

## What's Changed
* fix(release): generate changelog from pull requests by @tignear in https://github.com/celox-sim/celox/pull/584
* fix(ci): allow ARM64 Cranelift Linux boot to finish by @tignear in https://github.com/celox-sim/celox/pull/587
* feat(cli): add VPI build and cocotb workflow by @tignear in https://github.com/celox-sim/celox/pull/586
* perf(slt): reuse scheduler graph relations by @tignear in https://github.com/celox-sim/celox/pull/585
* chore(deps): update actions/setup-python action to v7 by @renovate[bot] in https://github.com/celox-sim/celox/pull/590
* chore(deps): update dependency @biomejs/biome to v2.5.8 by @renovate[bot] in https://github.com/celox-sim/celox/pull/593
* fix(napi): preserve unpacked array port layout by @tignear in https://github.com/celox-sim/celox/pull/592
* fix(playground): prevent browser freeze during run by @tignear in https://github.com/celox-sim/celox/pull/591
* perf(slt): bound shared region slice lowering by @tignear in https://github.com/celox-sim/celox/pull/594


**Full Changelog**: https://github.com/celox-sim/celox/compare/v0.2.1...v0.2.2

## [0.2.1](https://github.com/celox-sim/celox/compare/v0.2.0...v0.2.1) (2026-08-18)


### Features

* **vpi:** run cocotb on native program images ([c84f7c5](https://github.com/celox-sim/celox/commit/c84f7c54f61a4d777b2b4742ac1ad67f42590eb3))


### Bug Fixes

* **ci:** address release queue review ([3f02d10](https://github.com/celox-sim/celox/commit/3f02d10b4f431470665878d3ca9bbc35acc2438e))
* **ci:** dequeue held release by pull request ([cc37747](https://github.com/celox-sim/celox/commit/cc37747cd999f26f23dc8038dcaf44d3a09e0a4e))
* **ci:** verify release merge queue entry ([7ce179f](https://github.com/celox-sim/celox/commit/7ce179f7107f1c4c19aaf1a6cdc711380972185e))
* **ci:** verify release merge queue entry ([fcffaf4](https://github.com/celox-sim/celox/commit/fcffaf4b806b7e1454105ddc5b7ee37bb53685af))
* **frontend:** scope top parameter overrides ([5efe12e](https://github.com/celox-sim/celox/commit/5efe12e9a091f92dc3fde9abf226e21bc2d33de3))
* **frontend:** scope top parameter overrides ([a114336](https://github.com/celox-sim/celox/commit/a1143368f681af6a51f6de27e777742dc477caef))
* **vpi:** address portable runtime edge cases ([6b6d54c](https://github.com/celox-sim/celox/commit/6b6d54c34420b63cc6b27b4babcdbbbed4583b73))
* **vpi:** enforce native runtime boundaries ([dae4cca](https://github.com/celox-sim/celox/commit/dae4cca20103c7a0208628584d08ef43169d60dd))
* **vpi:** harden runtime event handling ([660c0f1](https://github.com/celox-sim/celox/commit/660c0f1020833a15255df90af187a70f3ca72d03))
* **vpi:** honor callback and image requirements ([f92cfdf](https://github.com/celox-sim/celox/commit/f92cfdf12db6ea06011dd4de2427337ccd22ffc9))
* **vpi:** make force writes transactional ([441d60d](https://github.com/celox-sim/celox/commit/441d60decec34a4c4cbdcc2b6f89594a826dc7ad))
* **vpi:** own callback records and transitions ([47fd9b9](https://github.com/celox-sim/celox/commit/47fd9b901ba62950aee13e5063a24fba102417be))
* **vpi:** preserve callback and release values ([8f7171f](https://github.com/celox-sim/celox/commit/8f7171f36b8a4f36b02e7f1f1e9df13ad32f3655))
* **vpi:** preserve force and hierarchy semantics ([9e3d279](https://github.com/celox-sim/celox/commit/9e3d27923e22aeb1eff5de149d204be171851298))
* **vpi:** preserve reflected object semantics ([48aaa2c](https://github.com/celox-sim/celox/commit/48aaa2c89b89bf2e2501d23926e6e18ed6c34be9))
* **vpi:** preserve runtime diagnostic metadata ([b8a3a50](https://github.com/celox-sim/celox/commit/b8a3a505b40893134ef22e2a94a9d9973ea9f7ff))
* **vpi:** preserve runtime event ordering ([475f676](https://github.com/celox-sim/celox/commit/475f676e2b4790f7d455c33a1d5058238c1fba72))
* **vpi:** preserve singleton hierarchy names ([92e6980](https://github.com/celox-sim/celox/commit/92e698099fe39547ad036a8cb7bcd452936e2967))
* **vpi:** preserve termination and output semantics ([a80c1df](https://github.com/celox-sim/celox/commit/a80c1dfda41f18f58b21e2e0e233c8a2464290cb))
* **vpi:** render fatal ff events before errors ([647d869](https://github.com/celox-sim/celox/commit/647d8698035e9506cf2de4816ede08e528c2456a))
* **vpi:** retain queued callback writes ([0a8980b](https://github.com/celox-sim/celox/commit/0a8980b497a26992727f246bae096e34f7abef9d))
* **vpi:** settle pre-time callback writes ([438fd14](https://github.com/celox-sim/celox/commit/438fd148d52fc9b40f6156972862c9545a020969))
* **wasm:** preserve array layout metadata ([8754e96](https://github.com/celox-sim/celox/commit/8754e96c6f7e08f401e909caad3153c39cc4d869))
* **wasm:** preserve array layout metadata ([171c62d](https://github.com/celox-sim/celox/commit/171c62dd7be698f6ea41492766dd23b44db18903))


### Performance Improvements

* **frontend:** avoid temporary select buffers ([1dd9afa](https://github.com/celox-sim/celox/commit/1dd9afa5a1b286339160575195c191e65bf2744c))
* **frontend:** borrow symbolic range parts ([286f5aa](https://github.com/celox-sim/celox/commit/286f5aa89e9010049d1ec61d9507add250fc921c))
* **frontend:** build instance symbolic state lazily ([ce9f67f](https://github.com/celox-sim/celox/commit/ce9f67fac2597b581369a56ec1162249782c9eab))
* **frontend:** build select strides in place ([bfdd266](https://github.com/celox-sim/celox/commit/bfdd266940c6f467d65c3cb60c9a97fcfc408eed))
* **frontend:** initialize comb symbolic state lazily ([3983da2](https://github.com/celox-sim/celox/commit/3983da2f470c3d23b262f92e1afbe8dea825a8e0))
* **frontend:** inline common uncovered windows ([f0eacb3](https://github.com/celox-sim/celox/commit/f0eacb3a920ea454f42e70bd5582f681d3e7756d))
* **frontend:** limit comb path extraction to writes ([7a6e6d5](https://github.com/celox-sim/celox/commit/7a6e6d51eb2152b8e19d9279c419d4e7117f1d01))
* **frontend:** merge sparse symbolic ranges linearly ([3a427b4](https://github.com/celox-sim/celox/commit/3a427b4228d99bb026c2071876eaf1d763f56626))
* **frontend:** move single symbolic part sources ([ef9f3cc](https://github.com/celox-sim/celox/commit/ef9f3cc9ee9116d685f5836cf6f44143a93ef060))
* **frontend:** propagate boundaries without clones ([bdbeaeb](https://github.com/celox-sim/celox/commit/bdbeaeb591d1150215345420b5974471d93ced9a))
* **frontend:** reduce symbolic observer scans ([9515311](https://github.com/celox-sim/celox/commit/951531164ae0f6d82926631fb789d3e1fe5860f4))
* **frontend:** reuse validated select geometry ([3520ddf](https://github.com/celox-sim/celox/commit/3520ddf661972786fff76a5b3fdf40be3b3d5ce9))
* **slt:** allocate symbolic pages lazily ([7883640](https://github.com/celox-sim/celox/commit/7883640d8c41239be68039307159b00fb926d82b))
* **slt:** avoid singleton schedule buffers ([1c6e19d](https://github.com/celox-sim/celox/commit/1c6e19dee6e78881f54d0bd2ad308b7b9ae6a70d))
* **slt:** make symbolic construction sparse ([de431b6](https://github.com/celox-sim/celox/commit/de431b6b1b9d3e98649f16e8584d9ff7b43608c1))
* **slt:** reduce scheduler construction overhead ([7ad38be](https://github.com/celox-sim/celox/commit/7ad38bea99902cc9ac61682097e194569c55ffb7))
* **slt:** remove sparse ranges in place ([7d9f050](https://github.com/celox-sim/celox/commit/7d9f0502dfd902c92645e8661bacc64fabd2b408))
* **slt:** reuse lowering cost buffers ([cf309c6](https://github.com/celox-sim/celox/commit/cf309c61fc7ba997e15703f743d239d385cda47f))
* **slt:** share symbolic page tables across forks ([cc525db](https://github.com/celox-sim/celox/commit/cc525db89dfaa649092dc2f3b6635b8479ec9018))
* **slt:** stream sparse symbolic diffs ([2a0642d](https://github.com/celox-sim/celox/commit/2a0642d5c3e7eb6875f6c8c02c84139649ea953c))

## [0.2.0](https://github.com/celox-sim/celox/compare/v0.1.35...v0.2.0) (2026-08-17)


### ⚠ BREAKING CHANGES

* **frontend:** diagnose mutable for loop bounds

### Features

* **arm64:** add bootstrap backend and shared regalloc ([e4c7c50](https://github.com/celox-sim/celox/commit/e4c7c50bbb4eee1a7f9992cb9e4d46060e1b3c57))
* **arm64:** add experimental native backend ([e7e8b2b](https://github.com/celox-sim/celox/commit/e7e8b2bac267923a21c97096a585d6d6f58f81a7))
* **arm64:** gate experimental backend ([82924f2](https://github.com/celox-sim/celox/commit/82924f26aa908e5a425a4116e2e805a02d27f73e))
* **arm64:** integrate native scalar backend ([1c2c67c](https://github.com/celox-sim/celox/commit/1c2c67c60a87f99e15729338f9142528e3de5da7))
* **arm64:** own final register coloring ([655a303](https://github.com/celox-sim/celox/commit/655a3031c9e658ea32749bb014d9b273c85ed6f4))
* **arm64:** own spill insertion ([f54df10](https://github.com/celox-sim/celox/commit/f54df10d114c3d0954302e02b74947ea706f9139))
* **arm64:** verify target allocation facts ([9f138a6](https://github.com/celox-sim/celox/commit/9f138a6e1cc0bda019bbbd665a075db2d8e21fed))
* **frontend:** diagnose mutable for loop bounds ([e4f3af3](https://github.com/celox-sim/celox/commit/e4f3af3c942cfaca61d5d0d805839bbc2c2092da))
* **testbench:** inject TypeScript components ([d19e9da](https://github.com/celox-sim/celox/commit/d19e9dadbb3327116321444ab694886e47f02dcf))
* **testbench:** inject TypeScript components ([af09819](https://github.com/celox-sim/celox/commit/af098193fc92098cd9c5f5d3baeefd821c91a012))
* **testbench:** support Veryl components ([b64208b](https://github.com/celox-sim/celox/commit/b64208b4faeff36bac2a7ab55d13121c6338ec72))
* **testbench:** support Veryl components ([5f48c08](https://github.com/celox-sim/celox/commit/5f48c08717735af64a032e9dde88883253ff93fa))
* **testbench:** support Veryl random methods ([df3052f](https://github.com/celox-sim/celox/commit/df3052f9c72817e1aca579dca7bc65c7a6ad4984))
* **testbench:** support Veryl random methods ([d4704a4](https://github.com/celox-sim/celox/commit/d4704a43c417851885400527b39e4237f998b073))
* **veryl:** support hierarchical testbench reads ([c182952](https://github.com/celox-sim/celox/commit/c18295289fa0a25f2018b28ba13cf1e343fe0b4e))
* **veryl:** support hierarchical testbench reads ([50de288](https://github.com/celox-sim/celox/commit/50de2886b6428393af619d3571fe6312b988a520))
* **vite:** expose injected components to Veryl LSP ([c4725f7](https://github.com/celox-sim/celox/commit/c4725f717f162cdc3ab875d99f5af0b914318891))


### Bug Fixes

* **arm64:** preserve spilled phi copies ([3a54676](https://github.com/celox-sim/celox/commit/3a54676d16df57b4c392f8fac8d1c0df5424bc89))
* **bench:** keep CodSpeed compilation suite opt-in ([f5debc3](https://github.com/celox-sim/celox/commit/f5debc3e0e953f278dc6ffea974fab34f698d3fd))
* **ci:** address benchmark matrix review ([9453945](https://github.com/celox-sim/celox/commit/9453945a6c16da36d53ea0f6748758d7c33f2e14))
* **ci:** address Veryl lane review feedback ([8f7393c](https://github.com/celox-sim/celox/commit/8f7393c54bb471be0cd5a2ef015e402df3a377cc))
* **ci:** allow stable develop transition ([2e3b1ac](https://github.com/celox-sim/celox/commit/2e3b1ac8112b81fe481a7474043becfecc9cee01))
* **ci:** allow stable develop transition ([e5ca907](https://github.com/celox-sim/celox/commit/e5ca9079510f72aaba559bbb16232227878022f4))
* **ci:** build playground before browser tests ([7b14669](https://github.com/celox-sim/celox/commit/7b146697a0d05ddb0e63b6831c8c783fe7e95b41))
* **ci:** guard develop sync branch reset ([272d2eb](https://github.com/celox-sim/celox/commit/272d2eb0b4db9ceb4c55f512a1e749a4859cdfa4))
* **ci:** isolate fixed gate Cargo features ([dd004cf](https://github.com/celox-sim/celox/commit/dd004cfcefadd51f85724dc113629a59b8942ff9))
* **ci:** keep Heliodor host metadata outside checkout ([c00dbee](https://github.com/celox-sim/celox/commit/c00dbee8a64e5caff676abbb1a3f93436a66fd47))
* **ci:** keep Heliodor host metadata outside checkout ([40953bd](https://github.com/celox-sim/celox/commit/40953bd72d3041a850954f164bf942ec0f3cc354))
* **ci:** make Heliodor benchmarks non-blocking ([9933f65](https://github.com/celox-sim/celox/commit/9933f651d79c307248aaa50a3a8a15feec90345e))
* **ci:** make release publication recoverable ([829f547](https://github.com/celox-sim/celox/commit/829f547edafc0fb64b7b3a98dba003732d8b1440))
* **ci:** make release publication recoverable ([3d9622b](https://github.com/celox-sim/celox/commit/3d9622b22eb97860eaf8d789a30e199c64eb7e1a))
* **ci:** preserve nightly and converter coverage ([d10516f](https://github.com/celox-sim/celox/commit/d10516fef3968f6787c0baa8b229e394a27d1ce5))
* **ci:** preserve Veryl promotion overlays ([3850cd2](https://github.com/celox-sim/celox/commit/3850cd27803011174e38436a47473681ad14192f))
* **ci:** reconcile develop lockfile during sync ([d0a57ce](https://github.com/celox-sim/celox/commit/d0a57cea86f47dec9c14d71fe16d18d25d800155))
* **ci:** reconcile develop lockfile during sync ([e08da72](https://github.com/celox-sim/celox/commit/e08da72c3a00128ba623fd217c7755b03ec9fd10))
* **ci:** refine ARM64 Heliodor scheduling ([a06b92b](https://github.com/celox-sim/celox/commit/a06b92b575b149f0c85dd4f08ca4134cf9848ab1))
* **ci:** restore Heliodor on pull requests ([3f8530a](https://github.com/celox-sim/celox/commit/3f8530a80fd42f6a684274562e83e053ab2a3051))
* **ci:** restore Heliodor on pull requests ([7795d8f](https://github.com/celox-sim/celox/commit/7795d8ff924076857e648a3b1dd5e667db7fd116))
* **ci:** skip CodSpeed for merge groups ([2ed7233](https://github.com/celox-sim/celox/commit/2ed7233030e83d7112edca9a80ca28a5f177a0f6))
* **ci:** skip CodSpeed for merge groups ([d688c24](https://github.com/celox-sim/celox/commit/d688c24edba07029594f5524c5ac1eb3419dc8fb))
* **ci:** stop repeated Veryl lockfile rolls ([982b0c8](https://github.com/celox-sim/celox/commit/982b0c8f31126c2cb0073247c2dffc5197dfc236))
* **ci:** stop repeated Veryl lockfile rolls ([70e8432](https://github.com/celox-sim/celox/commit/70e8432cd54d58b5274ce1d2b2f385027cbc57e9))
* **ci:** sync develop from checked-out master ([1dbf0c2](https://github.com/celox-sim/celox/commit/1dbf0c2f1c20e9a1ee6c6e6e6d2981c475828bfc))
* **ci:** use FxHashMap alias in builder ([af71811](https://github.com/celox-sim/celox/commit/af7181171f03e9098c78ebe1b478898c0dbeeb4d))
* **cranelift:** boot Linux correctly ([65c3fd0](https://github.com/celox-sim/celox/commit/65c3fd0069e1a361322e5997214071fcbfceb360))
* **cranelift:** boot Linux correctly ([3bdc7dc](https://github.com/celox-sim/celox/commit/3bdc7dc55f560f4fe53ae56b7fac6feecd3f52a2))
* **deps:** update napi-rs and emnapi compatibility ([2959f19](https://github.com/celox-sim/celox/commit/2959f196bc4eb9d02ab78d6b3cbfe36c34838960))
* **deps:** update napi-rs and emnapi compatibility ([ed36df1](https://github.com/celox-sim/celox/commit/ed36df1222831a743ef1cc5b26a50b707f157406))
* **deps:** update napi-rs and emnapi compatibility ([b9542c6](https://github.com/celox-sim/celox/commit/b9542c601d7ba407ddca876a955a35e21ee9c535))
* **deps:** update napi-rs and emnapi compatibility ([614baba](https://github.com/celox-sim/celox/commit/614baba1adaaefe2e68b5d0662e02d1c8c49d4e9))
* **deps:** update rust crate wasm-encoder to 0.255 ([7bfabb7](https://github.com/celox-sim/celox/commit/7bfabb7de16071ca4f75d62ef9d5e0ef2a66712b))
* **deps:** update rust crate wasm-encoder to 0.255 ([bcae53d](https://github.com/celox-sim/celox/commit/bcae53d0d0ccc28110b6c51a0f9237533c370257))
* **deps:** update rust crate wasm-encoder to 0.256 ([d931980](https://github.com/celox-sim/celox/commit/d931980cdf9b7afbfe8f64d9413b5401cb69ec62))
* **deps:** update rust crate wasm-encoder to 0.256 ([e339949](https://github.com/celox-sim/celox/commit/e3399498f5ab60d51784a1802579f95b1128803e))
* **frontend:** adapt call analysis to Veryl 0.20.3 ([6f369fc](https://github.com/celox-sim/celox/commit/6f369fce547e5499c7ab5640afa40b0c433f4a06))
* **frontend:** address remaining output observer cases ([ab897a9](https://github.com/celox-sim/celox/commit/ab897a948e9c13ac31646075685f8e8bf216b4b1))
* **frontend:** align connection effect previews ([38b227d](https://github.com/celox-sim/celox/commit/38b227d3fbc387f7242365f0c4073ef84ab46368))
* **frontend:** allow outputless comb statement calls ([75f50ed](https://github.com/celox-sim/celox/commit/75f50ed9e95147dd33d1232b0bdcf996a96371d1))
* **frontend:** allow outputless comb statement calls ([0e37b31](https://github.com/celox-sim/celox/commit/0e37b31ca8a2de208e998ca53c3314a708c4a741))
* **frontend:** allow reverse exclusive sentinel ([6cd2572](https://github.com/celox-sim/celox/commit/6cd2572ef83468068168274ad078fc783b956507))
* **frontend:** classify missing function bindings as invalid ([23ef912](https://github.com/celox-sim/celox/commit/23ef912b9334f8a5571a75b126323a615715250f))
* **frontend:** classify missing function bindings as invalid ([fe2a0c0](https://github.com/celox-sim/celox/commit/fe2a0c01635a4f82d58a956135a104bb718136db))
* **frontend:** close remaining loop-width gaps ([eaf938c](https://github.com/celox-sim/celox/commit/eaf938c7820bc4bc03b2766a062f0b3a08b07d16))
* **frontend:** collect nested comb runtime effects ([cd2f5d7](https://github.com/celox-sim/celox/commit/cd2f5d7433fd31affce341d36468a6ad33f4acd1))
* **frontend:** complete ordered effect collection ([c96b5ca](https://github.com/celox-sim/celox/commit/c96b5ca66d0b935613c818616ce7a8db2615e753))
* **frontend:** compose instance connection effects ([872709a](https://github.com/celox-sim/celox/commit/872709ade36e54b1c1940a09592f29efba2ec190))
* **frontend:** cover mutable for-bound edge cases ([7e72d05](https://github.com/celox-sim/celox/commit/7e72d05fc3267f0e981858828932dbded73e4d20))
* **frontend:** enforce i32 bitwise loop steps ([fb15dba](https://github.com/celox-sim/celox/commit/fb15dbaeffc12b5fcfc8c2e9161f9d12128efce4))
* **frontend:** guard ordered comb effects ([61f1b08](https://github.com/celox-sim/celox/commit/61f1b08ea0b9dba15dadb3051ec99e61cfb26d4c))
* **frontend:** harden fixed-width loop progress ([3ad4c4e](https://github.com/celox-sim/celox/commit/3ad4c4e38ff89e8cb77576630995bb59d784f26c))
* **frontend:** harden observer effect collection ([cb37859](https://github.com/celox-sim/celox/commit/cb3785922eb2ab5d106337a0723f5a05118baf07))
* **frontend:** isolate connection observer state ([dfef593](https://github.com/celox-sim/celox/commit/dfef59342556e6ea6f547240ce2944045ede1a81))
* **frontend:** isolate nested observer captures ([ba447cf](https://github.com/celox-sim/celox/commit/ba447cf6d5471b4d57039309dfc6761a9eaa3505))
* **frontend:** lower comb output calls in expressions ([e135f6a](https://github.com/celox-sim/celox/commit/e135f6a1fb377b534826b965e4360cde6eab8dc3))
* **frontend:** lower comb output calls in expressions ([658e9ad](https://github.com/celox-sim/celox/commit/658e9ad4fbf61cbcd2bd94f84d79c0b7fc36d68a))
* **frontend:** lower instance connection outputs ([dd4ae06](https://github.com/celox-sim/celox/commit/dd4ae06ed6dcdfd252a0d90d34eb6d9a67d4dabb))
* **frontend:** lower output connection index effects ([bf5a0d3](https://github.com/celox-sim/celox/commit/bf5a0d34f12c7683fb1bf3fb926ce3734c01ad03))
* **frontend:** preserve bitwise loop step width ([3797d78](https://github.com/celox-sim/celox/commit/3797d78a6ec6931f797f0a00a373d04847531572))
* **frontend:** preserve conditional comb effects ([564d059](https://github.com/celox-sim/celox/commit/564d059124ba8898ddab17ff975c00113985db4c))
* **frontend:** preserve event operand evaluation order ([f98ab94](https://github.com/celox-sim/celox/commit/f98ab943d680b632d58b4bf437d0621e25d07c00))
* **frontend:** preserve fixed-width loop progress ([3ede429](https://github.com/celox-sim/celox/commit/3ede429e43a758baa419a8287b97c123c4bd76c3))
* **frontend:** preserve function loop effect control ([9f0ff8e](https://github.com/celox-sim/celox/commit/9f0ff8ea10237634bdada9ddf89d6399491fa2fa))
* **frontend:** preserve function output contexts ([62841f2](https://github.com/celox-sim/celox/commit/62841f23b47869b3171d54228117cef43190e994))
* **frontend:** preserve function return effect control ([5ba0e71](https://github.com/celox-sim/celox/commit/5ba0e714c0d251f4a8690177dddbe502b147fd2d))
* **frontend:** preserve guarded call evaluation ([1463135](https://github.com/celox-sim/celox/commit/1463135f2d040f482d372a59f42dba53da6475c9))
* **frontend:** preserve i32 loop step overflow ([77ab12f](https://github.com/celox-sim/celox/commit/77ab12f8befa67263414a4083b1efd1bae67133b))
* **frontend:** preserve instance connection effects ([5264795](https://github.com/celox-sim/celox/commit/5264795e71807942d53c83ac395d4de40397f1a4))
* **frontend:** preserve observer capture positions ([67e8750](https://github.com/celox-sim/celox/commit/67e8750b439dfb810c31e51a82ff66367a270887))
* **frontend:** preserve ordered comb effects ([905b521](https://github.com/celox-sim/celox/commit/905b5215589a0cbf34c772ccb0409f910dd4d28a))
* **frontend:** preserve ordered comb expression effects ([a2639fc](https://github.com/celox-sim/celox/commit/a2639fc242ed9923bf75d53ffcb13e1d1c174310))
* **frontend:** preserve ordered output connections ([d0b0821](https://github.com/celox-sim/celox/commit/d0b08211e364d1f7b34a1ef46a12321c07460ee4))
* **frontend:** preserve output and const expression order ([ecc7328](https://github.com/celox-sim/celox/commit/ecc73286c166c4fe663011290bd7df8ab2de1efd))
* **frontend:** propagate comb output calls across contexts ([a73ffff](https://github.com/celox-sim/celox/commit/a73ffffcd3a2fb1a7d6935c2f8539cf10e78b1bd))
* **frontend:** skip inactive connection writes ([d0aca85](https://github.com/celox-sim/celox/commit/d0aca85a917a766993dd5c2e7dd09bc6d23803f4))
* **frontend:** support bitwise for-loop steps ([06f1635](https://github.com/celox-sim/celox/commit/06f1635c2e9162772e3ef9f2d09922edfe451617))
* **frontend:** support bitwise for-loop steps ([8526717](https://github.com/celox-sim/celox/commit/85267179ddf0caa21767bd712b13e44c7f1c22a8))
* **frontend:** trigger connection effects on transitions ([08afb04](https://github.com/celox-sim/celox/commit/08afb045b665c9cd807fd7fccae57776b38240af))
* **frontend:** use converted constant values ([982844d](https://github.com/celox-sim/celox/commit/982844d72998c64207335a88c5c823ea0c0973a3))
* **frontend:** warn on time-advancing loop bounds ([eae78c7](https://github.com/celox-sim/celox/commit/eae78c78af9a48c4e8e0d3a44cbb4f39def8375c))
* **napi:** synchronize CLI and emnapi versions ([9641615](https://github.com/celox-sim/celox/commit/9641615dcfd27e6ce17e37191e7296ddabcb2b47))
* **napi:** synchronize Rust and emnapi compatibility ([f7a4ba3](https://github.com/celox-sim/celox/commit/f7a4ba3422c5cc33480134615eabe2f456c57501))
* **native:** preserve four-state constant masks ([ace4863](https://github.com/celox-sim/celox/commit/ace486325d935f00cf6a27da7a85cdf43aeaa1d5))
* **native:** preserve four-state constant masks ([0ca402c](https://github.com/celox-sim/celox/commit/0ca402cabab2c233208fcfc80f0b76fbaea8d30a))
* **optimizer:** preserve entry-live fused state ([488010f](https://github.com/celox-sim/celox/commit/488010fa37d801118066b12f09ff71e78c3d8325))
* **playground:** isolate preview server ([1205727](https://github.com/celox-sim/celox/commit/1205727774246aef36cd049ddb597760b8f1dae2))
* **playground:** restore browser startup ([621469a](https://github.com/celox-sim/celox/commit/621469a02eee9302fe86fd936dec7a6c72f711dd))
* **playground:** restore browser startup ([463890c](https://github.com/celox-sim/celox/commit/463890cbe8c15a8c93f901901546729cc679accd))
* preserve required compiler validation ([bd83f0a](https://github.com/celox-sim/celox/commit/bd83f0a5b90d6209d321b1b1159accf0bea7e810))
* restore Heliodor boot compatibility ([28c4a66](https://github.com/celox-sim/celox/commit/28c4a66c0a398bad838d27d27919390c3b7e7226))
* restore Heliodor boot compatibility ([b1781c9](https://github.com/celox-sim/celox/commit/b1781c9fa60663dab72da5dc3eb399ec791b252f))
* **simulator:** defer authorized loop validation ([2cd40e3](https://github.com/celox-sim/celox/commit/2cd40e3358f30f46f058014719f200c5ce83a7ae))
* **simulator:** preserve tests for named projects ([4153897](https://github.com/celox-sim/celox/commit/41538978c535ad3292a631321e4e7a4e033ddf69))
* **slt:** map addresses without recursion ([18190c9](https://github.com/celox-sim/celox/commit/18190c9d7aca85940242619278e476609965f9d2))
* stage overlapping FF write ranges together ([0be0f57](https://github.com/celox-sim/celox/commit/0be0f5755d91927c85ce7404dc7f584192fcaca5))
* **testbench:** address component review feedback ([ac02dc6](https://github.com/celox-sim/celox/commit/ac02dc65a00deb03346f95acc1ca33b487e7656b))
* **testbench:** address component scheduling review ([db81516](https://github.com/celox-sim/celox/commit/db815162d0b75604c062d341ec3720103cbe95d5))
* **testbench:** address follow-up review ([69568c8](https://github.com/celox-sim/celox/commit/69568c868e84526d0c2a2b7869b44308ba263bea))
* **testbench:** address injected component review ([ce7452e](https://github.com/celox-sim/celox/commit/ce7452e401917fcb0f4bc2ea288b18e7d4e2f173))
* **testbench:** address latest component review ([92e8085](https://github.com/celox-sim/celox/commit/92e8085702012e887f1c02d855bf133ecd02bb97))
* **testbench:** address latest component review feedback ([b989e6a](https://github.com/celox-sim/celox/commit/b989e6a5f964c454c0dca4e47434e4620cec62ef))
* **testbench:** draw implicit seeds at execution ([b7e0d77](https://github.com/celox-sim/celox/commit/b7e0d77284265ed0c268cc99b46f65f5095502b0))
* **testbench:** evaluate dynamic reset durations ([8b4eb86](https://github.com/celox-sim/celox/commit/8b4eb864b1286a34f6d948d80e9989f9162afd49))
* **testbench:** flatten packed hierarchical selects ([12b11ee](https://github.com/celox-sim/celox/commit/12b11eec4bf7acc7bdaea88913a6b4b26e3215a5))
* **testbench:** handle hierarchical select geometry ([08c15d1](https://github.com/celox-sim/celox/commit/08c15d189d7e72311dfdffb19be5899a26e7b536))
* **testbench:** honor selected destination geometry ([8019bd0](https://github.com/celox-sim/celox/commit/8019bd059e5733f8fe0cb9c956cf0effb1647315))
* **testbench:** preserve dynamic resets and clocked child state ([6dced49](https://github.com/celox-sim/celox/commit/6dced4984070083c24bf8f11da2785b1e7e09421))
* **testbench:** preserve random seed accessor API ([5da3bd7](https://github.com/celox-sim/celox/commit/5da3bd7d52e69a22727dea5f3e5d01aa5f1cc3d1))
* **testbench:** preserve selected destinations ([8e8bb42](https://github.com/celox-sim/celox/commit/8e8bb42ceae2931af0ed6b572e1140ce2bfa1021))
* **testbench:** preserve selected destinations ([8c8966d](https://github.com/celox-sim/celox/commit/8c8966d6f25b8a863235757a1829f6b7411fccf0))
* **testbench:** preserve signed random result width ([1f59dd1](https://github.com/celox-sim/celox/commit/1f59dd130f8e83d354fbc1b20a9d7052630fa777))
* **testbench:** reject dynamic reset durations ([54e2314](https://github.com/celox-sim/celox/commit/54e2314c51eba437cda6d96e3059865921c4cb29))
* **testbench:** reject runtime values as static timing ([7a22a67](https://github.com/celox-sim/celox/commit/7a22a677fa8362d4377b558a87723319844c9384))
* **testbench:** reject selected destinations and randomize unset seeds ([db3c7ea](https://github.com/celox-sim/celox/commit/db3c7ea0f7b1731a28010efa376a06fe5ad6987a))
* **testbench:** reject unsupported selected helper writes ([a0979b4](https://github.com/celox-sim/celox/commit/a0979b4f88cdaf64415725b86f1a6812d09648a4))
* **testbench:** respect layout and argument widths ([9191526](https://github.com/celox-sim/celox/commit/91915264703cdca85555040c103a72058286fbb7))
* **testbench:** validate selected random returns ([d4d5b1a](https://github.com/celox-sim/celox/commit/d4d5b1acef8e880c81e2b49d170f1fd1b9858ce7))
* **veryl:** address 0.20.3 integration regressions ([63f510e](https://github.com/celox-sim/celox/commit/63f510e1f956182b7e45a5723cdcfae50b88dd73))
* **veryl:** address hierarchical read feedback ([f361b5e](https://github.com/celox-sim/celox/commit/f361b5e386de94116b6f774422942d61caa305ee))
* **veryl:** carry lazy array item caches across branches ([caa9e93](https://github.com/celox-sim/celox/commit/caa9e93f91e8d90cdcb8a871d5fdc28546e0da86))
* **veryl:** check hierarchical immediate writes ([06e5662](https://github.com/celox-sim/celox/commit/06e5662fab92503b3790af98edfe83c79634e667))
* **veryl:** coerce FF function results ([e0594fe](https://github.com/celox-sim/celox/commit/e0594febc5a8063c8b26108cf35935816553c625))
* **veryl:** detect callee array alias writes ([79b7a1c](https://github.com/celox-sim/celox/commit/79b7a1c03699f91ef44d544f724fceb810f44ac9))
* **veryl:** detect nested FF runtime effects ([872ed71](https://github.com/celox-sim/celox/commit/872ed714c0591e0d1e715324e8595ccec33debc6))
* **veryl:** evaluate unused array input effects ([b6e2107](https://github.com/celox-sim/celox/commit/b6e21074299c803b0c902757133caac6023c729b))
* **veryl:** execute conditional FF runtime effects ([f65630c](https://github.com/celox-sim/celox/commit/f65630c2a89aca6b3bc8926200a475c8d6de0a2c))
* **veryl:** guard FF function state transitions ([2a4aca8](https://github.com/celox-sim/celox/commit/2a4aca8969b984a8cadae76c728a725000c21584))
* **veryl:** guard nonlocal function writes ([c5bba52](https://github.com/celox-sim/celox/commit/c5bba52c0ad5542d9f5aee5ae7e40b65c9dd3af4))
* **veryl:** limit hierarchical names to module scope ([76435af](https://github.com/celox-sim/celox/commit/76435af25f2cd5857a77d5ed0862cd9be35df0a5))
* **veryl:** lower dynamic nonlocal function stores ([b17bae1](https://github.com/celox-sim/celox/commit/b17bae1330cb25ff047f3ee3ac84968be009aa42))
* **veryl:** materialize array literal function arguments ([a13d8f8](https://github.com/celox-sim/celox/commit/a13d8f81b4cc6d2566aa2310e3bfcfbfa7b6d7f3))
* **veryl:** materialize array literal function arguments ([0f066fb](https://github.com/celox-sim/celox/commit/0f066fb73b50965ef5a13f63d82d8c623c2a9e8d))
* **veryl:** materialize array views at access sites ([e84d0c5](https://github.com/celox-sim/celox/commit/e84d0c513b423acdbaaba6c2ee0cf32c9fb76280))
* **veryl:** merge forwarded array cache state ([177970f](https://github.com/celox-sim/celox/commit/177970f19b9daa6ffb4f8de78371ea7d0d77bd49))
* **veryl:** merge nested array state at cache joins ([9a7842e](https://github.com/celox-sim/celox/commit/9a7842ef38e56dfc3d8817d29b6ebd91130678e5))
* **veryl:** order nonlocal function copyout ([07f6259](https://github.com/celox-sim/celox/commit/07f62598b4d16000c2c3d9ff91a7d9b82838518a))
* **veryl:** preserve array literal evaluation semantics ([729907e](https://github.com/celox-sim/celox/commit/729907e42e2c45c9924cfe7ecc7a6c0435e974de))
* **veryl:** preserve array literal views across calls ([1c66f61](https://github.com/celox-sim/celox/commit/1c66f616d01ddb31e4aee5dc6128315f14ac93fa))
* **veryl:** preserve array views through output assignment ([99aa6a5](https://github.com/celox-sim/celox/commit/99aa6a5255dc6b92a5cc03897d5367ad4ef6e401))
* **veryl:** preserve dynamic function copyouts ([397cb72](https://github.com/celox-sim/celox/commit/397cb722eeb7e0bb69936fbd06767dbbf910dff0))
* **veryl:** preserve eager FF assertion effects ([eb489df](https://github.com/celox-sim/celox/commit/eb489dfd533de4c52be74665fcd51697736b7c38))
* **veryl:** preserve eager FF assertion effects ([3578cd1](https://github.com/celox-sim/celox/commit/3578cd1f2d106d46e04ac8ce8306f3c268e01a77))
* **veryl:** preserve FF array input snapshots ([631f1a4](https://github.com/celox-sim/celox/commit/631f1a41263e367c45a78aa9faf401a39e4be27d))
* **veryl:** preserve FF call-time inputs ([7b9892a](https://github.com/celox-sim/celox/commit/7b9892ac6e7137119707f41434b8512b80097854))
* **veryl:** preserve FF case evaluation order ([5475af6](https://github.com/celox-sim/celox/commit/5475af6f6d57b319f72ae514fd02677fede95a3f))
* **veryl:** preserve FF case return paths ([3c93218](https://github.com/celox-sim/celox/commit/3c9321807bb4b29c3eddb79eb50c58595851e746))
* **veryl:** preserve FF control effects lazily ([dfe8ae7](https://github.com/celox-sim/celox/commit/dfe8ae72124b16b3e7f9ab3860db337f15ec61f0))
* **veryl:** preserve FF effect evaluation order ([cd9bef5](https://github.com/celox-sim/celox/commit/cd9bef556e5f097eb91d0d9f6b677d20804d718b))
* **veryl:** preserve FF evaluation order ([4475a44](https://github.com/celox-sim/celox/commit/4475a4496baf0bf0a2bf51261c141fe2f8f71ca0))
* **veryl:** preserve FF expression evaluation order ([9f28386](https://github.com/celox-sim/celox/commit/9f28386a57908be0460720cc30b936ae2762d54b))
* **veryl:** preserve FF function observation semantics ([149b393](https://github.com/celox-sim/celox/commit/149b3934b28c33540570aaab1e253de1fa8a1fb7))
* **veryl:** preserve guarded function effects ([b10cda3](https://github.com/celox-sim/celox/commit/b10cda389357d4fda68656d93e02076839fd8d76))
* **veryl:** preserve lazy array argument evaluation ([c264ea2](https://github.com/celox-sim/celox/commit/c264ea2954a4914eb69b54443280adb2cb791356))
* **veryl:** preserve lazy array views across branches ([8867382](https://github.com/celox-sim/celox/commit/8867382d06a46d7f83e2d6e701c4b286fc3b6763))
* **veryl:** preserve nested FF copy-out semantics ([802fa24](https://github.com/celox-sim/celox/commit/802fa2437590c5027d23b6ee90b8cde143cda3aa))
* **veryl:** preserve nested FF effect paths ([bd497c0](https://github.com/celox-sim/celox/commit/bd497c08d42392bfe62b148ae1aeb252bd8824a3))
* **veryl:** preserve nested FF effect state ([272fcac](https://github.com/celox-sim/celox/commit/272fcacd18ee1ea02859a8c462909749bcab8a6c))
* **veryl:** preserve nested nonlocal state ([98a3f15](https://github.com/celox-sim/celox/commit/98a3f158042085585a10deaa9444990f3fff5047))
* **veryl:** preserve nonlocal copyout semantics ([f0c1d68](https://github.com/celox-sim/celox/commit/f0c1d68aa2afbe246d6b7d6334577585b7a1ae85))
* **veryl:** preserve nonlocal function snapshots ([928dc52](https://github.com/celox-sim/celox/commit/928dc52b1069c43f026dc27ba59d09d7d3d3a286))
* **veryl:** preserve ordered nonlocal state ([be15f5c](https://github.com/celox-sim/celox/commit/be15f5c0a0eec0fb4092ca7cbe1589d97ad8305c))
* **veryl:** preserve snapshots after master merge ([3e4aa13](https://github.com/celox-sim/celox/commit/3e4aa13c70a9f935240a5a4f330599a69f533268))
* **veryl:** preserve unevaluated expression state ([2c0d887](https://github.com/celox-sim/celox/commit/2c0d88749d240d091c9f749855e28e93fbb7d543))
* **veryl:** preserve unknown FF expression effects ([2c09834](https://github.com/celox-sim/celox/commit/2c09834cb23bd5278b3e26ea48828d804830991e))
* **veryl:** preserve unpacked argument dependencies ([a4bd550](https://github.com/celox-sim/celox/commit/a4bd550da0576730902a2344ba495e6daa97f929))
* **veryl:** propagate evaluated call state ([c18db04](https://github.com/celox-sim/celox/commit/c18db0424aff9041ac67ac85a1a487040cea6ba5))
* **veryl:** propagate FF runtime effect state ([cbfaaf2](https://github.com/celox-sim/celox/commit/cbfaaf223025b35ee92b4c09ff9e166979f64f21))
* **veryl:** reconcile FF eager evaluation with array views ([c0a7ae2](https://github.com/celox-sim/celox/commit/c0a7ae2417afb4111977b1345428a7cf26bd5d95))
* **veryl:** reject aliased lazy array inputs ([af64a68](https://github.com/celox-sim/celox/commit/af64a68606ce4e61bc0c364827b5125826dfce7d))
* **veryl:** reject aliased lazy array literals ([20a3f3e](https://github.com/celox-sim/celox/commit/20a3f3e4ea86d88211ac82ab01bb1fc1335f6352))
* **veryl:** restore nearest reentrant array view last ([541b553](https://github.com/celox-sim/celox/commit/541b553079be36330c26b8cc9709d82088f504e5))
* **veryl:** snapshot array inputs before callee writes ([c7a4188](https://github.com/celox-sim/celox/commit/c7a4188d5f756783a21776905e5b67e3ee83a4cb))
* **veryl:** snapshot FF control expressions ([ae1c110](https://github.com/celox-sim/celox/commit/ae1c110162323aaaf8eec10216a5e39c6e41fe3a))
* **veryl:** snapshot FF function assignment state ([d4a3f89](https://github.com/celox-sim/celox/commit/d4a3f8993bf5be7afdd97e1819f079e9651b59f5))
* **veryl:** snapshot helper call inputs ([719922d](https://github.com/celox-sim/celox/commit/719922de6e2b57d7758f97037d2237a5d0356640))
* **veryl:** snapshot nonlocal function writes ([369e4b2](https://github.com/celox-sim/celox/commit/369e4b2adfe52e75eb8840e4920e66b16a83ab62))
* **veryl:** stabilize FF effect evaluation ([b1fc2d8](https://github.com/celox-sim/celox/commit/b1fc2d84c0ee886c51c12a1080dacb48639dc7c0))
* **veryl:** stabilize nested FF output state ([0eaefcf](https://github.com/celox-sim/celox/commit/0eaefcf5d8d60727a47a1b70bd520f3f7fcb5123))
* **veryl:** track array views in bound literals ([26c08ee](https://github.com/celox-sim/celox/commit/26c08ee8de397f6ab3206f4f3a2f4eb1822c3376))
* **veryl:** track nested reads in output indices ([e769063](https://github.com/celox-sim/celox/commit/e7690631cde948397c9e137a2fb416e0c92b4bcb))
* **veryl:** track precise array merge state ([603d33d](https://github.com/celox-sim/celox/commit/603d33dce2c5d629c296ee5b2c77776c91c1bea6))
* **veryl:** type FF symbolic function state ([5c765f4](https://github.com/celox-sim/celox/commit/5c765f443b50ff810fa2acd1738a5c11a6b69125))
* **vite:** normalize component dependency paths ([1590b1d](https://github.com/celox-sim/celox/commit/1590b1d08ec09d99ca74d7f382ee797ed8faae88))


### Performance Improvements

* **arm64:** reuse spill storage ([53c79d7](https://github.com/celox-sim/celox/commit/53c79d7f22f2d4f4249a1e7851398ec0d5a94cd3))
* **native:** match Heliodor compile time to Veryl ([02b58aa](https://github.com/celox-sim/celox/commit/02b58aa80a227d103d3f4fbc955ac35e22875d20))
* **native:** match Heliodor compile time to Veryl ([86b4775](https://github.com/celox-sim/celox/commit/86b4775c5369023dff3137fc8ddab4fb76662063))
* **optimizer:** index redundant load ranges ([61467ee](https://github.com/celox-sim/celox/commit/61467ee63ebf8bf50b1818ebc61bb45ae6b74c20))
* **optimizer:** reduce compile-time analysis overhead ([32b1f45](https://github.com/celox-sim/celox/commit/32b1f452efa47e6bb2a012e551dfcee11f9b9a1c))
* **optimizer:** reduce repeated scheduling work ([4efa080](https://github.com/celox-sim/celox/commit/4efa08013c2641215dd9fe5c1ffa723312a3d262))
* **optimizer:** skip redundant store analysis ([37eb2c6](https://github.com/celox-sim/celox/commit/37eb2c6a0d8a4f7d0289395d4e858d8b9e84013b))
* **optimizer:** skip redundant store analysis ([4cd92b4](https://github.com/celox-sim/celox/commit/4cd92b42241fa437852c053490e8792d02a04879))
* reduce Heliodor O2 compile time ([c32ae63](https://github.com/celox-sim/celox/commit/c32ae633162dbf25d81e9f1c4a8220746d417191))
* reduce Heliodor O2 compile time ([9908e72](https://github.com/celox-sim/celox/commit/9908e72f1b7563fb01a038a1818d86599207d62a))

## [0.1.35](https://github.com/celox-sim/celox/compare/v0.1.34...v0.1.35) (2026-08-01)


### Features

* **bench:** add a dedicated Heliodor benchmark harness ([aed35e6](https://github.com/celox-sim/celox/commit/aed35e603d1383a0184f1fb11362f07d05898fc9))


### Bug Fixes

* align sliced dependencies with lowering ([8c96d3a](https://github.com/celox-sim/celox/commit/8c96d3a61f4623db098bbeb37cbf4f9451da2b6d))
* **ci:** disable benchmark debuginfo ([7773da3](https://github.com/celox-sim/celox/commit/7773da3c80776d7af4ba90b5f09ce23c05b3bb65))
* **ci:** disable benchmark debuginfo ([5cd80b7](https://github.com/celox-sim/celox/commit/5cd80b7182a39cb3561d0a377d481b4eee619d24))
* **ci:** extend Heliodor gate timeout ([5e35668](https://github.com/celox-sim/celox/commit/5e35668f2065f64dd19e06f131ddcab1c229e016))
* **ci:** extend Heliodor gate timeout ([66026a3](https://github.com/celox-sim/celox/commit/66026a309fb323fc71ca12dee0d7ed779dfb3ca5))
* **ci:** install ripgrep for Heliodor ([349df8b](https://github.com/celox-sim/celox/commit/349df8b18533abb357b16bb21183d8e1dfd170bd))
* **ci:** let benchmark runs finish ([13b5848](https://github.com/celox-sim/celox/commit/13b5848854f69218fe6f10a142a4f48a67623257))
* **ci:** let benchmark runs finish ([62ea240](https://github.com/celox-sim/celox/commit/62ea2402ac9a1c8131a80e91b9859303eecf83ff))
* **ci:** mint release tokens from GitHub App ([f6102bd](https://github.com/celox-sim/celox/commit/f6102bda9d1e393cd8fdfa47f7593f17bc909c0b))
* **ci:** mint release tokens from GitHub App ([f605361](https://github.com/celox-sim/celox/commit/f605361ae21ecef4baff941aef6f69d0c2d3f05e))
* **ci:** normalize Heliodor optimization level ([dab15b6](https://github.com/celox-sim/celox/commit/dab15b6faf3a875c0d9afedcb1cfaf42cd2358d7))
* **ci:** normalize Heliodor optimization level ([359a52f](https://github.com/celox-sim/celox/commit/359a52f154074d36d86cea23445d0ff2bed9036e))
* **ci:** remove Heliodor ripgrep dependency ([fbe455f](https://github.com/celox-sim/celox/commit/fbe455f73eb270941eebe9e22e5f56c3c62472e0))
* **ci:** remove Heliodor ripgrep dependency ([5f89500](https://github.com/celox-sim/celox/commit/5f895001630c2491d0b341248b2b3b56e7c7e82b))
* **ci:** skip product tests for release PRs ([b4fac74](https://github.com/celox-sim/celox/commit/b4fac74c31c0c271248f8e4fbd799495d48e51e6))
* **ci:** skip product tests for release PRs ([29cb509](https://github.com/celox-sim/celox/commit/29cb509106e1d56c10c3d5a1054d4ebafb9747e4))
* **ci:** validate PR titles from current default branch ([f9ddd47](https://github.com/celox-sim/celox/commit/f9ddd47849ec856fa12a23bc4d422eaafc4001a7))
* **ci:** validate PR titles from current default branch ([bbda83c](https://github.com/celox-sim/celox/commit/bbda83c48ec78316b7ba84cd86adea0eeede88ec))
* **cli:** accept case-insensitive optimization levels ([89248e9](https://github.com/celox-sim/celox/commit/89248e9b482841661fe5d6f356ffe2c9dc6760ab))
* **cli:** accept case-insensitive optimization levels ([356fddd](https://github.com/celox-sim/celox/commit/356fddda465a654fb57d618705cb925f8669ca7b))
* **cranelift:** adapt to 0.134 API ([6648f80](https://github.com/celox-sim/celox/commit/6648f80aa2be4450dc6fccb8d69e8596213f405a))
* **deps:** refresh Cargo.lock for master base ([b615cd9](https://github.com/celox-sim/celox/commit/b615cd9566d5b7b56bee970b3ca537f32a746bf3))
* **deps:** update cranelift crates to 0.134.0 ([cdc8904](https://github.com/celox-sim/celox/commit/cdc8904b5552ce883e1064d4c91242bdaed2a9f1))
* **deps:** update cranelift crates to 0.134.0 ([e0f95b2](https://github.com/celox-sim/celox/commit/e0f95b2012c6bad27559a9bf683e1a70a1cbfc54))
* honor Cranelift codegen trace options ([c64d03a](https://github.com/celox-sim/celox/commit/c64d03a01ab6bba0f0e9e3b191a07a8ffc975788))
* **hooks:** allow uninitialized submodules ([7432ddd](https://github.com/celox-sim/celox/commit/7432ddd6558cc49c7028fe1e15ff1308dc1e7d9d))
* **hooks:** allow uninitialized submodules ([6623366](https://github.com/celox-sim/celox/commit/6623366e950ffd31d9537cbfcf5f5b632c0342d3))
* preserve dependencies in guarded SIR regions ([f05487d](https://github.com/celox-sim/celox/commit/f05487d3824010a9706bb1103ad874ff38ad7b5b))
* preserve logical widths in dynamic state access ([c687e5d](https://github.com/celox-sim/celox/commit/c687e5d668699ec9f575fec5c496691c7c5868d8))
* preserve propagated RTL atom boundaries ([9e01add](https://github.com/celox-sim/celox/commit/9e01add76bba8e1a4cd728b330291ef9c3266875))
* **regalloc:** preserve edge reconstruction pressure ([bef13f1](https://github.com/celox-sim/celox/commit/bef13f1ebae106f7791fd42582f9a1ce4320cdb7))
* **regalloc:** preserve edge reconstruction pressure ([b249f52](https://github.com/celox-sim/celox/commit/b249f522403980c3641c40e0df08d70a1d58b59c))
* remove unused host dependencies from wasm build ([db4d6b2](https://github.com/celox-sim/celox/commit/db4d6b2334b69b4d0bfc551c4fe4570e5e8da52c))
* **renovate:** group emnapi package updates ([dd92c72](https://github.com/celox-sim/celox/commit/dd92c723df07d025da3a7a6f459aba8f0abcaf0c))
* **renovate:** group emnapi package updates ([a59a738](https://github.com/celox-sim/celox/commit/a59a7387990a6bb7f7b9a2c258eaaf5af948c5fe))
* scope native runtime extraction to x86 ([39677aa](https://github.com/celox-sim/celox/commit/39677aa24d260f37f25452af26a50c77ef4f87b5))


### Performance Improvements

* coalesce pointwise RTL atoms by source identity ([76f10f2](https://github.com/celox-sim/celox/commit/76f10f253db92f7abdec7294bdd3b9ea18c82bae))
* reuse block-local dynamic state loads ([3014472](https://github.com/celox-sim/celox/commit/301447216c234e5fd94c4c47f3c9d574576319ac))


### Reverts

* drop redundant Heliodor setup ([625511f](https://github.com/celox-sim/celox/commit/625511f8b540d04ff7a426bb57a19613c9dc75cf))

## Changelog

Celox releases are generated from Conventional Commit pull request titles.
