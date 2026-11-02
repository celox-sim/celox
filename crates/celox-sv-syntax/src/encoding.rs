use std::{io, path::Path};

use crate::{SourceMap, TokenKind, lex};

/// Encoding policy for SystemVerilog source files and included headers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceEncoding {
    /// Require valid UTF-8 throughout the file.
    Utf8,
    /// Accept legacy bytes in comments, replacing each invalid byte with a
    /// space. Valid UTF-8 and byte offsets are preserved. Enabled by default.
    #[default]
    NonUtf8Comments,
}

impl SourceEncoding {
    pub fn read(self, path: &Path) -> io::Result<String> {
        self.decode(std::fs::read(path)?)
    }

    pub fn decode(self, bytes: Vec<u8>) -> io::Result<String> {
        let error = match String::from_utf8(bytes) {
            Ok(text) => return Ok(text),
            Err(error) => error,
        };
        if self == Self::Utf8 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, error));
        }
        let mut bytes = error.into_bytes();
        let mut invalid = Vec::new();
        let mut at = 0;
        while let Err(error) = std::str::from_utf8(&bytes[at..]) {
            let start = at + error.valid_up_to();
            let end = start + error.error_len().unwrap_or(bytes.len() - start);
            invalid.push(start);
            bytes[start..end].fill(b' ');
            at = end;
        }
        let text = String::from_utf8(bytes).expect("invalid UTF-8 bytes were replaced with spaces");
        // Use the source lexer so strings, escaped identifiers and macro
        // quotes cannot disguise non-comment bytes as a comment.
        let mut sources = SourceMap::default();
        let file = sources.add("<source encoding>", text.as_str());
        let tokens = lex(&sources, file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        for offset in invalid {
            let index = tokens.partition_point(|token| token.origin.spelling.end <= offset);
            if !tokens.get(index).is_some_and(|token| {
                token.kind == TokenKind::Comment && token.origin.spelling.range().contains(&offset)
            }) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("non-UTF-8 source byte outside a comment at byte {offset}"),
                ));
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_comments_preserve_utf8_and_source_offsets() {
        let bytes = b"// Copyright \xa9, \xc3\xa9\n/* \xff\xfe */ module M; endmodule".to_vec();
        let text = SourceEncoding::default().decode(bytes.clone()).unwrap();
        assert_eq!(text.len(), bytes.len());
        assert_eq!(text, "// Copyright  , é\n/*    */ module M; endmodule");
        assert!(SourceEncoding::Utf8.decode(bytes).is_err());
        let valid = "// 日本語\nmodule M; endmodule";
        assert_eq!(
            SourceEncoding::default()
                .decode(valid.as_bytes().to_vec())
                .unwrap(),
            valid
        );
    }

    #[test]
    fn invalid_bytes_in_code_and_strings_remain_errors() {
        for bytes in [
            b"module \xff; endmodule".as_slice(),
            b"module M; initial $display(\"// \xff\"); endmodule",
            b"module M; wire \\/*\xff ; endmodule",
            b"// ok\nmodule M; initial $display(\"/* \xff */\"); endmodule",
        ] {
            assert!(SourceEncoding::default().decode(bytes.to_vec()).is_err());
        }
    }
}
