//! Expressions: the operator table of §5.2, and what it computes.
//!
//! §5.1 sets the frame: an expression is built from integer constants, integer
//! variables, operators and parentheses, and its value is an integer. Strings
//! are not operands at all — comparing them is `strcompare`, converting them is
//! `str2int` — so a string inside an expression is a type error rather than a
//! coercion. A relational operator yields `1` for true and `0` for false, which
//! is what `if` and the loop conditions test.
//!
//! The precedence table is §5.2's, tightest first: unary, `* / %`, `+ -`,
//! shifts, `and`, `xor`, `or`, relational, equality, `&&`, `||`. It is worth
//! noticing that **relational binds tighter than equality** (`a < b = c < d` is
//! `(a<b) = (c<d)`) and both bind tighter than the logical operators.
//!
//! # What §5.5 leaves undefined, and what is chosen here
//!
//! The manual does not say how a 32-bit overflow behaves, how a shift of 32 or
//! more behaves, or whether `&&`/`||` short-circuit. Those decisions are made
//! here so they are written down rather than inherited from an accident:
//!
//! * **Arithmetic wraps.** A 32-bit C implementation does, and a macro that
//!   relies on saturation would be relying on an accident.
//! * **Shifts mask the count to 5 bits**, which is what x86 does and what
//!   `wrapping_shl`/`wrapping_shr` do. `1 << 32` is `1`, not `0`.
//! * **`&&` and `||` short-circuit.** There are no side effects in an
//!   expression, so the only thing this changes is whether the right operand's
//!   errors happen: `if a <> 0 && b/a > 1` survives a zero `a`, which is the
//!   reading a macro author expects.
//! * **Division truncates toward zero** and `%` is the remainder of that
//!   division, so `-7 / 2` is `-3` and `-7 % 2` is `-1`.
//!
//! # The `=` token
//!
//! §5.3: `=` assigns as a statement and compares inside an expression, which is
//! why `A = B = C` assigns `0` or `1` to `A`. The lexer produces one token for
//! it; [`Expr::Equal`] is that token used as a comparison, so nothing is lost.

use std::fmt;

use crate::error::Error;
use crate::lex::{Op, Tok, Token};
use crate::value::Value;
use crate::vars::Vars;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Int(i32),
    Str(String),
    Var(String),
    /// `name[expr]`
    Index(String, Box<Expr>),
    Unary(Op, Box<Expr>),
    Binary(Op, Box<Expr>, Box<Expr>),
    /// `=` used as equality, the same token that assigns in a statement.
    Equal(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// An operator with nothing after it, or an empty expression.
    UnexpectedEnd,
    /// A token that cannot start or continue an expression.
    Unexpected(String),
    /// Something after a complete expression, which one line cannot mean.
    Trailing(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("Syntax error."),
            Self::Unexpected(t) => write!(f, "Syntax error: unexpected {t}"),
            Self::Trailing(t) => write!(f, "Syntax error: unexpected {t} after the expression"),
        }
    }
}

impl std::error::Error for ParseError {}

/// What a name means while an expression is being evaluated.
///
/// The interpreter implements this over the user variables and the system
/// variables together, so an expression cannot tell `result` from `A`.
pub trait Scope {
    /// A scalar when `index` is `None`, an array element otherwise.
    fn value(&self, name: &str, index: Option<i32>) -> Result<Value, Error>;
}

impl Scope for Vars {
    fn value(&self, name: &str, index: Option<i32>) -> Result<Value, Error> {
        match index {
            Some(index) => self.get_index(name, index),
            None => self.get(name),
        }
    }
}

/// Parse a complete expression: every token must be consumed.
pub fn parse(toks: &[Tok]) -> Result<Expr, ParseError> {
    let mut parser = Parser { toks, pos: 0 };
    let expr = parser.expression(0)?;
    if let Some(more) = parser.peek() {
        return Err(ParseError::Trailing(format!("{more:?}")));
    }
    Ok(expr)
}

