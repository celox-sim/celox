//! Covergroup declarations as typed nodes (IEEE 1800-2023 clause 19,
//! A.2.11).
use super::*;

fn parse(code: &str) -> Declarations {
    Declarations::parse(code, Path::new("coverage.sv")).unwrap()
}

/// `items` as the items of a covergroup `cg`, of the compilation unit and
/// of a module.
fn covergroups(items: &str) -> [Declarations; 2] {
    [
        parse(&format!(
            "covergroup cg; {items} endgroup module M; endmodule"
        )),
        parse(&format!(
            "module M; covergroup cg; {items} endgroup endmodule"
        )),
    ]
}

fn text(syntax: &Declarations, id: ExprId) -> String {
    syntax.token_text(syntax.expression(id).tokens)
}

/// The form and the operands of each of `ranges`.
fn ranges(syntax: &Declarations, ranges: &[CoverageRange]) -> Vec<String> {
    let bound = |bound: &Option<ExprId>| bound.map_or("$".to_string(), |id| text(syntax, id));
    ranges
        .iter()
        .map(|range| match &range.kind {
            CoverageRangeKind::Value(value) => format!("value {}", text(syntax, *value)),
            CoverageRangeKind::Range { low, high } => {
                format!("range {} {}", bound(low), bound(high))
            }
            CoverageRangeKind::Tolerance {
                center,
                tolerance,
                relative,
            } => format!(
                "{} {} {}",
                if *relative { "relative" } else { "absolute" },
                text(syntax, *center),
                text(syntax, *tolerance)
            ),
        })
        .collect()
}

/// The steps of each transition set of `bins`: their ranges, then their
/// repetition.
fn transitions(syntax: &Declarations, bins: &Bins) -> Vec<Vec<String>> {
    let BinsValue::Transitions(sets) = &bins.value else {
        panic!("{:?}", bins.value)
    };
    sets.iter()
        .map(|set| {
            set.steps
                .iter()
                .map(|step| {
                    let repeat = step.repeat.as_ref().map_or(String::new(), |repeat| {
                        format!(
                            " {:?} {}{}",
                            repeat.kind,
                            text(syntax, repeat.low),
                            repeat
                                .high
                                .map_or(String::new(), |high| format!(":{}", text(syntax, high)))
                        )
                    });
                    format!("{}{repeat}", ranges(syntax, &step.ranges).join(" | "))
                })
                .collect()
        })
        .collect()
}

/// The structure of select expression `id`.
fn select(syntax: &Declarations, id: SelectId) -> String {
    let count = |count: &Option<MatchCount>| match count {
        None => String::new(),
        Some(MatchCount::All) => " matches $".to_string(),
        Some(MatchCount::Value(value)) => format!(" matches {}", text(syntax, *value)),
    };
    match &syntax.coverage_select(id).kind {
        SelectKind::Binsof {
            negated,
            target,
            bin,
            intersect,
        } => format!(
            "{}binsof({}{}){}",
            if *negated { "!" } else { "" },
            target.value(),
            bin.as_ref()
                .map_or(String::new(), |bin| format!(".{}", bin.value())),
            intersect
                .as_ref()
                .map_or(String::new(), |intersect| format!(
                    " intersect [{}]",
                    ranges(syntax, intersect).join(", ")
                ))
        ),
        SelectKind::And { left, right } => {
            format!("and({}, {})", select(syntax, *left), select(syntax, *right))
        }
        SelectKind::Or { left, right } => {
            format!("or({}, {})", select(syntax, *left), select(syntax, *right))
        }
        SelectKind::Group(inner) => format!("group({})", select(syntax, *inner)),
        SelectKind::With {
            select: inner,
            with,
            matches,
        } => format!(
            "with({}, {}{})",
            select(syntax, *inner),
            text(syntax, *with),
            count(matches)
        ),
        SelectKind::Expression { value, matches } => {
            format!("expr({}{})", text(syntax, *value), count(matches))
        }
    }
}

/// The bins of the coverpoint that is item `index` of the first covergroup.
fn coverpoint_bins(syntax: &Declarations, index: usize) -> Vec<&Bins> {
    let CoverageItem::Coverpoint(point) = &syntax.covergroups[0].items[index] else {
        panic!("{:?}", syntax.covergroups[0].items[index])
    };
    point
        .items
        .iter()
        .filter_map(|item| match item {
            CoverpointItem::Bins(bins) => Some(bins),
            CoverpointItem::Option(_) => None,
        })
        .collect()
}

