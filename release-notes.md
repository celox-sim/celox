:robot: I have created a release *beep* *boop*
---


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

---
This PR was generated with [Release Please](https://github.com/googleapis/release-please). See [documentation](https://github.com/googleapis/release-please#release-please).