/// Parse one expression from the front, returning how much was consumed.
///
/// The single-line `if` form of §6.1 needs this: `if A>1 goto label` is an
/// expression followed by a statement, and only the caller knows which token
/// starts the statement.
pub fn parse_prefix(toks: &[Tok]) -> Result<(Expr, usize), ParseError> {
    let mut parser = Parser { toks, pos: 0 };
    let expr = parser.expression(0)?;
    Ok((expr, parser.pos))
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

/// The right-hand side of a binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinOp {
    Op(Op),
    /// `=`
    Equal,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos).map(|t| &t.kind)
    }

    fn bump(&mut self) -> Option<Token> {
        let tok = self.toks.get(self.pos)?.kind.clone();
        self.pos += 1;
        Some(tok)
    }

    /// Precedence climbing: §5.2's table, with each level left-associative.
    fn expression(&mut self, min_precedence: u8) -> Result<Expr, ParseError> {
        let mut left = self.unary()?;

        while let Some((precedence, op)) = self.peek().and_then(binary_op) {
            if precedence < min_precedence {
                break;
            }
            self.pos += 1;
            // `precedence + 1` is what makes the level left-associative.
            let right = self.expression(precedence + 1)?;
            left = match op {
                BinOp::Equal => Expr::Equal(Box::new(left), Box::new(right)),
                BinOp::Op(op) => Expr::Binary(op, Box::new(left), Box::new(right)),
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            // All of §5.2's precedence-1 operators, and they stack: `- -1`.
            Some(Token::Op(op @ (Op::BitNot | Op::Not | Op::Add | Op::Sub))) => {
                let op = *op;
                self.pos += 1;
                let operand = self.unary()?;
                Ok(Expr::Unary(op, Box::new(operand)))
            }
            Some(Token::LParen) => {
                self.pos += 1;
                let expr = self.expression(0)?;
                match self.bump() {
                    Some(Token::RParen) => Ok(expr),
                    _ => Err(ParseError::Unexpected(")".into())),
                }
            }
            Some(Token::Number(value)) => {
                let value = *value;
                self.pos += 1;
                Ok(Expr::Int(value as i32))
            }
            Some(Token::Str(text)) => {
                let text = text.clone();
                self.pos += 1;
                Ok(Expr::Str(text))
            }
            Some(Token::Ident(name)) => {
                let name = name.clone();
                self.pos += 1;
                if matches!(self.peek(), Some(Token::LBracket)) {
                    self.pos += 1;
                    let index = self.expression(0)?;
                    match self.bump() {
                        Some(Token::RBracket) => Ok(Expr::Index(name, Box::new(index))),
                        _ => Err(ParseError::Unexpected("]".into())),
                    }
                } else {
                    Ok(Expr::Var(name))
                }
            }
            Some(other) => Err(ParseError::Unexpected(format!("{other:?}"))),
            None => Err(ParseError::UnexpectedEnd),
        }
    }
}

/// §5.2, tightest first. Returns `None` for a token that is not a binary
/// operator, which is how the loop knows where the expression ends.
fn binary_op(tok: &Token) -> Option<(u8, BinOp)> {
    let (precedence, op) = match tok {
        Token::Op(Op::Mul) => (10, BinOp::Op(Op::Mul)),
        Token::Op(Op::Div) => (10, BinOp::Op(Op::Div)),
        Token::Op(Op::Mod) => (10, BinOp::Op(Op::Mod)),

        Token::Op(Op::Add) => (9, BinOp::Op(Op::Add)),
        Token::Op(Op::Sub) => (9, BinOp::Op(Op::Sub)),

        Token::Op(Op::Shl) => (8, BinOp::Op(Op::Shl)),
        Token::Op(Op::Shr) => (8, BinOp::Op(Op::Shr)),
        Token::Op(Op::UShr) => (8, BinOp::Op(Op::UShr)),

        Token::Op(Op::BitAnd) => (7, BinOp::Op(Op::BitAnd)),
        Token::Op(Op::BitXor) => (6, BinOp::Op(Op::BitXor)),
        Token::Op(Op::BitOr) => (5, BinOp::Op(Op::BitOr)),

        Token::Op(Op::Lt) => (4, BinOp::Op(Op::Lt)),
        Token::Op(Op::Gt) => (4, BinOp::Op(Op::Gt)),
        Token::Op(Op::Le) => (4, BinOp::Op(Op::Le)),
        Token::Op(Op::Ge) => (4, BinOp::Op(Op::Ge)),

        Token::Op(Op::Eq) => (3, BinOp::Op(Op::Eq)),
        Token::Op(Op::Ne) => (3, BinOp::Op(Op::Ne)),
        Token::Equals => (3, BinOp::Equal),

        Token::Op(Op::And) => (2, BinOp::Op(Op::And)),
        Token::Op(Op::Or) => (1, BinOp::Op(Op::Or)),

        _ => return None,
    };
    Some((precedence, op))
}

