//! Reserved identifier sets and directive state (IEEE 1800-2023, 22.14).
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum KeywordVersion {
    V1995,
    V2001NoConfig,
    V2001,
    V2005,
    Sv2005,
    Sv2009,
    #[default]
    Sv2012,
}

impl KeywordVersion {
    pub(super) fn from_specifier(specifier: &str) -> Option<Self> {
        Some(match specifier {
            "\"1364-1995\"" => Self::V1995,
            "\"1364-2001-noconfig\"" => Self::V2001NoConfig,
            "\"1364-2001\"" => Self::V2001,
            "\"1364-2005\"" => Self::V2005,
            "\"1800-2005\"" => Self::Sv2005,
            "\"1800-2009\"" => Self::Sv2009,
            // 2017 and 2023 add no reserved identifiers (22.14.9).
            "\"1800-2012\"" | "\"1800-2017\"" | "\"1800-2023\"" => Self::Sv2012,
            _ => return None,
        })
    }

    fn reserves(self, word: &str) -> bool {
        if matches!(
            word,
            "always"
                | "and"
                | "assign"
                | "begin"
                | "buf"
                | "bufif0"
                | "bufif1"
                | "case"
                | "casex"
                | "casez"
                | "cmos"
                | "deassign"
                | "default"
                | "defparam"
                | "disable"
                | "edge"
                | "else"
                | "end"
                | "endcase"
                | "endfunction"
                | "endmodule"
                | "endprimitive"
                | "endspecify"
                | "endtable"
                | "endtask"
                | "event"
                | "for"
                | "force"
                | "forever"
                | "fork"
                | "function"
                | "highz0"
                | "highz1"
                | "if"
                | "ifnone"
                | "initial"
                | "inout"
                | "input"
                | "integer"
                | "join"
                | "large"
                | "macromodule"
                | "medium"
                | "module"
                | "nand"
                | "negedge"
                | "nmos"
                | "nor"
                | "not"
                | "notif0"
                | "notif1"
                | "or"
                | "output"
                | "parameter"
                | "pmos"
                | "posedge"
                | "primitive"
                | "pull0"
                | "pull1"
                | "pulldown"
                | "pullup"
                | "rcmos"
                | "real"
                | "realtime"
                | "reg"
                | "release"
                | "repeat"
                | "rnmos"
                | "rpmos"
                | "rtran"
                | "rtranif0"
                | "rtranif1"
                | "scalared"
                | "small"
                | "specify"
                | "specparam"
                | "strong0"
                | "strong1"
                | "supply0"
                | "supply1"
                | "table"
                | "task"
                | "time"
                | "tran"
                | "tranif0"
                | "tranif1"
                | "tri"
                | "tri0"
                | "tri1"
                | "triand"
                | "trior"
                | "trireg"
                | "vectored"
                | "wait"
                | "wand"
                | "weak0"
                | "weak1"
                | "while"
                | "wire"
                | "wor"
                | "xnor"
                | "xor"
        ) {
            return true;
        }
        if self == Self::V2001NoConfig
            && matches!(
                word,
                "cell"
                    | "config"
                    | "design"
                    | "endconfig"
                    | "incdir"
                    | "include"
                    | "instance"
                    | "liblist"
                    | "library"
                    | "use"
            )
        {
            return false;
        }
        if self >= Self::V2001NoConfig
            && matches!(
                word,
                "automatic"
                    | "cell"
                    | "config"
                    | "design"
                    | "endconfig"
                    | "endgenerate"
                    | "generate"
                    | "genvar"
                    | "incdir"
                    | "include"
                    | "instance"
                    | "liblist"
                    | "library"
                    | "localparam"
                    | "noshowcancelled"
                    | "pulsestyle_ondetect"
                    | "pulsestyle_onevent"
                    | "showcancelled"
                    | "signed"
                    | "unsigned"
                    | "use"
            )
        {
            return true;
        }
        if self >= Self::V2005 && matches!(word, "uwire") {
            return true;
        }
        if self >= Self::Sv2005
            && matches!(
                word,
                "alias"
                    | "always_comb"
                    | "always_ff"
                    | "always_latch"
                    | "assert"
                    | "assume"
                    | "before"
                    | "bind"
                    | "bins"
                    | "binsof"
                    | "bit"
                    | "break"
                    | "byte"
                    | "chandle"
                    | "class"
                    | "clocking"
                    | "const"
                    | "constraint"
                    | "context"
                    | "continue"
                    | "cover"
                    | "covergroup"
                    | "coverpoint"
                    | "cross"
                    | "dist"
                    | "do"
                    | "endclass"
                    | "endclocking"
                    | "endgroup"
                    | "endinterface"
                    | "endpackage"
                    | "endprogram"
                    | "endproperty"
                    | "endsequence"
                    | "enum"
                    | "expect"
                    | "export"
                    | "extends"
                    | "extern"
                    | "final"
                    | "first_match"
                    | "foreach"
                    | "forkjoin"
                    | "iff"
                    | "ignore_bins"
                    | "illegal_bins"
                    | "import"
                    | "inside"
                    | "int"
                    | "interface"
                    | "intersect"
                    | "join_any"
                    | "join_none"
                    | "local"
                    | "logic"
                    | "longint"
                    | "matches"
                    | "modport"
                    | "new"
                    | "null"
                    | "package"
                    | "packed"
                    | "priority"
                    | "program"
                    | "property"
                    | "protected"
                    | "pure"
                    | "rand"
                    | "randc"
                    | "randcase"
                    | "randsequence"
                    | "ref"
                    | "return"
                    | "sequence"
                    | "shortint"
                    | "shortreal"
                    | "solve"
                    | "static"
                    | "string"
                    | "struct"
                    | "super"
                    | "tagged"
                    | "this"
                    | "throughout"
                    | "timeprecision"
                    | "timeunit"
                    | "type"
                    | "typedef"
                    | "union"
                    | "unique"
                    | "var"
                    | "virtual"
                    | "void"
                    | "wait_order"
                    | "wildcard"
                    | "with"
                    | "within"
            )
        {
            return true;
        }
        if self >= Self::Sv2009
            && matches!(
                word,
                "accept_on"
                    | "checker"
                    | "endchecker"
                    | "eventually"
                    | "global"
                    | "implies"
                    | "let"
                    | "nexttime"
                    | "reject_on"
                    | "restrict"
                    | "s_always"
                    | "s_eventually"
                    | "s_nexttime"
                    | "s_until"
                    | "s_until_with"
                    | "strong"
                    | "sync_accept_on"
                    | "sync_reject_on"
                    | "unique0"
                    | "until"
                    | "until_with"
                    | "untyped"
                    | "weak"
            )
        {
            return true;
        }
        if self >= Self::Sv2012
            && matches!(word, "implements" | "interconnect" | "nettype" | "soft")
        {
            return true;
        }
        false
    }
}

impl Parser<'_> {
    pub(super) fn reserved_identifier(&self, token: &Token) -> bool {
        token.kind == TokenKind::Identifier && self.reserved_word(&token.text)
    }

    pub(super) fn reserved_word(&self, word: &str) -> bool {
        self.keyword_versions
            .last()
            .copied()
            .unwrap_or_default()
            .reserves(word)
    }

    pub(super) fn keyword_directive(&mut self, name: &str) -> Result<bool> {
        match name {
            "begin_keywords" => {
                let version = KeywordVersion::from_specifier(self.peek())
                    .ok_or_else(|| self.error("invalid keyword version specifier"))?;
                self.keyword_versions.push(version);
                self.at += 1;
                Ok(true)
            }
            "end_keywords" => {
                self.keyword_versions.pop();
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
