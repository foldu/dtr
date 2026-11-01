//! Tokens: the lexical layer of §3.3–§3.7.
//!
//! A line is lexed on its own, because §3.2 makes the line the unit of a
//! statement. The one piece of state that crosses a line boundary is a `/*`
//! comment, which is why [`lex_line`] takes it by reference and returns it.
//!
//! Everything the manual gives a spelling to gets a token: decimal and `$`
//! hexadecimal numbers (§3.6), quoted strings and `#`/`#$` character codes
//! (§3.7), labels (`:name`), and the operator table of §5.2. Two things are
//! worth knowing about the shape of the result:
//!
//! * **A string is the concatenation the source wrote.** `'abc'#$0d#$0a'def'`
//!   is one token, because §3.7 says adjacent pieces are one string — but only
//!   when nothing separates them, so `send 'a' 'b'` stays two parameters. That
//!   is what [`Tok::space`] is for.
//! * **`=` is one token with two meanings.** A statement that assigns is
//!   recognised by its shape (an identifier, then `=`), and the same token is
//!   the equality operator inside an expression (§5.3) — the evaluator decides
//!   which, and `==` is a separate token so nothing is lost.

use std::fmt;

/// The operators of §5.2, in the manual's spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `not`, `~`
    BitNot,
    /// `!`
    Not,
    Mul,
    Div,
    Mod,
    Add,
    Sub,
    /// `>>` — arithmetic, which replicates the sign bit.
    Shr,
    /// `<<`
    Shl,
    /// `>>>` — logical, which does not.
    UShr,
    /// `and`, `&`
    BitAnd,
    /// `or`, `|`
    BitOr,
    /// `xor`, `^`
    BitXor,
    Lt,
    Gt,
    Le,
    Ge,
    /// `==`
    Eq,
    /// `<>`, `!=`
    Ne,
    /// `&&`
    And,
    /// `||`
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// A command name, variable name, or one of the word operators when it
    /// appears as an operator.
    Ident(String),
    /// `:name`, only ever the first token of a line.
    Label(String),
    /// A decimal or `$` hexadecimal constant (§3.6).
    Number(i64),
    /// A string constant, already concatenated from adjacent pieces (§3.7).
    Str(String),
    Op(Op),
    /// `=` — assignment in a statement, equality in an expression.
    Equals,
    LParen,
    RParen,
    LBracket,
    RBracket,
}

/// A token and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tok {
    pub kind: Token,
    /// Whether whitespace preceded it. Parameters are separated by whitespace,
    /// so this is what tells one argument from a continuation of the last.
    pub space: bool,
    /// Zero-based character column, for error messages.
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexError {
    /// A quoted string not closed on its line. §3.7 has no escapes, so a quote
    /// always closes it and this is always a mistake.
    UnterminatedString { col: usize },
    /// `/*` with no `*/` before the end of the file.
    UnterminatedComment,
    /// Nothing after `$`, `#` or `#$`.
    EmptyNumber { col: usize },
    /// Digits that are not a number in the base the prefix asked for.
    BadNumber { col: usize },
    /// A constant outside `i32`, which is the only integer type (§4.1).
    NumberOutOfRange { col: usize },
    /// `#0`, which would be NUL; §3.7 forbids it in a string.
    NulCharacter { col: usize },
    /// A character that cannot begin a token.
    Unexpected { ch: char, col: usize },
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedString { col } => write!(f, "column {col}: unterminated string"),
            Self::UnterminatedComment => write!(f, "unterminated /* comment"),
            Self::EmptyNumber { col } => write!(f, "column {col}: no digits after the prefix"),
            Self::BadNumber { col } => write!(f, "column {col}: invalid digit"),
            Self::NumberOutOfRange { col } => {
                write!(f, "column {col}: constant does not fit in 32 bits")
            }
            Self::NulCharacter { col } => write!(f, "column {col}: NUL is not allowed in a string"),
            Self::Unexpected { ch, col } => write!(f, "column {col}: unexpected {ch:?}"),
        }
    }
}