/// A covergroup is one body item of the scope that declares it, and of a
/// class also one of its embedded covergroups (19.3, 19.4).
#[test]
fn covergroups_are_items_of_the_scopes_that_declare_them() {
    let cg = "covergroup cg @(posedge clk); coverpoint x; endgroup : cg";
    let declared = |syntax: &Declarations, item: BodyItemId| {
        let body = syntax.body_item(item);
        let BodyItemKind::Covergroup(id) = body.kind else {
            panic!("{:?}", body.kind)
        };
        let covergroup = syntax.covergroup(id);
        assert_eq!(covergroup.name.value(), "cg");
        assert_eq!(syntax.token_text(covergroup.header), "covergroup cg");
        assert_eq!(syntax.token_text(covergroup.tokens), cg);
        assert_eq!(syntax.token_text(body.tokens), cg);
        id
    };
    let children = |syntax: &Declarations, item: BodyItemId, expected: &str| {
        let BodyItemKind::Unconverted { category, items } = &syntax.body_item(item).kind else {
            panic!("{:?}", syntax.body_item(item).kind)
        };
        assert_eq!(category, expected);
        items.clone()
    };
    let source_declaration = |syntax: &Declarations| {
        let SourceItemKind::Declaration(item) = syntax.source_items[0].kind else {
            panic!("{:?}", syntax.source_items[0].kind)
        };
        item
    };

    let syntax = parse(&format!("{cg} module M; endmodule"));
    assert_eq!(syntax.source_items.len(), 2);
    declared(&syntax, source_declaration(&syntax));
    assert_eq!(syntax.token_text(syntax.source_items[0].tokens), cg);

    for (open, close) in [
        ("module M;", "endmodule"),
        ("package P;", "endpackage"),
        ("interface I;", "endinterface"),
        ("program P;", "endprogram"),
    ] {
        let syntax = parse(&format!("{open} bit clk, x; {cg} int after; {close}"));
        let items = &syntax.units[0].items;
        assert_eq!(items.len(), 3, "{open}");
        declared(&syntax, items[1]);
    }

    // A generate region and a generate block.
    let syntax = parse(&format!(
        "module M; bit clk, x; generate {cg} if (1) begin : g {cg} end endgenerate endmodule"
    ));
    let BodyItemKind::Region(region) = &syntax.body_item(syntax.units[0].items[1]).kind else {
        panic!()
    };
    declared(&syntax, region[0]);
    let BodyItemKind::Conditional { then_body, .. } = syntax.body_item(region[1]).kind else {
        panic!()
    };
    let BodyItemKind::Block { items, .. } = &syntax.body_item(then_body).kind else {
        panic!()
    };
    declared(&syntax, items[0]);

    // A checker of a design element and of the compilation unit.
    let syntax = parse(&format!("module M; checker C; {cg} endchecker endmodule"));
    let items = children(&syntax, syntax.units[0].items[0], "checker");
    declared(&syntax, items[0]);
    let syntax = parse(&format!("checker C; {cg} endchecker module M; endmodule"));
    let items = children(&syntax, source_declaration(&syntax), "checker");
    declared(&syntax, items[0]);

    // A class: the covergroup is an item of the class's body and one of
    // its embedded covergroups.
    for (source, unit) in [
        (
            format!("class C; int x; {cg} endclass module M; endmodule"),
            false,
        ),
        (
            format!("module M; class C; int x; {cg} endclass endmodule"),
            true,
        ),
        (
            format!("package P; class C; int x; {cg} endclass endpackage"),
            true,
        ),
    ] {
        let syntax = parse(&source);
        let class = if unit {
            syntax.units[0].items[0]
        } else {
            source_declaration(&syntax)
        };
        let items = children(&syntax, class, "class");
        assert_eq!(items.len(), 1, "{source}");
        let id = declared(&syntax, items[0]);
        assert_eq!(syntax.classes[0].covergroups, [id], "{source}");
    }

    // A nested class owns its covergroups, and its outer class does not.
    let classes = "class Outer; class Inner; covergroup ci; endgroup endclass \
                   covergroup co; endgroup endclass";
    for source in [
        format!("{classes} module M; endmodule"),
        format!("module M; {classes} endmodule"),
    ] {
        let syntax = parse(&source);
        let embedded = |class: &ClassDeclaration| {
            class
                .covergroups
                .iter()
                .map(|id| syntax.covergroup(*id).name.value().to_owned())
                .collect::<Vec<_>>()
        };
        // The nested class finishes first.
        assert_eq!(syntax.classes[0].name.value(), "Inner", "{source}");
        assert_eq!(embedded(&syntax.classes[0]), ["ci"], "{source}");
        assert_eq!(embedded(&syntax.classes[1]), ["co"], "{source}");
    }
}

