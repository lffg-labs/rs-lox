use std::iter;

use anyhow::{bail, Result};

use crate::{
    human,
    scanner::input::{Input, SpannedChar},
    span::Span,
    token::{Token, TokenKind},
};

mod input;

pub struct Scanner<'s> {
    input: Input<'s>,
    lexme_lo_bound: Span,
}

// The actual scanner implementation.
impl<'s> Scanner<'s> {
    /// Returns a new iterator over the tokens of the source stream.
    pub fn scan_tokens(&'s mut self) -> impl Iterator<Item = Result<Token<'s>>> {
        let mut done = false;
        iter::from_fn(move || {
            if done {
                return None;
            }
            let token = self.scan_token().map(|token| {
                if token.kind == TokenKind::Eof {
                    done = true
                }
                token
            });
            // Ensure that every produced token will start a new lexme.
            self.lexme_lo_bound = self.input.current().0;
            Some(token)
        })
    }

    /// Produces the next token.
    fn scan_token(&mut self) -> Result<Token<'s>> {
        use TokenKind::*;
        let (span, char) = self.input.current();

        let kind = match char {
            '\0' => Eof,
            '(' => LeftParen,
            ')' => RightParen,
            '{' => LeftBrace,
            '}' => RightBrace,
            '.' => Dot,
            ',' => Comma,
            ';' => Semicolon,
            '+' => Plus,
            '-' => Minus,
            '*' => Star,
            '"' => self.string()?,
            '/' => self.slash_or_comment(),
            '<' => self.peek_select('=', LessEqual, Equal),
            '>' => self.peek_select('=', GreaterEqual, Greater),
            '!' => self.peek_select('=', BangEqual, Bang),
            '=' => self.peek_select('=', EqualEqual, Equal),
            c if c.is_whitespace() => Whitespace(c),
            c => {
                self.input.advance();
                bail!(
                    "Unexpected character `{}` at position {}.",
                    human::char(c),
                    span
                );
            }
        };
        Ok(self.token(kind))
    }

    /// Tries to scan a `Comment` token kind. Otherwise will return a `Slash` kind.
    fn slash_or_comment(&mut self) -> TokenKind {
        if self.peek_match('/') {
            while !self.peek_is('\n') && !self.input.finished() {
                self.input.advance();
            }
            let lit_span = self.lexme_lo_bound.to(self.input.current().0).updated(2, 0);
            let lit_val = self.input.spanned(lit_span).into();
            return TokenKind::Comment(lit_val);
        }
        TokenKind::Slash
    }

    /// Tries to scan a `String` token kind.
    fn string(&mut self) -> Result<TokenKind> {
        while !self.peek_is('"') && !self.input.finished() {
            self.input.advance();
        }
        self.peek_expect('"')?;
        let lit_span = self
            .lexme_lo_bound
            .to(self.input.current().0)
            .updated(1, -1);
        let lit_val = self.input.spanned(lit_span).into();
        Ok(TokenKind::String(lit_val))
    }
}

// The scanner helper methods.
impl<'s> Scanner<'s> {
    /// Creates a new scanner.
    pub fn new(source: &'s str) -> Scanner<'s> {
        Scanner {
            input: Input::new(source),
            lexme_lo_bound: Span::new(0, 0),
        }
    }

    /// Checks if the next character matches the given one.
    #[inline]
    fn peek_is(&mut self, expected: char) -> bool {
        self.input.peek().1 == expected
    }

    /// Checks if the next character matches the given one. Will advance in such case.
    fn peek_match(&mut self, expected: char) -> bool {
        if self.input.peek().1 != expected {
            return false;
        }
        self.input.advance();
        true
    }

    /// Checks if the next character matches the given one.
    /// Will advance in such case, otherwise returns an `Err`.
    fn peek_expect(&mut self, expected: char) -> Result<SpannedChar> {
        let (span, char) = self.input.peek();
        if char != expected {
            bail!(
                "Unexpected character `{}`, expected `{}` at position {}.",
                human::char(char),
                human::char(expected),
                span
            );
        }
        Ok(self.input.advance())
    }

    /// Returns `a` if the next character matches the given one. Otherwise returns `b`.
    #[inline]
    fn peek_select<T>(&mut self, expected: char, a: T, b: T) -> T {
        match self.peek_match(expected) {
            true => a,
            false => b,
        }
    }

    /// Creates a new token.
    #[inline]
    fn token(&mut self, kind: TokenKind) -> Token<'s> {
        let span = self.lexme_lo_bound.to(self.input.current().0);
        let token = Token {
            kind,
            lexme: self.input.spanned(span),
            span,
        };
        self.input.advance();
        token
    }
}
