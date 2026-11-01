//! Lines: the four kinds of §3.2, and the block structure of §6.
//!
//! A line is classified, not parsed. §3.2 says a line holds one of four
//! things — nothing, a label, an assignment, or a command with parameters — and
//! that is all this module decides. Parameters stay as token groups so the
//! evaluator can parse them as expressions when it knows what the command
//! wants; nothing here guesses what a parameter means.
//!
//! The one structural question answered here is block nesting, because it can
//! be answered from command names alone: `if … then` is closed by `endif`,
//! `for` by `next`, `while` by `endwhile`, `until` by `enduntil`, `do` by
//! `loop`. `if` in its single-line form (`if A>1 goto label`) has no `then` and
//! therefore opens nothing.

use std::fmt;

use crate::lex::{lex_line, LexError, Tok, Token};
use crate::source;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    /// An empty line, or one holding only a comment.
    Blank,
    /// `:name`
    Label(String),
    /// `target = value` or `target[index] = value`.
    Assign {
        target: String,
        index: Option<Vec<Tok>>,
        value: Vec<Tok>,
    },
    /// Anything else: a command name and its whitespace-separated parameters.
    Command { name: String, args: Vec<Vec<Tok>> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// One-based, for error messages.
    pub number: usize,
    pub kind: LineKind,
}

#[derive(Debug)]
pub enum ScanError {
    Lex { line: usize, error: LexError },
    /// A `/*` that is never closed, reported at the last line.
    UnterminatedComment { line: usize },
    /// A line that starts with something that cannot start one.
    UnexpectedLineStart { line: usize },
    /// `name[` with no `]`.
    UnclosedBracket { line: usize },
    /// `name[0]` with no `=` after it.
    ExpectedEquals { line: usize },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lex { line, error } => write!(f, "line {line}: {error}"),
            Self::UnterminatedComment { line } => {
                write!(f, "line {line}: unterminated /* comment")
            }
            Self::UnexpectedLineStart { line } => write!(f, "line {line}: cannot start a statement"),
            Self::UnclosedBracket { line } => write!(f, "line {line}: missing ]"),
            Self::ExpectedEquals { line } => write!(f, "line {line}: missing = after the index"),
        }
    }
}

impl std::error::Error for ScanError {}

/// Split a macro's text into classified lines.
pub fn scan(text: &str) -> Result<Vec<Line>, ScanError> {
    let raw = source::lines(text);
    let mut out = Vec::with_capacity(raw.len());
    let mut in_comment = false;

    for (index, text) in raw.iter().enumerate() {
        let number = index + 1;
        let chars: Vec<char> = text.chars().collect();
        let toks = lex_line(&chars, &mut in_comment)
            .map_err(|error| ScanError::Lex { line: number, error })?;
        let kind = classify(&toks, number)?;
        out.push(Line { number, kind });
    }

    if in_comment {
        return Err(ScanError::UnterminatedComment {
            line: raw.len().max(1),
        });
    }
    Ok(out)
}