/// The header of a covergroup: its formals and its coverage event (19.3,
/// 19.8.1), or the covergroup of a base class it extends (19.4.1).
#[test]
fn covergroup_headers_keep_their_formals_and_events() {
    let headers = |header: &str| {
        [
            parse(&format!(
                "covergroup {header}; endgroup module M; endmodule"
            )),
            parse(&format!(
                "module M; covergroup {header}; endgroup endmodule"
            )),
        ]
    };
    for syntax in headers("cg") {
        let covergroup = &syntax.covergroups[0];
        assert_eq!(
            covergroup.kind,
            CovergroupKind::Declared {
                ports: None,
                event: None
            }
        );
        assert!(covergroup.items.is_empty());
        assert!(covergroup.end_label.is_none());
    }
    for syntax in headers("cg()") {
        assert_eq!(
            syntax.covergroups[0].kind,
            CovergroupKind::Declared {
                ports: Some(Vec::new()),
                event: None
            }
        );
    }
    for syntax in headers("cg(int a, ref bit [3:0] b, input string s = \"x\")") {
        let CovergroupKind::Declared {
            ports: Some(ports),
            event: None,
        } = &syntax.covergroups[0].kind
        else {
            panic!("{:?}", syntax.covergroups[0].kind)
        };
        let formals = ports
            .iter()
            .map(|port| (port.name.as_ref().unwrap().value(), port.direction))
            .collect::<Vec<_>>();
        assert_eq!(
            formals,
            [
                ("a", None),
                ("b", Some(Direction::Ref)),
                ("s", Some(Direction::Input))
            ]
        );
        assert!(ports[2].default.is_some());
    }

    for (event, named) in [
        ("@clk", true),
        ("@top.clk", true),
        ("@P::cb", true),
        ("@(posedge clk iff en)", false),
        ("@(a or negedge b)", false),
    ] {
        for syntax in headers(&format!("cg {event}")) {
            let CovergroupKind::Declared {
                ports: None,
                event: Some(CoverageEvent::Clocking(id)),
            } = syntax.covergroups[0].kind
            else {
                panic!("{event}: {:?}", syntax.covergroups[0].kind)
            };
            let control = syntax.timing_control(id);
            assert_eq!(syntax.token_text(control.tokens), event);
            match &control.kind {
                TimingControlKind::Event(EventControl::Named(name)) => {
                    assert!(named, "{event}");
                    assert_eq!(text(&syntax, *name), &event[1..]);
                }
                TimingControlKind::Event(EventControl::Items(_)) => assert!(!named, "{event}"),
                kind => panic!("{event}: {kind:?}"),
            }
        }
    }

    for (formals, count) in [("", 0), ("int x, bit [1:0] y", 2)] {
        for syntax in headers(&format!("cg(int limit) with function sample({formals})")) {
            let CovergroupKind::Declared {
                ports: Some(ports),
                event: Some(CoverageEvent::Sample(sample)),
            } = &syntax.covergroups[0].kind
            else {
                panic!("{formals}: {:?}", syntax.covergroups[0].kind)
            };
            assert_eq!(ports.len(), 1);
            assert_eq!(sample.len(), count);
        }
    }

    for syntax in headers("cg @@(begin top.start or end P::C::f)") {
        let CovergroupKind::Declared {
            ports: None,
            event: Some(CoverageEvent::Block(events)),
        } = &syntax.covergroups[0].kind
        else {
            panic!("{:?}", syntax.covergroups[0].kind)
        };
        let events = events
            .iter()
            .map(|event| {
                (
                    event.end,
                    text(&syntax, event.target),
                    syntax.token_text(event.tokens),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            [
                (
                    false,
                    "top.start".to_string(),
                    "begin top.start".to_string()
                ),
                (true, "P::C::f".to_string(), "end P::C::f".to_string()),
            ]
        );
    }

    // `covergroup extends`, which only a class declares.
    let class = "class D extends B; covergroup extends cg; option.weight = 2; \
                 coverpoint y; endgroup : cg endclass";
    for source in [
        format!("{class} module M; endmodule"),
        format!("module M; {class} endmodule"),
        format!("package P; {class} endpackage"),
    ] {
        let syntax = parse(&source);
        let covergroup = &syntax.covergroups[0];
        assert_eq!(covergroup.kind, CovergroupKind::Extends, "{source}");
        assert_eq!(covergroup.name.value(), "cg");
        assert_eq!(
            syntax.token_text(covergroup.header),
            "covergroup extends cg"
        );
        assert_eq!(covergroup.items.len(), 2);
        assert_eq!(syntax.classes[0].covergroups, [CovergroupId(0)]);
    }

    // The name after `endgroup` is that of the covergroup (9.3.4).
    for source in [
        "covergroup cg; endgroup : cg module M; endmodule",
        "module M; covergroup cg; endgroup : cg endmodule",
    ] {
        let syntax = parse(source);
        let covergroup = &syntax.covergroups[0];
        assert_eq!(covergroup.end_label.as_ref().unwrap().value(), "cg");
        assert_eq!(
            syntax.token_text(covergroup.tokens),
            "covergroup cg; endgroup : cg"
        );
    }
    for source in [
        "covergroup cg; endgroup : other module M; endmodule",
        "module M; covergroup cg; endgroup : other endmodule",
        "module M; class D extends B; covergroup extends cg; endgroup : other endclass endmodule",
    ] {
        let error = Declarations::parse(source, Path::new("coverage.sv"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("end label `other` differs from the name `cg` of its block"),
            "{source}: {error}"
        );
    }
}

/// Every alternative of a coverpoint's bins (19.5, Syntax 19-2), and the
/// options and attributes of the items.
#[test]
fn coverpoint_bins_keep_their_values() {
    let items = "(* a *) option.per_instance = 1; \
        type_option.weight = 2; \
        cp: coverpoint x { \
            (* b *) option.weight = 3; \
            bins one = {1}; \
            bins lows[] = {[0:3], 5}; \
            bins fixed[4] = {[$:7], [8:$]}; \
            bins near = {[8.5 +/- 0.1], [A +%- 25]}; \
            bins tight = {[8.5+/-0.1]}; \
            wildcard bins wc = {4'b1xx0} iff (en); \
            bins evens = {[0:15]} with (item % 2 == 0) iff (en); \
            bins filtered[] = cp with (item > 2); \
            bins set_name = values; \
            bins set_call[] = f(3); \
            (* c *) illegal_bins bad = {13}; \
            ignore_bins skip = default; \
            bins rest[] = default; \
            bins others = default sequence; \
        } \
        logic [3:0] wide: coverpoint y iff (en); \
        coverpoint z;";
    for syntax in covergroups(items) {
        let [
            CoverageItem::Option(instance),
            CoverageItem::Option(of_type),
            CoverageItem::Coverpoint(point),
            CoverageItem::Coverpoint(wide),
            CoverageItem::Coverpoint(plain),
        ] = syntax.covergroups[0].items.as_slice()
        else {
            panic!("{:?}", syntax.covergroups[0].items)
        };
        assert!(!instance.type_option);
        assert_eq!(instance.member.value(), "per_instance");
        assert_eq!(text(&syntax, instance.value), "1");
        assert_eq!(
            syntax.token_text(instance.tokens),
            "(* a *) option.per_instance = 1;"
        );
        assert!(of_type.type_option);
        assert_eq!(of_type.member.value(), "weight");

        assert_eq!(point.label.as_ref().unwrap().value(), "cp");
        assert!(point.data_type.is_none());
        assert_eq!(text(&syntax, point.expression), "x");
        assert!(point.iff.is_none());
        let body = syntax.token_text(point.body.unwrap());
        assert!(body.starts_with('{') && body.ends_with('}'), "{body}");
        assert!(
            syntax
                .token_text(point.tokens)
                .starts_with("cp: coverpoint x {")
        );
        let CoverpointItem::Option(option) = &point.items[0] else {
            panic!("{:?}", point.items[0])
        };
        assert_eq!(
            syntax.token_text(option.tokens),
            "(* b *) option.weight = 3;"
        );

        let bins = coverpoint_bins(&syntax, 2);
        let named = |name: &str| {
            *bins
                .iter()
                .find(|bins| bins.name.value() == name)
                .unwrap_or_else(|| panic!("{name}"))
        };
        let values = |name: &str| {
            let BinsValue::Ranges { ranges: list, with } = &named(name).value else {
                panic!("{name}: {:?}", named(name).value)
            };
            (ranges(&syntax, list), with.map(|with| text(&syntax, with)))
        };
        let guard = |name: &str| named(name).iff.map(|iff| text(&syntax, iff));

        let one = named("one");
        assert_eq!(
            (one.wildcard, one.kind, &one.array),
            (false, BinsKind::Bins, &None)
        );
        assert_eq!(values("one"), (vec!["value 1".to_string()], None));
        assert_eq!(syntax.token_text(one.tokens), "bins one = {1};");

        assert_eq!(named("lows").array, Some(BinsArray::Unsized));
        assert_eq!(values("lows").0, ["range 0 3", "value 5"]);

        let Some(BinsArray::Sized(size)) = named("fixed").array else {
            panic!("{:?}", named("fixed").array)
        };
        assert_eq!(text(&syntax, size), "4");
        assert_eq!(values("fixed").0, ["range $ 7", "range 8 $"]);

        assert_eq!(values("near").0, ["absolute 8.5 0.1", "relative A 25"]);
        assert_eq!(values("tight").0, ["absolute 8.5 0.1"]);

        assert!(named("wc").wildcard);
        assert_eq!(values("wc").0, ["value 4'b1xx0"]);
        assert_eq!(guard("wc").as_deref(), Some("en"));

        assert_eq!(
            values("evens"),
            (
                vec!["range 0 15".to_string()],
                Some("item % 2 == 0".to_string())
            )
        );
        assert_eq!(guard("evens").as_deref(), Some("en"));

        let filtered = named("filtered");
        assert_eq!(filtered.array, Some(BinsArray::Unsized));
        let BinsValue::CoverpointWith { coverpoint, with } = &filtered.value else {
            panic!("{:?}", filtered.value)
        };
        assert_eq!(coverpoint.value(), "cp");
        assert_eq!(text(&syntax, *with), "item > 2");

        let set = |name: &str| {
            let BinsValue::Set(value) = named(name).value else {
                panic!("{name}: {:?}", named(name).value)
            };
            text(&syntax, value)
        };
        assert_eq!(set("set_name"), "values");
        assert_eq!(set("set_call"), "f(3)");
        assert_eq!(named("set_call").array, Some(BinsArray::Unsized));

        assert_eq!(named("bad").kind, BinsKind::IllegalBins);
        assert_eq!(
            syntax.token_text(named("bad").tokens),
            "(* c *) illegal_bins bad = {13};"
        );
        assert_eq!(named("skip").kind, BinsKind::IgnoreBins);
        assert_eq!(named("skip").value, BinsValue::Default);
        assert_eq!(named("rest").value, BinsValue::Default);
        assert_eq!(named("rest").array, Some(BinsArray::Unsized));
        assert_eq!(named("others").value, BinsValue::DefaultSequence);

        assert_eq!(wide.label.as_ref().unwrap().value(), "wide");
        assert_eq!(
            syntax.token_text(syntax.data_type(wide.data_type.unwrap()).tokens),
            "logic [3:0]"
        );
        assert_eq!(text(&syntax, wide.iff.unwrap()), "en");
        assert!(wide.body.is_none() && wide.items.is_empty());
        assert_eq!(
            syntax.token_text(wide.tokens),
            "logic [3:0] wide: coverpoint y iff (en);"
        );

        assert!(plain.label.is_none() && plain.data_type.is_none());
        assert_eq!(text(&syntax, plain.expression), "z");
    }
}

/// Transition bins (19.5.2): sets, steps, range lists and the three
/// repetitions, which follow a value without becoming a select of it.
#[test]
fn transition_bins_keep_their_steps_and_repetitions() {
    let items = "coverpoint x { \
            bins pair = (1 => 2); \
            bins sets = (1 => 2), (3 => 4); \
            bins lists = ([7:9], 10 => 11, 12); \
            bins consecutive = (3 [* 3:5]); \
            bins goto = (3[->3]); \
            bins nonconsecutive = (1 => 3 [= 2] => 6); \
            bins bus = (AHB_WR, AXI_WR => IDLE [*1:1000] => AHB_RD, AXI_RD); \
            bins operand = (a+1[*2] => b[1]); \
            bins range = ([1:2][*3]); \
            wildcard bins open[] = (2'b0x => 2'b1x); \
            bins others = default sequence; \
        }";
    for syntax in covergroups(items) {
        let bins = coverpoint_bins(&syntax, 0);
        let named = |name: &str| {
            *bins
                .iter()
                .find(|bins| bins.name.value() == name)
                .unwrap_or_else(|| panic!("{name}"))
        };
        let steps = |name: &str| transitions(&syntax, named(name));
        assert_eq!(steps("pair"), [["value 1", "value 2"]]);
        assert_eq!(
            steps("sets"),
            [["value 1", "value 2"], ["value 3", "value 4"]]
        );
        assert_eq!(
            steps("lists"),
            [["range 7 9 | value 10", "value 11 | value 12"]]
        );
        assert_eq!(steps("consecutive"), [["value 3 Consecutive 3:5"]]);
        assert_eq!(steps("goto"), [["value 3 Goto 3"]]);
        assert_eq!(
            steps("nonconsecutive"),
            [["value 1", "value 3 NonConsecutive 2", "value 6"]]
        );
        assert_eq!(
            steps("bus"),
            [[
                "value AHB_WR | value AXI_WR",
                "value IDLE Consecutive 1:1000",
                "value AHB_RD | value AXI_RD"
            ]]
        );
        // The repetition ends the operand; an ordinary select does not.
        assert_eq!(
            steps("operand"),
            [["value a+1 Consecutive 2", "value b[1]"]]
        );
        assert_eq!(steps("range"), [["range 1 2 Consecutive 3"]]);
        assert_eq!(steps("open"), [["value 2'b0x", "value 2'b1x"]]);
        assert!(named("open").wildcard);
        assert_eq!(named("open").array, Some(BinsArray::Unsized));
        assert_eq!(named("others").value, BinsValue::DefaultSequence);

        let BinsValue::Transitions(sets) = &named("consecutive").value else {
            panic!()
        };
        assert_eq!(syntax.token_text(sets[0].tokens), "(3 [* 3:5])");
        let step = &sets[0].steps[0];
        assert_eq!(syntax.token_text(step.tokens), "3 [* 3:5]");
        assert_eq!(
            syntax.token_text(step.repeat.as_ref().unwrap().tokens),
            "[* 3:5]"
        );
    }
}

/// Select expressions of cross bins (19.6.1): `||` binds weaker than `&&`,
/// which binds weaker than a postfix `with`; both fold to the left.
#[test]
fn cross_bins_keep_their_select_expressions() {
    let items = "cx: cross a, b, c iff (en) { \
            option.weight = 2; \
            bins shape = binsof(a) || binsof(b) && binsof(a.low); \
            bins chain = binsof(a) && binsof(b) && binsof(c) || binsof(d) || binsof(e); \
            bins apple = X with (a+b < 257) matches 127; \
            bins cherry = ( binsof(b) intersect {[0:50]} && binsof(a.low)) with (a==b) matches $; \
            bins plum = binsof(b.two) with (b > 12) || binsof(a.low) with (a & b & mask); \
            ignore_bins negated = !binsof(a) intersect {[1:2], 5} iff (en); \
            illegal_bins set = '{ '{1,2}, '{3,4} }; \
            bins call = f(n) matches $ && binsof(a); \
            bins method = q.find(v) with (v > 1) || binsof(a); \
            bins nested = ((binsof(a))) with (a > 1); \
        } \
        cross a, b;";
    for syntax in covergroups(items) {
        let [CoverageItem::Cross(cross), CoverageItem::Cross(plain)] =
            syntax.covergroups[0].items.as_slice()
        else {
            panic!("{:?}", syntax.covergroups[0].items)
        };
        assert_eq!(cross.label.as_ref().unwrap().value(), "cx");
        let crossed = cross.items.iter().map(Name::value).collect::<Vec<_>>();
        assert_eq!(crossed, ["a", "b", "c"]);
        assert_eq!(text(&syntax, cross.iff.unwrap()), "en");
        let body = syntax.token_text(cross.body.unwrap());
        assert!(body.starts_with('{') && body.ends_with('}'), "{body}");
        assert!(matches!(&cross.body_items[0], CrossItem::Option(option)
            if option.member.value() == "weight"));
        let named = |name: &str| {
            cross
                .body_items
                .iter()
                .find_map(|item| match item {
                    CrossItem::Bins(bins) if bins.name.value() == name => Some(bins),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name}"))
        };
        let selected = |name: &str| select(&syntax, named(name).select);
        assert_eq!(
            selected("shape"),
            "or(binsof(a), and(binsof(b), binsof(a.low)))"
        );
        assert_eq!(
            selected("chain"),
            "or(or(and(and(binsof(a), binsof(b)), binsof(c)), binsof(d)), binsof(e))"
        );
        // The three bins of the 19.6.1.2 example.
        assert_eq!(selected("apple"), "with(expr(X), a+b < 257 matches 127)");
        assert_eq!(
            selected("cherry"),
            "with(group(and(binsof(b) intersect [range 0 50], binsof(a.low))), a==b matches $)"
        );
        assert_eq!(
            selected("plum"),
            "or(with(binsof(b.two), b > 12), with(binsof(a.low), a & b & mask))"
        );
        assert_eq!(
            selected("negated"),
            "!binsof(a) intersect [range 1 2, value 5]"
        );
        assert_eq!(named("negated").kind, BinsKind::IgnoreBins);
        assert_eq!(text(&syntax, named("negated").iff.unwrap()), "en");
        assert_eq!(selected("set"), "expr('{ '{1,2}, '{3,4} })");
        assert_eq!(named("set").kind, BinsKind::IllegalBins);
        assert_eq!(selected("call"), "and(expr(f(n) matches $), binsof(a))");
        // An array method keeps its `with` clause.
        assert_eq!(
            selected("method"),
            "or(expr(q.find(v) with (v > 1)), binsof(a))"
        );
        assert_eq!(selected("nested"), "with(group(group(binsof(a))), a > 1)");

        let cherry = syntax.coverage_select(named("cherry").select);
        assert_eq!(
            syntax.token_text(cherry.tokens),
            "( binsof(b) intersect {[0:50]} && binsof(a.low)) with (a==b) matches $"
        );
        let SelectKind::With { select: group, .. } = cherry.kind else {
            panic!("{:?}", cherry.kind)
        };
        assert_eq!(
            syntax.token_text(syntax.coverage_select(group).tokens),
            "( binsof(b) intersect {[0:50]} && binsof(a.low))"
        );
        let shape = syntax.coverage_select(named("shape").select);
        assert_eq!(
            syntax.token_text(shape.tokens),
            "binsof(a) || binsof(b) && binsof(a.low)"
        );
        assert_eq!(
            syntax.token_text(named("shape").tokens),
            "bins shape = binsof(a) || binsof(b) && binsof(a.low);"
        );

        assert!(plain.label.is_none() && plain.body.is_none());
        assert_eq!(plain.items.len(), 2);
        assert_eq!(syntax.token_text(plain.tokens), "cross a, b;");
    }
}

/// The functions of a cross body (19.6.1.4) are items of the cross alone:
/// no scope lists them among its subroutines.
#[test]
fn cross_body_functions_stay_with_their_cross() {
    let covergroup = "covergroup cg(int limit); \
        coverpoint a; coverpoint c; \
        axc: cross a, c { \
            function CrossQueueType myFunc1(int x); \
                for (int i = 0; i < 10; ++i) if (i < x) myFunc1.push_back('{i, i}); \
            endfunction \
            bins one = myFunc1(a); \
            bins two = myFunc2(10); \
            function CrossQueueType myFunc2(logic [1:0] x); \
                foreach (bc[i]) if (bc[i].a < x) myFunc2.push_back(bc[i]); \
            endfunction : myFunc2 \
        } \
        endgroup";
    let function = "function int f(); return 1; endfunction";
    for (source, source_items) in [
        (format!("{covergroup} {function} module M; endmodule"), 3),
        (format!("module M; {covergroup} {function} endmodule"), 1),
        (
            format!("class C; {covergroup} {function} endclass module M; endmodule"),
            2,
        ),
        (
            format!("module M; class C; {covergroup} {function} endclass endmodule"),
            1,
        ),
    ] {
        let syntax = parse(&source);
        let CoverageItem::Cross(cross) = &syntax.covergroups[0].items[2] else {
            panic!("{:?}", syntax.covergroups[0].items[2])
        };
        let items = cross
            .body_items
            .iter()
            .map(|item| match item {
                CrossItem::Function(function) => {
                    assert_eq!(function.kind, SubroutineKind::Function);
                    assert_eq!(function.ports.as_ref().unwrap().len(), 1);
                    assert_eq!(function.items.len(), 1);
                    format!("function {}", function.name.value())
                }
                CrossItem::Bins(bins) => format!("bins {}", bins.name.value()),
                CrossItem::Option(option) => format!("option {}", option.member.value()),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            items,
            [
                "function myFunc1",
                "bins one",
                "bins two",
                "function myFunc2"
            ],
            "{source}"
        );
        let names = syntax
            .subroutines
            .iter()
            .map(|subroutine| subroutine.name.value())
            .collect::<Vec<_>>();
        assert_eq!(names, ["f"], "{source}");
        assert_eq!(syntax.source_items.len(), source_items, "{source}");
        assert!(syntax.units[0].subroutines.len() <= 1, "{source}");
        assert_eq!(
            syntax
                .classes
                .iter()
                .map(|class| class.methods.len())
                .sum::<usize>(),
            syntax.classes.len(),
            "{source}"
        );
    }
}

/// What the scanners of a design element and of the compilation unit used
/// to read from inside a covergroup: `sequence` of a `default sequence`
/// bin as a declaration, the label of `endgroup` as the end of the class
/// items, and a `typedef` of a cross-body function as a body item.
#[test]
fn covergroup_bodies_do_not_reach_the_enclosing_scanner() {
    let cg = "covergroup cg; coverpoint x { bins s = default sequence; } endgroup";
    for (open, close) in [
        ("module M;", "endmodule"),
        ("package P;", "endpackage"),
        ("interface I;", "endinterface"),
    ] {
        let syntax = parse(&format!("{open} bit [3:0] x; {cg} int after; {close}"));
        let unit = &syntax.units[0];
        assert_eq!(unit.items.len(), 3, "{open}");
        assert!(
            matches!(syntax.body_item(unit.items[2]).kind, BodyItemKind::Data(_)),
            "{open}"
        );
        assert_eq!(unit.data_declarations.len(), 2, "{open}");
    }
    let property_names = |class: &ClassDeclaration| {
        class
            .properties
            .iter()
            .flat_map(|property| &property.declaration.variables)
            .map(|variable| variable.name.value().to_owned())
            .collect::<Vec<_>>()
    };
    let syntax = parse(&format!(
        "module M; class C; bit [3:0] x; {cg} int after; endclass int outside; endmodule"
    ));
    assert_eq!(property_names(&syntax.classes[0]), ["x", "after"]);
    assert_eq!(syntax.units[0].data_declarations.len(), 1);

    // A labelled `endgroup` of a class of the compilation unit.
    for label in ["", " : cg"] {
        let syntax = parse(&format!(
            "class C; int x; covergroup cg; coverpoint x; endgroup{label} int after; \
             function int f(); return x; endfunction endclass module M; endmodule"
        ));
        assert_eq!(
            property_names(&syntax.classes[0]),
            ["x", "after"],
            "{label}"
        );
        assert_eq!(syntax.classes[0].methods.len(), 1, "{label}");
    }

    // A declaration in a function of a cross body is one of the function:
    // no body item, and no type alias of a class or a design element.
    let cross = "covergroup cg; cross a, b { \
                 function int helper(); typedef int T; return 1; endfunction } endgroup";
    let syntax = parse(&format!("class C; {cross} endclass module M; endmodule"));
    assert!(syntax.classes[0].type_aliases.is_empty());
    assert_eq!(syntax.source_items.len(), 2);
    let syntax = parse(&format!("{cross} module M; endmodule"));
    assert_eq!(syntax.source_items.len(), 2);
    for source in [
        format!("{cross} module M; endmodule"),
        format!("class C; {cross} endclass module M; endmodule"),
        format!("module M; {cross} endmodule"),
        format!("module M; class C; {cross} endclass endmodule"),
    ] {
        let syntax = parse(&source);
        assert!(
            syntax
                .body_items
                .iter()
                .all(|item| !matches!(item.kind, BodyItemKind::TypeAlias(_))),
            "{source}"
        );
        assert!(syntax.units[0].type_aliases.is_empty(), "{source}");
        assert!(
            syntax
                .classes
                .iter()
                .all(|class| class.type_aliases.is_empty()),
            "{source}"
        );
    }
}

/// What A.2.11 and its footnotes do not derive is a syntax error.
#[test]
fn malformed_covergroups_are_syntax_errors() {
    let rejected = |sources: &[String]| {
        for source in sources {
            assert!(
                Declarations::parse(source, Path::new("invalid-coverage.sv")).is_err(),
                "{source}"
            );
        }
    };
    // Declarations of any scope.
    for covergroup in [
        // Footnote 28: every formal is named.
        "covergroup cg(int); endgroup",
        "covergroup cg with function sample(int); endgroup",
        // Footnote 29: only in a class.
        "covergroup extends cg; endgroup",
        "covergroup cg @(*); endgroup",
        "covergroup cg @*; endgroup",
        "covergroup cg @f(); endgroup",
        "covergroup cg with function wrong(); endgroup",
        "covergroup cg @@(begin); endgroup",
        "covergroup cg @@(start a); endgroup",
        "covergroup cg; coverpoint x;",
    ] {
        rejected(&[
            format!("{covergroup} module M; endmodule"),
            format!("module M; {covergroup} endmodule"),
        ]);
    }
    // The covergroup of a base class has neither formals nor an event
    // (19.4.1).
    for covergroup in [
        "covergroup extends cg(int x); endgroup",
        "covergroup extends cg @(posedge clk); endgroup",
        "covergroup extends cg with function sample(); endgroup",
        "covergroup extends; endgroup",
    ] {
        rejected(&[
            format!("class D extends B; {covergroup} endclass module M; endmodule"),
            format!("module M; class D extends B; {covergroup} endclass endmodule"),
        ]);
    }
    for items in [
        // Items and their separators.
        "coverpoint x; ;",
        "coverpoint x { bins b = {1}; };",
        "coverpoint x { bins b = {1} }",
        "coverpoint x { bins b = {1}; ",
        "coverpoint x { nonsense b = {1}; }",
        "coverpoint x iff en;",
        "coverpoint x { bins b = {1} iff en; }",
        "cross a;",
        "cross a, b iff en;",
        "option.weight = ;",
        "type_option.weight = x++;",
        "coverpoint x { type_option.weight = x++; }",
        // Value ranges.
        "coverpoint x { bins b = {}; }",
        "coverpoint x { bins b = {1,}; }",
        "coverpoint x { bins b = {[$:$]}; }",
        "coverpoint x { bins b = {[1]}; }",
        "coverpoint x { bins b = {[1:2:3]}; }",
        "coverpoint x { bins b = {[$ +/- 1]}; }",
        "coverpoint x { bins b = {[1 +%- $]}; }",
        "coverpoint x { bins b = {[1 +/- ]}; }",
        "coverpoint x { bins b = {1} with item; }",
        // The array and `wildcard` forms each alternative has.
        "coverpoint x { bins b[2] = (1 => 2); }",
        "coverpoint x { bins b[] = default sequence; }",
        "coverpoint x { bins b[2] = default sequence; }",
        "coverpoint x { wildcard bins b = default; }",
        "coverpoint x { wildcard bins b = default sequence; }",
        "coverpoint x { bins b[ = {1}; }",
        // Transitions: the repetition is of the whole range list.
        "coverpoint x { bins b = (); }",
        "coverpoint x { bins b = (1 =>); }",
        "coverpoint x { bins b = (1[*2], 3 => 4); }",
        "coverpoint x { bins b = (1 [* ]); }",
        "coverpoint x { bins b = (1 [-> 2:]); }",
        "coverpoint x { bins b = (1 [* 2:3:4]); }",
        "coverpoint x { bins b = (1 => 2) (3 => 4); }",
        // Cross bins.
        "cross a, b { wildcard bins w = binsof(a); }",
        "cross a, b { bins w[] = binsof(a); }",
        "cross a, b { bins w[2] = binsof(a); }",
        "cross a, b { task t(); endtask }",
        "cross a, b { bins w = binsof(a) }",
        // Select expressions.
        "cross a, b { bins w = !x; }",
        "cross a, b { bins w = !(binsof(a)); }",
        "cross a, b { bins w = binsof(a) matches 2; }",
        "cross a, b { bins w = binsof(a.b.c); }",
        "cross a, b { bins w = binsof(a) intersect {}; }",
        "cross a, b { bins w = binsof(a) intersect 1; }",
        "cross a, b { bins w = (binsof(a); }",
        "cross a, b { bins w = binsof(a)); }",
        "cross a, b { bins w = binsof(a) &&; }",
        "cross a, b { bins w = binsof(a) with a; }",
        "cross a, b { bins w = f(a) ? x : y; }",
        "cross a, b { bins w = (a > b) ? x : y; }",
        "cross a, b { bins w = binsof(a) ? x : y; }",
    ] {
        rejected(&[
            format!("covergroup cg; {items} endgroup module M; endmodule"),
            format!("module M; covergroup cg; {items} endgroup endmodule"),
            format!("module M; class C; covergroup cg; {items} endgroup endclass endmodule"),
        ]);
    }
    let rejection = |items: &str| {
        Declarations::parse(
            &format!("module M; covergroup cg; {items} endgroup endmodule"),
            Path::new("unsupported-coverage.sv"),
        )
        .unwrap_err()
    };
    // A form the grammar lacks but tools accept, and a conditional cross
    // set expression (19.6.1.4), whose condition reads as a select
    // expression of expressions, are unsupported syntax of crosses.
    for items in [
        "cross a, b { bins w = !(binsof(a) && binsof(b)); }",
        "cross a, b { bins w = f(a) ? x : y; }",
        "cross a, b { bins w = a > b ? x : y; }",
        "cross a, b { bins w = (a > b) ? x : y; }",
        "cross a, b { bins w = ((a > b) ? x : y); }",
        "cross a, b { bins w = (a > 1 && b < 2 || f(a)) ? x : y; }",
        "cross a, b { bins w = a > 1 && (b < 2) ? x : y; }",
    ] {
        let error = rejection(items);
        assert_eq!(error.kind, ParseErrorKind::UnsupportedSyntax, "{items}");
        assert!(
            error.to_string().contains(": covergroup cross "),
            "{items}: {error}"
        );
    }
    // A select condition is the condition of no expression.
    for items in [
        "cross a, b { bins w = binsof(a) ? x : y; }",
        "cross a, b { bins w = (binsof(a)) ? x : y; }",
        "cross a, b { bins w = (a > b && binsof(a)) ? x : y; }",
        "cross a, b { bins w = (x with (a > b)) ? x : y; }",
        "cross a, b { bins w = (x matches 2) ? x : y; }",
    ] {
        let error = rejection(items);
        assert_eq!(error.kind, ParseErrorKind::MalformedHeader, "{items}");
    }
}

/// A `resetall` between the items of a covergroup, of a coverpoint or of a
/// cross is one inside the design element that contains the covergroup
/// (IEEE 1800-2023 22.3), and none in a covergroup of the compilation unit.
#[test]
fn resetall_in_a_covergroup_of_a_design_element_is_rejected() {
    let covergroups = [
        "covergroup cg;\n`resetall\ncoverpoint x; endgroup",
        "covergroup cg; coverpoint x;\n`resetall\nendgroup",
        "covergroup cg; coverpoint x {\n`resetall\nbins a = {1}; } endgroup",
        "covergroup cg; coverpoint x { bins a = {1};\n`resetall\n} endgroup",
        "covergroup cg; a: coverpoint x; b: coverpoint y; \
         cross a, b {\n`resetall\nbins s = binsof(a); } endgroup",
    ];
    for covergroup in covergroups {
        for source in [
            format!("module M; logic x, y; {covergroup} endmodule"),
            format!("package P; logic x, y; {covergroup} endpackage"),
            format!("module M; logic x, y; if (1) begin : g {covergroup} end endmodule"),
            format!("module M; class C; logic x, y; {covergroup} endclass endmodule"),
        ] {
            let error = Declarations::parse(&source, Path::new("reset-coverage.sv")).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("resetall directive inside a design element"),
                "{source}: {error}"
            );
        }
        for source in [
            format!("logic x, y; {covergroup} module M; endmodule"),
            format!("class C; logic x, y; {covergroup} endclass module M; endmodule"),
        ] {
            assert_eq!(parse(&source).covergroups.len(), 1, "{source}");
        }
    }
    // The directive would otherwise take effect inside the module: the
    // implicit net that `default_nettype none` forbids stays forbidden.
    let source = "`default_nettype none\n\
                  module M; logic x; covergroup cg;\n`resetall\ncoverpoint x; endgroup \
                  assign undeclared = x; endmodule";
    let error = Declarations::parse(source, Path::new("reset-coverage.sv")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("resetall directive inside a design element"),
        "{error}"
    );
    // Other directives stay items of a covergroup.
    let syntax = parse(
        "module M; logic x; covergroup cg;\n`celldefine\ncoverpoint x {\n`endcelldefine\n\
         bins a = {1}; } endgroup endmodule",
    );
    assert_eq!(syntax.covergroups.len(), 1);
}

/// A tolerance range of a set membership (11.4.13) stays unsupported
/// syntax, now that its operator is one token.
#[test]
fn tolerance_ranges_outside_covergroups_stay_unsupported() {
    for operator in ["+/-", "+%-"] {
        for source in [
            format!(
                "module M; logic [7:0] x, a, b; wire y = x inside {{[a {operator} b]}}; endmodule"
            ),
            format!(
                "module M; logic [7:0] x; initial if (x inside {{[8{operator}1]}}) x = 0; endmodule"
            ),
        ] {
            let error = Declarations::parse(&source, Path::new("tolerance.sv")).unwrap_err();
            assert_eq!(error.kind, ParseErrorKind::UnsupportedSyntax, "{source}");
        }
    }
}
