use std::{iter::Peekable, mem};

use crate::{
    ast::{
        expr::{self, Expr, ExprKind},
        stmt::{self, Stmt, StmtKind},
    },
    parser::{error::ParseError, options::ParserOptions, scanner::Scanner},
    span::Span,
    token::{Token, TokenKind},
    value::LoxValue,
};

pub mod error;
pub mod options;
pub mod scanner;

type PResult<T> = Result<T, ParseError>;

pub type ParserOutcome = (Vec<Stmt>, Vec<ParseError>, bool);

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
// -----------------------------------------------------------------------------
//
// program     ::= decl* EOF ;
//
// decl        ::= var_decl
//               | stmt ;
//
// var_decl    ::= "var" IDENTIFIER ( "=" expr )? ";" ;
//
// stmt        ::= if_stmt
//               | for_stmt
//               | while_stmt
//               | print_stmt
//               | block_stmt
//               | expr_stmt ;
//
// if_stmt     ::= "if" "(" expr ")" statement ( "else" statement )? ;
// for_stmt    ::= "for" for_clauses statement ;
// for_clauses ::= "(" ( var_decl | expr_stmt | ";" ) expr? ";" expr? ")"
// while_stmt  ::= "while" "(" expr ")" statement ;
// print_stmt  ::= "print" expr ";" ;
// block_stmt  ::= "{" declaration* "}" ;
// expr_stmt   ::= expr ";" ;
//
// expr        ::= assignment ;
// assignment  ::= IDENTIFIER "=" expr
//               | logic_or ;
// logic_or    ::= logic_and ( "or" logic_and )* ;
// logic_and   ::= equality ( "and" equality )* ;
// equality    ::= comparison ( ( "==" | "!=" ) comparison )* ;
// comparison  ::= term ( ( ">" | ">=" | "<" | "<=" ) term )* ;
// term        ::= factor ( ( "+" | "-" ) factor )* ;
// factor      ::= unary ( ( "*" | "/" ) unary )* ;
// unary       ::= ( "show" | "typeof" | "!" | "-" ) unary
//               | primary ;
// primary     ::= IDENTIFIER
//               | NUMBER | STRING
//               | "true" | "false"
//               | "nil"
//               | "(" expr ")" ;
//
// -----------------------------------------------------------------------------
//
// Each production has a correspondent method in the following implementation.
impl Parser<'_> {
    pub fn parse(mut self) -> ParserOutcome {
        let stmts = self.parse_program();

        let allow_continuation = self.options.repl_mode
            && self.current_token.kind == TokenKind::Eof
            && self.diagnostics.len() == 1
            && self
                .diagnostics
                .last()
                .map(|error| error.allows_continuation())
                .unwrap_or(false);

        (stmts, self.diagnostics, allow_continuation)
    }

    fn parse_program(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        while !self.is_at_end() {
            // TODO: Maybe synchronize at `parse_decl`.
            stmts.push(self.parse_decl().unwrap_or_else(|error| {
                self.diagnostics.push(error);
                self.synchronize();
                let hi = self.current_token.span.hi;
                Stmt {
                    kind: stmt::Dummy().into(),
                    span: Span::new(hi, hi),
                }
            }));
        }
        stmts
    }

    //
    // Declarations
    //

    fn parse_decl(&mut self) -> PResult<Stmt> {
        match self.current_token.kind {
            TokenKind::Var => {
                self.advance();
                self.parse_var_decl()
            }
            _ => self.parse_stmt(),
        }
    }

    fn parse_var_decl(&mut self) -> PResult<Stmt> {
        use TokenKind::*;
        let var_span = self.consume(Var, S_MUST)?.span;

        if let Identifier(name) = &self.current_token.kind {
            let name = name.clone();
            let name_span = self.advance().span;

            let mut init = None;
            if self.take(Equal) {
                init = Some(self.parse_expr()?);
            }

            let semicolon_span = self
                .consume(Semicolon, "Expected `;` after variable declaration")?
                .span;

            return Ok(Stmt {
                kind: StmtKind::from(stmt::Var {
                    name,
                    name_span,
                    init,
                }),
                span: var_span.to(semicolon_span),
            });
        }

        Err(self.unexpected("Expected variable name", Some(Identifier("<ident>".into()))))
    }

    //
    // Statements
    //

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        use TokenKind::*;
        match self.current_token.kind {
            If => self.parse_if_stmt(),
            For => self.parse_for_stmt(),
            While => self.parse_while_stmt(),
            Print => self.parse_print_stmt(),
            LeftBrace => {
                let (stmts, span) = self.parse_block()?;
                let kind = stmt::Block { stmts }.into();
                Ok(Stmt { kind, span })
            }
            _ => self.parse_expr_stmt(),
        }
    }

    fn parse_if_stmt(&mut self) -> PResult<Stmt> {
        use TokenKind::*;
        let if_token_span = self.consume(If, S_MUST)?.span;

        let cond = self.paired(
            LeftParen,
            "Expected `if` condition group opening",
            "Expected `if` condition group to be closed",
            |this| this.parse_expr(),
        )?;
        let then_branch = self.parse_stmt()?;
        let else_branch = if self.take(Else) {
            Some(self.parse_stmt()?)
        } else {
            None
        };

        Ok(Stmt {
            span: if_token_span.to(else_branch
                .as_ref()
                .map(|it| it.span)
                .unwrap_or(then_branch.span)),
            kind: StmtKind::from(stmt::If {
                cond,
                then_branch: then_branch.into(),
                else_branch: else_branch.map(|it| it.into()),
            }),
        })
    }

    // In this implementation, all `for` statements are translated to `while` statements by the
    // parser. Hence there is not even a `StmtKind::For` kind since it is a syntactic sugar. E.g.:
    //
    // ```
    // for (var i = 1; i <= 10; i = i + 1) { print show i; }
    // ```
    //
    // Is translated to:
    //
    // ```
    // {
    //    var i = 1;
    //    while (i <= 10) {
    //      { print show i; }
    //      i = i + 1;
    //    }
    // }
    // ```
    fn parse_for_stmt(&mut self) -> PResult<Stmt> {
        use TokenKind::*;
        let for_token_span = self.consume(For, S_MUST)?.span;

        let (init, cond, incr) = self.paired(
            LeftParen,
            "Expected `for` clauses group opening",
            "Expected `for` clauses group to be closed",
            |this| {
                let init = match this.current_token.kind {
                    Semicolon => {
                        this.advance();
                        None
                    }
                    Var => Some(this.parse_var_decl()?),
                    _ => Some(this.parse_expr_stmt()?),
                };
                let cond = match this.current_token.kind {
                    // If there is none condition in the for clauses, the parser creates a synthetic `true`
                    // literal expression. This must be placed here to capture the current span (†).
                    Semicolon => Expr {
                        kind: ExprKind::from(expr::Lit {
                            value: LoxValue::Boolean(true),
                        }),
                        span: {
                            let lo = this.current_token.span.lo; // <--- This span. (†)
                            Span::new(lo, lo)
                        },
                    },
                    _ => this.parse_expr()?,
                };
                this.consume(Semicolon, "Expected `;` after `for` condition")?;
                let incr = match this.current_token.kind {
                    RightParen => None,
                    _ => Some(this.parse_expr()?),
                };
                Ok((init, cond, incr))
            },
        )?;
        let mut body = self.parse_stmt()?;

        // Desugar `for` increment:
        if let Some(incr) = incr {
            body = Stmt {
                span: body.span,
                kind: StmtKind::from(stmt::Block {
                    stmts: Vec::from([
                        body,
                        Stmt {
                            span: incr.span,
                            kind: StmtKind::from(stmt::Expr { expr: incr }),
                        },
                    ]),
                }),
            };
        }

        // Create the while:
        body = Stmt {
            span: for_token_span.to(body.span),
            kind: StmtKind::from(stmt::While {
                cond,
                body: body.into(),
            }),
        };

        // Desugar `for` initializer:
        if let Some(init) = init {
            body = Stmt {
                span: body.span,
                kind: StmtKind::from(stmt::Block {
                    stmts: Vec::from([init, body]),
                }),
            };
        }

        Ok(body)
    }

    fn parse_while_stmt(&mut self) -> PResult<Stmt> {
        use TokenKind::*;
        let while_token_span = self.consume(While, S_MUST)?.span;

        let cond = self.paired(
            LeftParen,
            "Expected `while` condition group opening",
            "Expected `while` condition group to be closed",
            |this| this.parse_expr(),
        )?;
        let body = self.parse_stmt()?;

        Ok(Stmt {
            span: while_token_span.to(body.span),
            kind: StmtKind::from(stmt::While {
                cond,
                body: body.into(),
            }),
        })
    }

    fn parse_print_stmt(&mut self) -> PResult<Stmt> {
        let print_token_span = self.consume(TokenKind::Print, S_MUST)?.span;
        let expr = self.parse_expr()?;
        let semicolon_span = self
            .consume(TokenKind::Semicolon, "Expected `;` after value")?
            .span;
        Ok(Stmt {
            span: print_token_span.to(semicolon_span),
            kind: stmt::Print { expr, debug: false }.into(),
        })
    }

    fn parse_block(&mut self) -> PResult<(Vec<Stmt>, Span)> {
        self.paired_spanned(
            TokenKind::LeftBrace,
            "Expected block to be opened",
            "Expected block to be closed",
            |this| {
                let mut stmts = Vec::new();
                while !this.is(&TokenKind::RightBrace) && !this.is_at_end() {
                    stmts.push(this.parse_decl()?);
                }
                Ok(stmts)
            },
        )
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

    //
    // Expressions
    //

    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> PResult<Expr> {
        // The parser does not yet know if `left` should be used as an expression (i.e. an rvalue)
        // or as an "assignment target" (i.e. an lvalue).
        let left = self.parse_or()?;

        if self.take(TokenKind::Equal) {
            // Since assignments are right associative, we use right recursion to parse its value.
            // The right-most assignment value should be evaluated first (down in the parse tree),
            // so it should be parsed last. This semantic can be coded with this kind of recursion.
            let value = self.parse_assignment()?;

            // Now the parser knows that `left` must be an lvalue.
            if let ExprKind::Var(expr::Var { name }) = left.kind {
                return Ok(Expr {
                    span: left.span.to(value.span),
                    kind: ExprKind::from(expr::Assignment {
                        name,
                        name_span: left.span,
                        value: value.into(),
                    }),
                });
            }

            return Err(ParseError::Error {
                message: "Invalid assignment target".into(),
                span: left.span,
            });
        }

        Ok(left)
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        bin_expr!(
            self,
            parse_as = Logical,
            token_kinds = Or,
            next_production = parse_and
        )
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        bin_expr!(
            self,
            parse_as = Logical,
            token_kinds = And,
            next_production = parse_equality
        )
    }

    fn parse_equality(&mut self) -> PResult<Expr> {
        bin_expr!(
            self,
            parse_as = Binary,
            token_kinds = EqualEqual | BangEqual,
            next_production = parse_comparison
        )
    }

    fn parse_comparison(&mut self) -> PResult<Expr> {
        bin_expr!(
            self,
            parse_as = Binary,
            token_kinds = Greater | GreaterEqual | Less | LessEqual,
            next_production = parse_term
        )
    }

    fn parse_term(&mut self) -> PResult<Expr> {
        bin_expr!(
            self,
            parse_as = Binary,
            token_kinds = Plus | Minus,
            next_production = parse_factor
        )
    }

    fn parse_factor(&mut self) -> PResult<Expr> {
        bin_expr!(
            self,
            parse_as = Binary,
            token_kinds = Star | Slash,
            next_production = parse_unary
        )
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        use TokenKind::*;
        if let Bang | Minus | Typeof | Show = self.current_token.kind {
            let operator = self.advance().clone();
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
        match &self.current_token.kind {
            String(_) | Number(_) | True | False | Nil => {
                let token = self.advance();
                Ok(Expr {
                    kind: expr::Lit::from(token.clone()).into(),
                    span: token.span,
                })
            }
            Identifier(name) => Ok(Expr {
                kind: expr::Var { name: name.clone() }.into(),
                span: self.advance().span,
            }),
            LeftParen => {
                let (expr, span) = self.paired_spanned(
                    LeftParen,
                    S_MUST,
                    "Expected group to be closed",
                    |this| this.parse_expr(),
                )?;
                Ok(Expr {
                    kind: expr::Group { expr: expr.into() }.into(),
                    span,
                })
            }
            _ => Err(self.unexpected("Expected any expression", None)),
        }
    }
}

// The parser helper methods.
impl<'src> Parser<'src> {
    /// Creates a new parser.
    pub fn new(src: &'src str) -> Self {
        let mut parser = Self {
            scanner: Scanner::new(src).peekable(),
            current_token: Token::dummy(),
            prev_token: Token::dummy(),
            diagnostics: Vec::new(),
            options: Default::default(),
        };
        parser.advance(); // The first advancement.
        parser
    }

    /// Advances the parser and returns a reference to the `prev_token` field.
    fn advance(&mut self) -> &Token {
        let next = loop {
            let maybe_next = self.scanner.next().expect("Cannot advance past Eof.");
            // Report and ignore tokens with the `Error` kind:
            if let TokenKind::Error(message) = maybe_next.kind {
                self.diagnostics.push(ParseError::ScannerError {
                    span: maybe_next.span,
                    message,
                });
                continue;
            }
            // Handle other common ignored kinds:
            if let TokenKind::Comment(_) | TokenKind::Whitespace(_) = maybe_next.kind {
                continue;
            }
            break maybe_next;
        };
        self.prev_token = mem::replace(&mut self.current_token, next);
        &self.prev_token
    }

    /// Checks if the current token matches the kind of the given one.
    #[inline]
    fn is(&mut self, expected: &TokenKind) -> bool {
        mem::discriminant(&self.current_token.kind) == mem::discriminant(expected)
    }

    /// Checks if the current token matches the kind of the given one. In such case advances and
    /// returns true. Otherwise returns false.
    fn take(&mut self, expected: TokenKind) -> bool {
        let res = self.is(&expected);
        if res {
            self.advance();
        }
        res
    }

    /// Checks if the current token matches the kind of the given one. In such case advances and
    /// returns `Ok(_)` with the consumed token. Otherwise returns an expectation error with the
    /// provided message.
    fn consume(&mut self, expected: TokenKind, msg: impl Into<String>) -> PResult<&Token> {
        if self.is(&expected) {
            Ok(self.advance())
        } else {
            Err(self.unexpected(msg, Some(expected)))
        }
    }

    /// Pair invariant.
    fn paired<I, R>(
        &mut self,
        delim_start: TokenKind,
        delim_start_expectation: impl Into<String>,
        delim_end_expectation: impl Into<String>,
        inner: I,
    ) -> PResult<R>
    where
        I: FnOnce(&mut Self) -> PResult<R>,
    {
        self.paired_spanned(
            delim_start,
            delim_start_expectation,
            delim_end_expectation,
            inner,
        )
        .map(|(ret, _)| ret)
    }

    /// Pair invariant (2), also returning the full span.
    fn paired_spanned<I, R>(
        &mut self,
        delim_start: TokenKind,
        delim_start_expectation: impl Into<String>,
        delim_end_expectation: impl Into<String>,
        inner: I,
    ) -> PResult<(R, Span)>
    where
        I: FnOnce(&mut Self) -> PResult<R>,
    {
        let start_span = self
            .consume(delim_start.clone(), delim_start_expectation)?
            .span;
        let ret = inner(self)?;
        let end_span = match self.consume(delim_start.get_pair(), delim_end_expectation) {
            Ok(token) => token.span,
            Err(error) => {
                return Err(error);
            }
        };
        Ok((ret, start_span.to(end_span)))
    }

    /// Returns an `ParseError::UnexpectedToken`.
    #[inline(always)]
    fn unexpected(&self, message: impl Into<String>, expected: Option<TokenKind>) -> ParseError {
        let mut message = message.into();
        if message == S_MUST {
            message = "Parser bug. Unexpected token".into()
        }
        ParseError::UnexpectedToken {
            message,
            expected,
            offending: self.current_token.clone(),
        }
    }

    /// Synchronizes the parser state with the current token.
    /// A synchronization is needed in order to match the parser state to the current token.
    ///
    /// When an error is encountered in a `parse_*` method, a `ParseError` is returned. These kind
    /// of errors are forwarded using the `?` operator, which, in practice, unwinds the parser
    /// stack frame. Hence the question mark operator should not be used in synchronization points.
    /// Such synchronization points call this method.
    ///
    /// The synchronization process discards all tokens until it reaches a grammar rule which marks
    /// a synchronization point.
    ///
    /// In this implementation, synchronizations are manually performed in statement boundaries:
    ///   * If the previous token is a semicolon, the parser is *probably* (exceptions exists, such
    ///     as a semicolon in a for loop) starting a new statement.
    ///   * If the next token marks the start of a new statement.
    ///
    /// Before synchronize one must not forget to emit the raised parse error.
    fn synchronize(&mut self) {
        // If the end is already reached any further advancements are needless.
        if self.is_at_end() {
            return;
        }

        self.advance();
        use TokenKind::*;
        while !self.is_at_end() {
            let curr = &self.current_token.kind;
            let prev = &self.prev_token.kind;

            if matches!(prev, Semicolon)
                || matches!(curr, Class | For | Fun | If | Print | Return | Var | While)
            {
                break;
            }

            self.advance();
        }
    }

    /// Checks if the parser has finished.
    #[inline]
    fn is_at_end(&self) -> bool {
        self.current_token.kind == TokenKind::Eof
    }
}

/// (String Must) Indicates the parser to emit a parser error (i.e. the parser is bugged) message.
const S_MUST: &str = "@@must";

/// Parses a binary expression.
macro_rules! bin_expr {
    ($self:expr, parse_as = $ast_kind:ident, token_kinds = $( $kind:ident )|+, next_production = $next:ident) => {{
        let mut expr = $self.$next()?;
        while let $( TokenKind::$kind )|+ = $self.current_token.kind {
            let operator = $self.advance().clone();
            let right = $self.$next()?;
            expr = Expr {
                span: expr.span.to(right.span),
                kind: ExprKind::from(expr::$ast_kind {
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
