use crate::{ParseError, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TokenKind {
    End,
    Name(String),
    String(String),
    Integer(String),
    Decimal(String),
    Null,
    True,
    False,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Comma,
    Dot,
    Question,
    Colon,
    Plus,
    Minus,
    Star,
    Slash,
    Bang,
    And,
    Or,
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

pub(crate) struct Lexer<'a> {
    source: &'a str,
    rule_name: &'a str,
    cursor: usize,
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(rule_name: &'a str, source: &'a str) -> Self {
        Self {
            source,
            rule_name,
            cursor: 0,
        }
    }

    pub(crate) fn next(&mut self) -> Result<Token, ParseError> {
        self.skip_whitespace();
        let start = self.cursor;
        let Some(character) = self.peek_char() else {
            return Ok(Token {
                kind: TokenKind::End,
                span: Span::new(start, start),
            });
        };

        if is_name_start(character) {
            return Ok(self.lex_name());
        }
        if character.is_ascii_digit() {
            return self.lex_number();
        }
        if matches!(character, '\'' | '"') {
            return self.lex_string(character);
        }

        let kind = match character {
            '(' => self.single(TokenKind::LeftParen),
            ')' => self.single(TokenKind::RightParen),
            '[' => self.single(TokenKind::LeftBracket),
            ']' => self.single(TokenKind::RightBracket),
            ',' => self.single(TokenKind::Comma),
            '.' => self.single(TokenKind::Dot),
            '?' => self.single(TokenKind::Question),
            ':' => self.single(TokenKind::Colon),
            '+' => self.single(TokenKind::Plus),
            '-' => self.single(TokenKind::Minus),
            '*' => self.single(TokenKind::Star),
            '/' => self.single(TokenKind::Slash),
            '!' => {
                self.cursor += 1;
                if self.consume_if('=') {
                    TokenKind::NotEqual
                } else {
                    TokenKind::Bang
                }
            }
            '=' => {
                self.cursor += 1;
                if self.consume_if('=') {
                    TokenKind::Equal
                } else {
                    return Err(self.error(start, "=="));
                }
            }
            '&' => {
                self.cursor += 1;
                if self.consume_if('&') {
                    TokenKind::And
                } else {
                    return Err(self.error(start, "&&"));
                }
            }
            '|' => {
                self.cursor += 1;
                if self.consume_if('|') {
                    TokenKind::Or
                } else {
                    return Err(self.error(start, "||"));
                }
            }
            '<' => {
                self.cursor += 1;
                if self.consume_if('=') {
                    TokenKind::LessThanOrEqual
                } else {
                    TokenKind::LessThan
                }
            }
            '>' => {
                self.cursor += 1;
                if self.consume_if('=') {
                    TokenKind::GreaterThanOrEqual
                } else {
                    TokenKind::GreaterThan
                }
            }
            _ => return Err(self.error(start, "a supported expression token")),
        };

        Ok(Token {
            kind,
            span: Span::new(start, self.cursor),
        })
    }

    fn lex_name(&mut self) -> Token {
        let start = self.cursor;
        self.consume_char();
        while self.peek_char().is_some_and(is_name_continue) {
            self.consume_char();
        }
        let name = &self.source[start..self.cursor];
        let kind = match name {
            "null" => TokenKind::Null,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            _ => TokenKind::Name(name.to_owned()),
        };
        Token {
            kind,
            span: Span::new(start, self.cursor),
        }
    }

    fn lex_number(&mut self) -> Result<Token, ParseError> {
        let start = self.cursor;
        while self
            .peek_char()
            .is_some_and(|character| character.is_ascii_digit())
        {
            self.consume_char();
        }

        let kind = if self.peek_char() == Some('.') {
            self.consume_char();
            if !self
                .peek_char()
                .is_some_and(|character| character.is_ascii_digit())
            {
                return Err(self.error(self.cursor, "a base-10 decimal literal"));
            }
            while self
                .peek_char()
                .is_some_and(|character| character.is_ascii_digit())
            {
                self.consume_char();
            }
            TokenKind::Decimal(self.source[start..self.cursor].to_owned())
        } else {
            TokenKind::Integer(self.source[start..self.cursor].to_owned())
        };

        Ok(Token {
            kind,
            span: Span::new(start, self.cursor),
        })
    }

    fn lex_string(&mut self, quote: char) -> Result<Token, ParseError> {
        let start = self.cursor;
        self.consume_char();
        let mut value = String::new();

        loop {
            let offset = self.cursor;
            let Some(character) = self.consume_char() else {
                return Err(self.error(self.source.len(), "a closing string quote"));
            };
            if character == quote {
                return Ok(Token {
                    kind: TokenKind::String(value),
                    span: Span::new(start, self.cursor),
                });
            }
            if character != '\\' {
                value.push(character);
                continue;
            }

            let Some(escaped) = self.consume_char() else {
                return Err(self.error(self.source.len(), "a supported string escape"));
            };
            let decoded = match escaped {
                '\\' => '\\',
                '\'' => '\'',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => return Err(self.error(offset, "a supported string escape")),
            };
            value.push(decoded);
        }
    }

    fn single(&mut self, kind: TokenKind) -> TokenKind {
        self.consume_char();
        kind
    }

    fn skip_whitespace(&mut self) {
        while self.peek_char().is_some_and(char::is_whitespace) {
            self.consume_char();
        }
    }

    fn consume_if(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.consume_char();
            true
        } else {
            false
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.cursor..].chars().next()
    }

    fn consume_char(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.cursor += character.len_utf8();
        Some(character)
    }

    fn error(&self, offset: usize, expectation: impl Into<String>) -> ParseError {
        ParseError::new(self.rule_name, offset, expectation)
    }
}

fn is_name_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_'
}

fn is_name_continue(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}