impl std::error::Error for LexError {}

/// Lex one line.
///
/// `in_comment` is `true` when the previous line ended inside a `/*` comment,
/// and is left `true` if this one does too.
pub fn lex_line(chars: &[char], in_comment: &mut bool) -> Result<Vec<Tok>, LexError> {
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    let mut space = true;

    while i < chars.len() {
        let c = chars[i];

        if *in_comment {
            if c == '*' && chars.get(i + 1) == Some(&'/') {
                *in_comment = false;
                i += 2;
                space = true;
            } else {
                i += 1;
            }
            continue;
        }

        if c.is_whitespace() {
            space = true;
            i += 1;
            continue;
        }

        // §3.3: `;` to end of line, but only outside a string — which is why
        // strings are lexed below rather than by a separate pass.
        if c == ';' {
            break;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            *in_comment = true;
            i += 2;
            continue;
        }

        let col = i;

        // §3.7: a string is quoted with either mark, matched, and the other
        // mark is an ordinary character inside it.
        if c == '\'' || c == '"' {
            let quote = c;
            let mut text = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    None => return Err(LexError::UnterminatedString { col }),
                    Some(&ch) if ch == quote => {
                        i += 1;
                        break;
                    }
                    Some(&ch) => {
                        text.push(ch);
                        i += 1;
                    }
                }
            }
            push_possibly_glued(&mut out, Token::Str(text), space, col);
            space = false;
            continue;
        }

        // §3.7: `#65`, `#$41`, one character.
        if c == '#' {
            let (radix, start) = if chars.get(i + 1) == Some(&'$') {
                (16, i + 2)
            } else {
                (10, i + 1)
            };
            let (value, next) = number_after(chars, start, radix)?;
            let code = u32::try_from(value).map_err(|_| LexError::NumberOutOfRange { col })?;
            let ch = char::from_u32(code).ok_or(LexError::NulCharacter { col })?;
            if ch == '\0' {
                return Err(LexError::NulCharacter { col });
            }
            push_possibly_glued(&mut out, Token::Str(ch.to_string()), space, col);
            i = next;
            space = false;
            continue;
        }

        // §3.6: `$` begins a hexadecimal constant.
        if c == '$' {
            let (value, next) = number_after(chars, i + 1, 16)?;
            push(&mut out, Token::Number(value), space, col);
            i = next;
            space = false;
            continue;
        }

        // §3.4: a label is a colon and a name, and only at the start of a line.
        if c == ':' && out.is_empty() {
            let mut name = String::new();
            let mut j = i + 1;
            while let Some(&ch) = chars.get(j) {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    name.push(ch);
                    j += 1;
                } else {
                    break;
                }
            }
            if name.is_empty() {
                return Err(LexError::Unexpected { ch: ':', col });
            }
            push(&mut out, Token::Label(name), space, col);
            i = j;
            space = false;
            continue;
        }

        if c.is_ascii_digit() {
            let mut j = i;
            while chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
                j += 1;
            }
            let text: String = chars[i..j].iter().collect();
            let value: i64 = text.parse().map_err(|_| LexError::NumberOutOfRange { col })?;
            // §4.1: the only integer type is signed 32-bit, so a constant that
            // does not fit is refused rather than truncated.
            i32::try_from(value).map_err(|_| LexError::NumberOutOfRange { col })?;
            push(&mut out, Token::Number(value), space, col);
            i = j;
            space = false;
            continue;
        }

        if is_ident_start(c) {
            let mut j = i;
            while chars.get(j).is_some_and(|&c| is_ident_continue(c)) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let kind = match word.as_str() {
                "and" => Token::Op(Op::BitAnd),
                "or" => Token::Op(Op::BitOr),
                "xor" => Token::Op(Op::BitXor),
                "not" => Token::Op(Op::BitNot),
                _ => Token::Ident(word),
            };
            push(&mut out, kind, space, col);
            i = j;
            space = false;
            continue;
        }

        let (kind, len) = operator(chars, i)?;
        push(&mut out, kind, space, col);
        i += len;
        space = false;
    }

    Ok(out)
}

