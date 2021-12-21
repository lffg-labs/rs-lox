use std::{iter::Peekable, mem};

use crate::{
    ast::{
        expr::{self, Expr, ExprKind},
        stmt::{self, Stmt},
    },
    parser::{error::ParseError, options::ParserOptions, scanner::Scanner},
    token::{Token, TokenKind},
};

pub mod error;
pub mod options;
mod scanner;

type PResult<T> = Result<T, ParseError>;

pub struct Parser<'src> {
    scanner: Peekable<Scanner<'src>>,
    current_token: Token,
    prev_token: Token,
    diagnostics: Vec<ParseError>,
    pub options: ParserOptions,
}

// The parser implementation.
//
// # Grammar:
//
// ```none
// program    -> stmt* EOF
//
// stmt       -> expr_stmt
//             | print_stmt ;
//
// expr_stmt  -> expr ";" ;
// print_stmt -> "print" expr ";" ;
//
// expr       -> equality
// equality   -> comparison ( ( "==" | "!=" ) comparison )* ;
// comparison -> term ( ( ">" | ">=" | "<" | "<=" ) term )* ;
// term       -> factor ( ( "+" | "-" ) factor )* ;
// factor     -> unary ( ( "*" | "/" ) unary )* ;
// unary      -> ( "show" | "typeof" | "!" | "-" ) unary | primary ;
// primary    -> NUMBER | STRING | "true" | "false" | "nil" | "(" expr ")" ;
// ```
//
// Each production has a correspondent method in the following implementation.
impl Parser<'_> {
    pub fn parse(mut self) -> (Vec<Stmt>, Vec<ParseError>) {
        // The first advancement:
        if let Err(err) = self.advance() {
            self.synchronize_with(err);
        }

        let mut stmts = Vec::new();
        while !self.is_at_end() {
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(error) => self.synchronize_with(error),
            }
        }
        (stmts, self.diagnostics)
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        if self.take(TokenKind::Print)? {
            return self.parse_print_stmt();
        }

        self.parse_expr_stmt()
    }

    fn parse_print_stmt(&mut self) -> PResult<Stmt> {
        let expr = self.parse_expr()?;
        let semicolon_span = self
            .consume(TokenKind::Semicolon, "Expected `;` after value")?
            .span;
        Ok(Stmt {
            span: expr.span.to(semicolon_span),
            kind: stmt::Print { expr, debug: false }.into(),
        })
    }

    fn parse_expr_stmt(&mut self) -> PResult<Stmt> {
        let expr = self.parse_expr()?;

        // If the parser is running in the REPL mode and the next token is of kind `Eof`, it will
        // emit a `Print` statement in order to show the given expression's value.
        if self.options.repl_mode && self.is_at_end() {
            return Ok(Stmt {
                span: expr.span,
                kind: stmt::Print { expr, debug: true }.into(),
            });
        }

        let semicolon_span = self
            .consume(TokenKind::Semicolon, "Expected `;` after expression")?
            .span;
        Ok(Stmt {
            span: expr.span.to(semicolon_span),
            kind: stmt::Expr { expr }.into(),
        })
    }

    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_equality()
    }

    fn parse_equality(&mut self) -> PResult<Expr> {
        bin_expr!(
            self,
            kinds = EqualEqual | BangEqual,
            next_production = parse_comparison
        )
    }

    fn parse_comparison(&mut self) -> PResult<Expr> {
        bin_expr!(
            self,
            kinds = Greater | GreaterEqual | Less | LessEqual,
            next_production = parse_term
        )
    }

    fn parse_term(&mut self) -> PResult<Expr> {
        bin_expr!(
            self, //↵
            kinds = Plus | Minus,
            next_production = parse_factor
        )
    }

    fn parse_factor(&mut self) -> PResult<Expr> {
        bin_expr!(
            self, //↵
            kinds = Star | Slash,
            next_production = parse_unary
        )
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        use TokenKind::{Bang, Minus, Show, Typeof};
        if let Bang | Minus | Typeof | Show = self.current_token.kind {
            let operator = self.advance()?.clone();
            let operand = self.parse_unary()?;
            return Ok(Expr {
                span: operator.span.to(operand.span),
                kind: ExprKind::from(expr::Unary {
                    operator,
                    operand: operand.into(),
                }),
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        use TokenKind::*;
        match self.current_token.kind {
            String(_) | Number(_) | True | False | Nil => {
                let token = self.advance()?;
                Ok(Expr {
                    kind: expr::Lit::from(token.clone()).into(),
                    span: token.span,
                })
            }
            LeftParen => {
                let left_paren_span = self.advance()?.span;
                let expr = self.parse_expr()?.into();
                let right_paren_span = self
                    .consume(RightParen, "Expected group to be closed")?
                    .span;
                Ok(Expr {
                    kind: expr::Group { expr }.into(),
                    span: left_paren_span.to(right_paren_span),
                })
            }
            _ => Err(ParseError::UnexpectedToken {
                message: "Expected any expression".into(),
                offending: self.current_token.clone(),
                expected: None,
            }),
        }
    }
}

// The parser helper methods.
impl<'src> Parser<'src> {
    /// Creates a new parser.
    pub fn new(src: &'src str) -> Self {
        Self {
            scanner: Scanner::new(src).peekable(),
            current_token: Token::dummy(),
            prev_token: Token::dummy(),
            diagnostics: Vec::new(),
            options: Default::default(),
        }
    }

    /// Checks if the given token kind shall be ignored by this parser.
    #[inline]
    fn is_ignored_kind(kind: &TokenKind) -> bool {
        use TokenKind::*;
        matches!(kind, Comment(_) | NewLine | Whitespace)
    }

    /// Advances the parser and returns a reference to the `prev_token` field.
    fn advance(&mut self) -> PResult<&Token> {
        self.advance_unchecked();
        // Handle errors from scanner:
        if let TokenKind::Error(message) = &self.current_token.kind {
            return Err(ParseError::ScannerError {
                message: message.clone(),
                offending_span: self.current_token.span,
            });
        }
        Ok(&self.prev_token)
    }

    /// Advances the parser without checking for an `Error` token.
    fn advance_unchecked(&mut self) {
        let next = loop {
            let maybe_next = self.scanner.next().expect("Cannot advance past Eof.");
            if !Self::is_ignored_kind(&maybe_next.kind) {
                break maybe_next;
            }
        };
        self.prev_token = mem::replace(&mut self.current_token, next);
    }

    /// Checks if the current token matches the expected one. If so, advances and returns true.
    /// Otherwise returns false. Such cases are `Ok(bool)`.
    ///
    /// Returns `Err` in case of advancement error.
    fn take(&mut self, expected: TokenKind) -> PResult<bool> {
        if self.current_token.kind == expected {
            self.advance()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Checks if the current token matches the expected one. If so, advances and returns the
    /// consumed token. Otherwise returns an expectation error with the given message.
    /// Also returns `Err` in case of advancement error.
    fn consume(&mut self, expected: TokenKind, msg: impl Into<String>) -> PResult<&Token> {
        if self.current_token.kind == expected {
            self.advance()
        } else {
            Err(ParseError::UnexpectedToken {
                message: msg.into(),
                offending: self.current_token.clone(),
                expected: Some(expected),
            })
        }
    }

    /// Synchronizes the parser state with the current token.
    ///
    /// A synchronization is needed in order to match the parser state to the current token.
    ///
    /// When an error is encountered in a `parse_` method, a `ParseError` is returned. These kind of
    /// errors are forwarded using the `?` operator, which, in practice, unwinds the parser stack
    /// frame. The question mark operator shall not be used in synchronization points.
    /// Such synchronization points will call this method, `synchronize_with`.
    ///
    /// The synchronization process discards all tokens until it reaches a grammar rule which marks
    /// a synchronization point.
    ///
    /// In this implementation, synchronization is done in statement boundaries:
    ///   * If the previous token is a semicolon, the parser is *probably* (exceptions exists, such
    ///     as a semicolon in a for loop) starting a new statement.
    ///   * If the next token is a listed (in the implementation) keyword the parser is also
    ///     starting a new statement.
    fn synchronize_with(&mut self, error: ParseError) {
        self.diagnostics.push(error);

        // If the end was already reached there is no need to advance the parser.
        if self.is_at_end() {
            return;
        }

        self.advance_unchecked();
        use TokenKind::*;
        while !{
            self.is_at_end()
                || matches!(self.prev_token.kind, Semicolon)
                || matches!(
                    self.current_token.kind,
                    Class | For | Fun | If | Print | Return | Var | While
                )
        } {
            self.advance_unchecked();
        }
    }

    /// Checks if the parser has finished.
    #[inline]
    fn is_at_end(&self) -> bool {
        self.current_token.kind == TokenKind::Eof
    }
}

/// Parses a binary expression.
macro_rules! bin_expr {
    ($self:expr, kinds = $( $kind:ident )|+, next_production = $next:ident) => {{
        let mut expr = $self.$next()?;
        while let $( TokenKind::$kind )|+ = $self.current_token.kind {
            let operator = $self.advance()?.clone();
            let right = $self.$next()?;
            expr = Expr {
                span: expr.span.to(right.span),
                kind: ExprKind::from(expr::Binary {
                    left: expr.into(),
                    operator,
                    right: right.into(),
                }),
            };
        }
        Ok(expr)
    }};
}
use bin_expr;