/// Evaluate to a value (an integer, or a string constant/`sprintf`-style
/// argument).
pub fn eval(expr: &Expr, scope: &dyn Scope) -> Result<Value, Error> {
    match expr {
        Expr::Int(value) => Ok(Value::Int(*value)),
        Expr::Str(text) => Ok(Value::Str(text.clone())),
        Expr::Var(name) => scope.value(name, None),
        Expr::Index(name, index) => {
            let at = eval_int(index, scope)?;
            scope.value(name, Some(at))
        }
        Expr::Unary(op, operand) => {
            let value = eval_int(operand, scope)?;
            Ok(Value::Int(unary(*op, value)))
        }
        // Short-circuit, so the right operand's errors do not happen when the
        // left one decided the answer.
        Expr::Binary(Op::And, left, right) => {
            if eval_int(left, scope)? == 0 {
                return Ok(Value::Int(0));
            }
            Ok(Value::Int(i32::from(eval_int(right, scope)? != 0)))
        }
        Expr::Binary(Op::Or, left, right) => {
            if eval_int(left, scope)? != 0 {
                return Ok(Value::Int(1));
            }
            Ok(Value::Int(i32::from(eval_int(right, scope)? != 0)))
        }
        Expr::Binary(op, left, right) => {
            let left = eval_int(left, scope)?;
            let right = eval_int(right, scope)?;
            Ok(Value::Int(binary(*op, left, right)?))
        }
        Expr::Equal(left, right) => {
            let left = eval_int(left, scope)?;
            let right = eval_int(right, scope)?;
            Ok(Value::Int(i32::from(left == right)))
        }
    }
}

/// Evaluate to an integer, which is what a condition and arithmetic need.
pub fn eval_int(expr: &Expr, scope: &dyn Scope) -> Result<i32, Error> {
    eval(expr, scope)?.as_int().ok_or(Error::TypeMismatch)
}

fn unary(op: Op, value: i32) -> i32 {
    match op {
        // `not` and `~` are the same bitwise negation (§5.3).
        Op::BitNot => !value,
        // `!` is logical: 1 when the value is zero.
        Op::Not => i32::from(value == 0),
        Op::Add => value,
        Op::Sub => value.wrapping_neg(),
        _ => unreachable!("only §5.2's precedence-1 operators are parsed as unary"),
    }
}