fn push(out: &mut Vec<Tok>, kind: Token, space: bool, col: usize) {
    out.push(Tok { kind, space, col });
}

/// §3.7: `'abc'#$0d'def'` is one string. Only pieces with nothing between them
/// join, so `send 'a' 'b'` is still two parameters.
fn push_possibly_glued(out: &mut Vec<Tok>, kind: Token, space: bool, col: usize) {
    if !space {
        if let Some(Tok { kind: Token::Str(prev), .. }) = out.last_mut() {
            if let Token::Str(add) = &kind {
                prev.push_str(add);
                return;
            }
        }
    }
    push(out, kind, space, col);
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Read the digits after a `$` or `#`, returning the value and the index after
/// them.
fn number_after(chars: &[char], start: usize, radix: u32) -> Result<(i64, usize), LexError> {
    let col = start;
    let mut j = start;
    let mut digits = String::new();
    while let Some(&c) = chars.get(j) {
        let ok = if radix == 16 {
            c.is_ascii_hexdigit()
        } else {
            c.is_ascii_digit()
        };
        if !ok {
            break;
        }
        digits.push(c);
        j += 1;
    }
    if digits.is_empty() {
        return Err(LexError::EmptyNumber { col });
    }
    let value = i64::from_str_radix(&digits, radix).map_err(|_| LexError::BadNumber { col })?;
    Ok((value, j))
}

/// Longest match first, so `>>>` does not win over `>>` and `&&` does not read
/// as two bitwise ANDs.
fn operator(chars: &[char], i: usize) -> Result<(Token, usize), LexError> {
    let at = |n: usize| chars.get(i + n).copied();

    if at(0) == Some('>') && at(1) == Some('>') && at(2) == Some('>') {
        return Ok((Token::Op(Op::UShr), 3));
    }

    let pair = match (at(0), at(1)) {
        (Some('>'), Some('>')) => Some((Token::Op(Op::Shr), 2)),
        (Some('<'), Some('<')) => Some((Token::Op(Op::Shl), 2)),
        (Some('<'), Some('=')) => Some((Token::Op(Op::Le), 2)),
        (Some('>'), Some('=')) => Some((Token::Op(Op::Ge), 2)),
        (Some('<'), Some('>')) => Some((Token::Op(Op::Ne), 2)),
        (Some('!'), Some('=')) => Some((Token::Op(Op::Ne), 2)),
        (Some('='), Some('=')) => Some((Token::Op(Op::Eq), 2)),
        (Some('&'), Some('&')) => Some((Token::Op(Op::And), 2)),
        (Some('|'), Some('|')) => Some((Token::Op(Op::Or), 2)),
        _ => None,
    };
    if let Some(found) = pair {
        return Ok(found);
    }

    let single = match chars[i] {
        '*' => Token::Op(Op::Mul),
        '/' => Token::Op(Op::Div),
        '%' => Token::Op(Op::Mod),
        '+' => Token::Op(Op::Add),
        '-' => Token::Op(Op::Sub),
        '<' => Token::Op(Op::Lt),
        '>' => Token::Op(Op::Gt),
        '&' => Token::Op(Op::BitAnd),
        '|' => Token::Op(Op::BitOr),
        '^' => Token::Op(Op::BitXor),
        '~' => Token::Op(Op::BitNot),
        '!' => Token::Op(Op::Not),
        '=' => Token::Equals,
        '(' => Token::LParen,
        ')' => Token::RParen,
        '[' => Token::LBracket,
        ']' => Token::RBracket,
        ch => return Err(LexError::Unexpected { ch, col: i }),
    };
    Ok((single, 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(text: &str) -> Vec<Token> {
        let chars: Vec<char> = text.chars().collect();
        let mut in_comment = false;
        lex_line(&chars, &mut in_comment)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn the_two_quote_styles_are_the_same_token() {
        assert_eq!(lex("'a'"), [Token::Str("a".into())]);
        assert_eq!(lex("\"a\""), [Token::Str("a".into())]);
    }

    #[test]
    fn the_other_quote_is_ordinary_inside_a_string() {
        assert_eq!(lex("\"I can't\""), [Token::Str("I can't".into())]);
        assert_eq!(lex("'she said \"hi\"'"), [Token::Str("she said \"hi\"".into())]);
    }

    #[test]
    fn a_semicolon_inside_a_string_is_not_a_comment() {
        assert_eq!(lex("'a;b' ; trailing"), [Token::Str("a;b".into())]);
    }

    #[test]
    fn a_block_comment_ends_mid_line_and_code_continues() {
        assert_eq!(lex("/* no */send"), [Token::Ident("send".into())]);
    }

    #[test]
    fn a_block_comment_carries_to_the_next_line() {
        let mut in_comment = false;
        lex_line(&"/* start".chars().collect::<Vec<_>>(), &mut in_comment).unwrap();
        assert!(in_comment);
        let toks = lex_line(&"*/ send".chars().collect::<Vec<_>>(), &mut in_comment).unwrap();
        assert!(!in_comment);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, Token::Ident("send".into()));
    }

    #[test]
    fn adjacent_string_pieces_are_one_string() {
        assert_eq!(
            lex("'cat'#13#10"),
            [Token::Str("cat\r\n".into())],
        );
        assert_eq!(lex("'abc'#$0d#$0a'def'"), [Token::Str("abc\r\ndef".into())]);
    }

    #[test]
    fn a_space_between_pieces_keeps_them_apart() {
        let toks = lex("'a' 'b'");
        assert_eq!(toks, [Token::Str("a".into()), Token::Str("b".into())]);
    }

    #[test]
    fn hex_and_decimal_constants() {
        assert_eq!(lex("$3a"), [Token::Number(0x3a)]);
        assert_eq!(lex("$10F"), [Token::Number(0x10f)]);
        assert_eq!(lex("123"), [Token::Number(123)]);
        assert_eq!(
            lex("-11"),
            [Token::Op(Op::Sub), Token::Number(11)],
            "a sign is an operator, which is what the negative-constant trap is about"
        );
    }

    #[test]
    fn the_word_operators_are_operators() {
        assert_eq!(
            lex("A and not B"),
            [
                Token::Ident("A".into()),
                Token::Op(Op::BitAnd),
                Token::Op(Op::BitNot),
                Token::Ident("B".into()),
            ]
        );
        // A word that merely starts like one is a name.
        assert_eq!(lex("android"), [Token::Ident("android".into())]);
    }

    #[test]
    fn shifts_take_the_longest_match() {
        assert_eq!(lex(">>>"), [Token::Op(Op::UShr)]);
        assert_eq!(lex(">>"), [Token::Op(Op::Shr)]);
        assert_eq!(lex("<="), [Token::Op(Op::Le)]);
        assert_eq!(lex("<>"), [Token::Op(Op::Ne)]);
    }

    #[test]
    fn a_label_is_only_a_label_at_the_start_of_a_line() {
        assert_eq!(lex(":100"), [Token::Label("100".into())]);
        assert_eq!(lex("  :retry"), [Token::Label("retry".into())]);
    }

    #[test]
    fn an_unterminated_string_is_an_error() {
        let chars: Vec<char> = "'abc".chars().collect();
        let mut in_comment = false;
        assert!(matches!(
            lex_line(&chars, &mut in_comment),
            Err(LexError::UnterminatedString { .. })
        ));
    }

    #[test]
    fn nul_cannot_be_expressed() {
        let chars: Vec<char> = "#0".chars().collect();
        let mut in_comment = false;
        assert!(matches!(
            lex_line(&chars, &mut in_comment),
            Err(LexError::NulCharacter { .. })
        ));
    }

    #[test]
    fn a_constant_past_32_bits_is_refused() {
        let chars: Vec<char> = "4294967296".chars().collect();
        let mut in_comment = false;
        assert!(matches!(
            lex_line(&chars, &mut in_comment),
            Err(LexError::NumberOutOfRange { .. })
        ));
    }
}