fn classify(toks: &[Tok], line: usize) -> Result<LineKind, ScanError> {
    let Some(first) = toks.first() else {
        return Ok(LineKind::Blank);
    };

    match &first.kind {
        // Anything after a label would be unreachable, so it is ignored rather
        // than rejected: a label is a position, not a statement.
        Token::Label(name) => Ok(LineKind::Label(name.clone())),

        Token::Ident(name) => match toks.get(1).map(|t| &t.kind) {
            Some(Token::Equals) => Ok(LineKind::Assign {
                target: name.clone(),
                index: None,
                value: toks[2..].to_vec(),
            }),

            Some(Token::LBracket) => {
                let mut depth = 0usize;
                let mut close = None;
                for (i, tok) in toks.iter().enumerate().skip(1) {
                    match tok.kind {
                        Token::LBracket => depth += 1,
                        Token::RBracket => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(i);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let close = close.ok_or(ScanError::UnclosedBracket { line })?;
                if toks.get(close + 1).map(|t| &t.kind) != Some(&Token::Equals) {
                    return Err(ScanError::ExpectedEquals { line });
                }
                Ok(LineKind::Assign {
                    target: name.clone(),
                    index: Some(toks[2..close].to_vec()),
                    value: toks[close + 2..].to_vec(),
                })
            }

            _ => Ok(LineKind::Command {
                name: name.clone(),
                args: groups(&toks[1..]),
            }),
        },

        _ => Err(ScanError::UnexpectedLineStart { line }),
    }
}

/// Split parameters on whitespace. A token with `space` set starts a new one,
/// which is how `send 'a' 'b'` differs from `send 'a''b'`.
fn groups(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut out: Vec<Vec<Tok>> = Vec::new();
    for tok in toks {
        if out.is_empty() || tok.space {
            out.push(vec![tok.clone()]);
        } else if let Some(last) = out.last_mut() {
            last.push(tok.clone());
        }
    }
    out
}

/// Which loop or branch a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    If,
    While,
    Until,
    Do,
    For,
}

impl Open {
    fn keyword(self) -> &'static str {
        match self {
            Self::If => "if",
            Self::While => "while",
            Self::Until => "until",
            Self::Do => "do",
            Self::For => "for",
        }
    }

    fn closer(self) -> &'static str {
        match self {
            Self::If => "endif",
            Self::While => "endwhile",
            Self::Until => "enduntil",
            Self::Do => "loop",
            Self::For => "next",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    /// A closer with nothing open, or the wrong kind open.
    Unexpected { line: usize, what: &'static str },
    /// An opener that is still open at the end of the file.
    Unclosed { line: usize, what: &'static str },
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unexpected { line, what } => {
                write!(f, "line {line}: {what} closes nothing that is open")
            }
            Self::Unclosed { line, what } => {
                write!(f, "line {line}: the block opened here is never closed with {what}")
            }
        }
    }
}

impl std::error::Error for BlockError {}

/// Check that every opened block is closed by its own keyword.
pub fn check_blocks(lines: &[Line]) -> Result<(), BlockError> {
    let mut stack: Vec<(Open, usize)> = Vec::new();

    for line in lines {
        let LineKind::Command { name, args } = &line.kind else {
            continue;
        };

        match name.as_str() {
            "if" if opens_if(args) => stack.push((Open::If, line.number)),

            // `elseif` and `else` continue the `if` that is already open, so
            // they only have to find one.
            "elseif" | "else" => {
                if !matches!(stack.last(), Some((Open::If, _))) {
                    return Err(BlockError::Unexpected {
                        line: line.number,
                        what: "else/elseif",
                    });
                }
            }

            "while" => stack.push((Open::While, line.number)),
            "until" => stack.push((Open::Until, line.number)),
            "do" => stack.push((Open::Do, line.number)),
            "for" => stack.push((Open::For, line.number)),

            "endif" => close(&mut stack, Open::If, line.number)?,
            "endwhile" => close(&mut stack, Open::While, line.number)?,
            "enduntil" => close(&mut stack, Open::Until, line.number)?,
            "loop" => close(&mut stack, Open::Do, line.number)?,
            "next" => close(&mut stack, Open::For, line.number)?,

            _ => {}
        }
    }

    if let Some((open, line)) = stack.pop() {
        return Err(BlockError::Unclosed {
            line,
            what: open.closer(),
        });
    }
    Ok(())
}

fn close(stack: &mut Vec<(Open, usize)>, want: Open, line: usize) -> Result<(), BlockError> {
    match stack.pop() {
        Some((open, _)) if open == want => Ok(()),
        _ => Err(BlockError::Unexpected {
            line,
            what: want.closer(),
        }),
    }
}

/// §6.1: the block form has `then`; the single-line form does not.
fn opens_if(args: &[Vec<Tok>]) -> bool {
    args.iter().any(|group| {
        group
            .iter()
            .any(|tok| matches!(&tok.kind, Token::Ident(word) if word == "then"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<LineKind> {
        scan(text).unwrap().into_iter().map(|l| l.kind).collect()
    }

    #[test]
    fn the_four_line_kinds() {
        let k = kinds(
            "; a comment\n\
             \n\
             :retry\n\
             A = 33\n\
             sendln 'cat'\n",
        );
        assert_eq!(k[0], LineKind::Blank);
        assert_eq!(k[1], LineKind::Blank);
        assert_eq!(k[2], LineKind::Label("retry".into()));
        assert_eq!(
            k[3],
            LineKind::Assign {
                target: "A".into(),
                index: None,
                value: vec![Tok {
                    kind: Token::Number(33),
                    space: true,
                    col: 4
                }]
            }
        );
        assert!(matches!(&k[4], LineKind::Command { name, args } if name == "sendln" && args.len() == 1));
    }

    #[test]
    fn parameters_are_split_on_whitespace_only() {
        let k = kinds("sendln 'a' 'b'\n");
        let LineKind::Command { args, .. } = &k[0] else {
            panic!("not a command")
        };
        assert_eq!(args.len(), 2, "'a' and 'b' are two parameters");
    }

    #[test]
    fn an_indexed_assignment_is_one_statement() {
        let k = kinds("ar_int[i] = i\n");
        let LineKind::Assign { target, index, .. } = &k[0] else {
            panic!("not an assignment")
        };
        assert_eq!(target, "ar_int");
        assert_eq!(index.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn a_negative_number_is_a_subtraction_in_the_tokens() {
        // §5.4's trap: `for i 5 -1` is "5-1", so the third parameter is gone.
        let k = kinds("for i 5 -1\n");
        let LineKind::Command { args, .. } = &k[0] else {
            panic!("not a command")
        };
        assert_eq!(args.len(), 3, "i, 5-1 -- the third parameter is missing");
    }

    #[test]
    fn blocks_must_match() {
        assert!(check_blocks(&scan("if a then\nsendln 'x'\nendif\n").unwrap()).is_ok());
        assert!(check_blocks(&scan("for i 1 3\nnext\n").unwrap()).is_ok());
        assert!(check_blocks(&scan("while 1\nendwhile\n").unwrap()).is_ok());
        assert!(check_blocks(&scan("do\nloop while result = 2\n").unwrap()).is_ok());
        assert!(check_blocks(&scan("until i > 10\nenduntil\n").unwrap()).is_ok());

        let err = check_blocks(&scan("if a then\nsendln 'x'\n").unwrap()).unwrap_err();
        assert!(matches!(err, BlockError::Unclosed { what: "endif", .. }));

        let err = check_blocks(&scan("endif\n").unwrap()).unwrap_err();
        assert!(matches!(err, BlockError::Unexpected { what: "endif", .. }));

        let err = check_blocks(&scan("for i 1 3\nendwhile\n").unwrap()).unwrap_err();
        assert!(matches!(err, BlockError::Unexpected { what: "endwhile", .. }));
    }

    #[test]
    fn a_single_line_if_opens_nothing() {
        assert!(check_blocks(&scan("if A>1 goto label\n:label\n").unwrap()).is_ok());
        assert!(check_blocks(&scan("if result break\n").unwrap()).is_ok());
    }

    #[test]
    fn else_needs_an_if() {
        assert!(matches!(
            check_blocks(&scan("else\n").unwrap()),
            Err(BlockError::Unexpected { .. })
        ));
        assert!(check_blocks(&scan("if a then\nelse\nendif\n").unwrap()).is_ok());
    }
}