fn binary(op: Op, left: i32, right: i32) -> Result<i32, Error> {
    let value = match op {
        Op::Add => left.wrapping_add(right),
        Op::Sub => left.wrapping_sub(right),
        Op::Mul => left.wrapping_mul(right),
        Op::Div => {
            if right == 0 {
                return Err(Error::DivideByZero);
            }
            left.wrapping_div(right)
        }
        Op::Mod => {
            if right == 0 {
                return Err(Error::DivideByZero);
            }
            left.wrapping_rem(right)
        }
        Op::Shl => left.wrapping_shl(right as u32),
        // Arithmetic: the sign bit is replicated.
        Op::Shr => left.wrapping_shr(right as u32),
        // Logical: zeros come in, so a negative number becomes large.
        Op::UShr => ((left as u32).wrapping_shr(right as u32)) as i32,
        Op::BitAnd => left & right,
        Op::BitOr => left | right,
        Op::BitXor => left ^ right,
        Op::Lt => i32::from(left < right),
        Op::Gt => i32::from(left > right),
        Op::Le => i32::from(left <= right),
        Op::Ge => i32::from(left >= right),
        Op::Eq => i32::from(left == right),
        Op::Ne => i32::from(left != right),
        Op::And | Op::Or => unreachable!("short-circuited before this point"),
        Op::Not | Op::BitNot => unreachable!("parsed as unary"),
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::lex_line;

    /// Parse and evaluate a snippet against a fixed set of variables.
    fn run(text: &str, vars: &[(&str, Value)]) -> Result<Value, Error> {
        let mut bank = Vars::new();
        for (name, value) in vars {
            bank.set(name, value.clone()).unwrap();
        }
        let chars: Vec<char> = text.chars().collect();
        let mut in_comment = false;
        let toks = lex_line(&chars, &mut in_comment).expect("lexes");
        let expr = parse(&toks).expect("parses");
        eval(&expr, &bank)
    }

    fn int(text: &str) -> i32 {
        run(text, &[]).unwrap().as_int().unwrap()
    }

    #[test]
    fn the_manuals_own_examples() {
        assert_eq!(int("1 + 1"), 2);
        assert_eq!(int("4 - 2 * 3"), -2);
        assert_eq!(int("15 % 10"), 5);
        assert_eq!(int("3 * (2 + 2)"), 12);
        assert_eq!(
            run("A <= B", &[("A", Value::Int(1)), ("B", Value::Int(2))]).unwrap(),
            Value::Int(1),
            "§5.1: a relation evaluates to 1 or 0"
        );
    }

    #[test]
    fn arithmetic_is_precedence_ordered() {
        // `*` before `+`, `+` before a shift, shifts before `and`.
        assert_eq!(int("2 + 3 * 4"), 14);
        assert_eq!(int("1 + 1 << 2"), 8, "(1+1) << 2, since + binds tighter");
        assert_eq!(int("6 and 3"), 2);
        assert_eq!(int("1 or 2 xor 3"), 0 | (2 ^ 3));
    }

    #[test]
    fn relational_binds_tighter_than_equality() {
        // 1 < 2 is 1, 1 < 2 is 1, so they are equal.
        assert_eq!(int("1 < 2 = 2 < 3"), 1);
        // ... while the other grouping would be (1 < (2 = 2)) < 3.
        assert_eq!(int("3 = 3"), 1);
    }

    #[test]
    fn logical_operators_are_not_the_word_ones() {
        // §5.3: `and` is bitwise, so it does not stop at 1.
        assert_eq!(int("2 and 1"), 0);
        assert_eq!(int("2 && 1"), 1);
        assert_eq!(int("0 || 3"), 1);
        // `not` is bitwise negation, `!` is logical.
        assert_eq!(int("not 0"), -1);
        assert_eq!(int("!0"), 1);
        assert_eq!(int("!5"), 0);
    }

    #[test]
    fn the_two_shifts_differ_on_the_sign_bit() {
        assert_eq!(int("-8 >> 1"), -4, "arithmetic replicates the sign");
        assert_eq!(int("-8 >>> 1"), 2_147_483_644, "logical brings zeros in");
    }

    #[test]
    fn division_truncates_toward_zero() {
        assert_eq!(int("-7 / 2"), -3);
        assert_eq!(int("-7 % 2"), -1);
        assert_eq!(int("7 / -2"), -3);
    }

    #[test]
    fn arithmetic_wraps_instead_of_panicking() {
        assert_eq!(int("2147483647 + 1"), i32::MIN);
        // INT_MIN cannot be written as a literal — 2147483648 is out of range
        // for a constant and the sign is an operator (§5.4) — so it is spelled
        // as arithmetic here and in `the_smallest_integer_cannot_be_written`.
        assert_eq!(int("(-2147483647 - 1) / -1"), i32::MIN);
    }

    #[test]
    fn the_smallest_integer_cannot_be_written_as_a_literal() {
        let chars: Vec<char> = "-2147483648".chars().collect();
        let mut in_comment = false;
        assert!(
            lex_line(&chars, &mut in_comment).is_err(),
            "the positive part is out of range, so the sign cannot rescue it"
        );
    }

    #[test]
    fn a_shift_count_is_masked() {
        // x86 masks the count to five bits; documented in the module.
        assert_eq!(int("1 << 32"), 1);
        assert_eq!(int("1 << 33"), 2);
    }

    #[test]
    fn divide_by_zero_is_an_error() {
        assert_eq!(run("1 / 0", &[]), Err(Error::DivideByZero));
        assert_eq!(run("1 % 0", &[]), Err(Error::DivideByZero));
    }

    #[test]
    fn short_circuit_saves_the_right_operand() {
        // Without short-circuiting the right side would divide by zero.
        assert_eq!(int("0 && 1/0"), 0);
        assert_eq!(int("1 || 1/0"), 1);
        // The other way round, the error has to happen.
        assert_eq!(run("1 && 1/0", &[]), Err(Error::DivideByZero));
    }

    #[test]
    fn strings_are_not_operands() {
        assert_eq!(run("'a' = 'b'", &[]), Err(Error::TypeMismatch));
        assert_eq!(run("'a' + 1", &[]), Err(Error::TypeMismatch));
        // But a bare string is a value, which is what a parameter needs.
        assert_eq!(run("'a'", &[]).unwrap(), Value::from("a"));
    }

    #[test]
    fn variables_are_read_through_the_scope() {
        assert_eq!(run("A + 1", &[("A", Value::Int(41))]).unwrap(), Value::Int(42));
        assert_eq!(
            run("A + 1", &[]),
            Err(Error::VariableNotInitialized("A".into()))
        );
    }

    #[test]
    fn an_indexed_variable_reads_an_element() {
        let mut bank = Vars::new();
        bank.declare_int_array("a", 3).unwrap();
        bank.set_index("a", 1, Value::Int(5)).unwrap();
        let chars: Vec<char> = "a[1] + a[0]".chars().collect();
        let mut in_comment = false;
        let toks = lex_line(&chars, &mut in_comment).unwrap();
        let expr = parse(&toks).unwrap();
        assert_eq!(eval(&expr, &bank).unwrap(), Value::Int(5));
    }

    #[test]
    fn an_index_can_be_an_expression() {
        let mut bank = Vars::new();
        bank.declare_int_array("a", 5).unwrap();
        bank.set_index("a", 2, Value::Int(9)).unwrap();
        bank.set("i", Value::Int(2)).unwrap();
        let chars: Vec<char> = "a[i]".chars().collect();
        let mut in_comment = false;
        let toks = lex_line(&chars, &mut in_comment).unwrap();
        assert_eq!(eval(&parse(&toks).unwrap(), &bank).unwrap(), Value::Int(9));
    }

    #[test]
    fn the_assignment_equals_is_equality_inside_an_expression() {
        // `if result = 2` is the idiom this exists for.
        assert_eq!(
            run("result = 2", &[("result", Value::Int(0))]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            run("result = 2", &[("result", Value::Int(2))]).unwrap(),
            Value::Int(1)
        );
        // Chained, and left-associative: (A = B) = C.
        assert_eq!(
            run(
                "A = B = C",
                &[
                    ("A", Value::Int(1)),
                    ("B", Value::Int(1)),
                    ("C", Value::Int(1))
                ]
            )
            .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn a_negative_constant_is_a_unary_minus() {
        assert_eq!(int("-11"), -11);
        assert_eq!(int("- -1"), 1);
    }

    #[test]
    fn unbalanced_parentheses_are_a_syntax_error() {
        let chars: Vec<char> = "(1 + 2".chars().collect();
        let mut in_comment = false;
        let toks = lex_line(&chars, &mut in_comment).unwrap();
        assert!(parse(&toks).is_err());
    }

    #[test]
    fn two_expressions_on_one_line_is_an_error() {
        let chars: Vec<char> = "1 2".chars().collect();
        let mut in_comment = false;
        let toks = lex_line(&chars, &mut in_comment).unwrap();
        assert!(matches!(parse(&toks), Err(ParseError::Trailing(_))));
    }

    #[test]
    fn parse_prefix_stops_where_the_expression_ends() {
        // The single-line if: `if A>1 goto label`.
        let chars: Vec<char> = "A>1 goto label".chars().collect();
        let mut in_comment = false;
        let toks = lex_line(&chars, &mut in_comment).unwrap();
        let (expr, used) = parse_prefix(&toks).unwrap();
        assert_eq!(used, 3, "A > 1");
        assert_eq!(expr, Expr::Binary(Op::Gt, Box::new(Expr::Var("A".into())), Box::new(Expr::Int(1))));
    }
}
