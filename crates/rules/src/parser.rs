use crate::lexer::{Lexer, Token, TokenKind};
use crate::{
    BinaryOp, Expr, ExprKind, Function, Literal, MAX_AST_DEPTH, MAX_EXPRESSION_BYTES,
    MAX_LIST_ITEMS, ParseError, Span, UnaryOp,
};

/// Parse a restricted CEL-profile expression for one named deploy-time rule.
///
/// The parser rejects unknown functions before type checking. Its result is an
/// AST with byte spans; later stages own all binding and type validation.
pub fn parse_expression(rule_name: &str, source: &str) -> Result<Expr, ParseError> {
    if source.len() > MAX_EXPRESSION_BYTES {
        return Err(ParseError::new(
            rule_name,
            MAX_EXPRESSION_BYTES,
            format!("an expression of at most {MAX_EXPRESSION_BYTES} bytes"),
        ));
    }

    let mut parser = Parser::new(rule_name, source)?;
    let expression = parser.parse_conditional()?;
    if !matches!(parser.current.kind, TokenKind::End) {
        return Err(parser.error_at_current("the end of the expression"));
    }
    if expression_depth(&expression) > MAX_AST_DEPTH {
        return Err(ParseError::new(
            rule_name,
            expression.span.start,
            format!("an expression nesting depth of at most {MAX_AST_DEPTH}"),
        ));
    }
    Ok(expression)
}

struct Parser<'a> {
    rule_name: &'a str,
    lexer: Lexer<'a>,
    current: Token,
    syntactic_nesting: usize,
}

impl<'a> Parser<'a> {
    fn new(rule_name: &'a str, source: &'a str) -> Result<Self, ParseError> {
        let mut lexer = Lexer::new(rule_name, source);
        let current = lexer.next()?;
        Ok(Self {
            rule_name,
            lexer,
            current,
            syntactic_nesting: 0,
        })
    }

    fn parse_conditional(&mut self) -> Result<Expr, ParseError> {
        let condition = self.parse_or()?;
        if !matches!(self.current.kind, TokenKind::Question) {
            return Ok(condition);
        }

        let start = condition.span.start;
        let question_offset = self.current.span.start;
        self.advance()?;
        let when_true = self.parse_nested_expression(question_offset)?;
        self.expect_colon()?;
        let when_false = self.parse_nested_expression(question_offset)?;
        Ok(Expr::new(
            Span::new(start, when_false.span.end),
            ExprKind::Conditional {
                condition: Box::new(condition),
                when_true: Box::new(when_true),
                when_false: Box::new(when_false),
            },
        ))
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        self.parse_left_associative(Self::parse_and, |kind| match kind {
            TokenKind::Or => Some(BinaryOp::Or),
            _ => None,
        })
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        self.parse_left_associative(Self::parse_equality, |kind| match kind {
            TokenKind::And => Some(BinaryOp::And),
            _ => None,
        })
    }

    fn parse_equality(&mut self) -> Result<Expr, ParseError> {
        self.parse_left_associative(Self::parse_comparison, |kind| match kind {
            TokenKind::Equal => Some(BinaryOp::Equal),
            TokenKind::NotEqual => Some(BinaryOp::NotEqual),
            _ => None,
        })
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        self.parse_left_associative(Self::parse_additive, |kind| match kind {
            TokenKind::LessThan => Some(BinaryOp::LessThan),
            TokenKind::LessThanOrEqual => Some(BinaryOp::LessThanOrEqual),
            TokenKind::GreaterThan => Some(BinaryOp::GreaterThan),
            TokenKind::GreaterThanOrEqual => Some(BinaryOp::GreaterThanOrEqual),
            _ => None,
        })
    }

    fn parse_additive(&mut self) -> Result<Expr, ParseError> {
        self.parse_left_associative(Self::parse_multiplicative, |kind| match kind {
            TokenKind::Plus => Some(BinaryOp::Add),
            TokenKind::Minus => Some(BinaryOp::Subtract),
            _ => None,
        })
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, ParseError> {
        self.parse_left_associative(Self::parse_unary, |kind| match kind {
            TokenKind::Star => Some(BinaryOp::Multiply),
            TokenKind::Slash => Some(BinaryOp::Divide),
            _ => None,
        })
    }

    fn parse_left_associative(
        &mut self,
        operand: fn(&mut Parser<'a>) -> Result<Expr, ParseError>,
        operator: impl Fn(&TokenKind) -> Option<BinaryOp>,
    ) -> Result<Expr, ParseError> {
        let mut left = operand(self)?;
        while let Some(op) = operator(&self.current.kind) {
            self.advance()?;
            let right = operand(self)?;
            left = Expr::new(
                Span::new(left.span.start, right.span.end),
                ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            );
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        let op = match self.current.kind {
            TokenKind::Bang => UnaryOp::Not,
            TokenKind::Minus => UnaryOp::Negate,
            _ => return self.parse_postfix(),
        };
        let start = self.current.span.start;
        self.enter_nesting(start)?;
        self.advance()?;
        let operand = self.parse_unary();
        self.exit_nesting();
        let operand = operand?;
        Ok(Expr::new(
            Span::new(start, operand.span.end),
            ExprKind::Unary {
                op,
                operand: Box::new(operand),
            },
        ))
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expression = self.parse_primary()?;
        loop {
            if matches!(self.current.kind, TokenKind::Dot) {
                self.advance()?;
                let Token {
                    kind: TokenKind::Name(field),
                    span,
                } = self.current.clone()
                else {
                    return Err(self.error_at_current("a static object field name"));
                };
                self.advance()?;
                expression = Expr::new(
                    Span::new(expression.span.start, span.end),
                    ExprKind::Member {
                        target: Box::new(expression),
                        field,
                    },
                );
                continue;
            }
            if matches!(self.current.kind, TokenKind::LeftBracket) {
                self.advance()?;
                let Token {
                    kind: TokenKind::Integer(value),
                    span,
                } = self.current.clone()
                else {
                    return Err(self.error_at_current("a non-negative integer list index"));
                };
                let index = value.parse::<usize>().map_err(|_| {
                    ParseError::new(
                        self.rule_name,
                        span.start,
                        "a valid non-negative list index",
                    )
                })?;
                self.advance()?;
                let end = self.expect_right_bracket()?.end;
                expression = Expr::new(
                    Span::new(expression.span.start, end),
                    ExprKind::Index {
                        target: Box::new(expression),
                        index,
                    },
                );
                continue;
            }
            return Ok(expression);
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let token = self.current.clone();
        match token.kind {
            TokenKind::Null => {
                self.advance()?;
                Ok(Expr::new(token.span, ExprKind::Literal(Literal::Null)))
            }
            TokenKind::True => {
                self.advance()?;
                Ok(Expr::new(
                    token.span,
                    ExprKind::Literal(Literal::Bool(true)),
                ))
            }
            TokenKind::False => {
                self.advance()?;
                Ok(Expr::new(
                    token.span,
                    ExprKind::Literal(Literal::Bool(false)),
                ))
            }
            TokenKind::Integer(value) => {
                self.advance()?;
                Ok(Expr::new(
                    token.span,
                    ExprKind::Literal(Literal::Int(value)),
                ))
            }
            TokenKind::Decimal(value) => {
                self.advance()?;
                Ok(Expr::new(
                    token.span,
                    ExprKind::Literal(Literal::Decimal(value)),
                ))
            }
            TokenKind::String(value) => {
                self.advance()?;
                Ok(Expr::new(
                    token.span,
                    ExprKind::Literal(Literal::String(value)),
                ))
            }
            TokenKind::Name(name) => {
                self.advance()?;
                if !matches!(self.current.kind, TokenKind::LeftParen) {
                    return Ok(Expr::new(token.span, ExprKind::Name(name)));
                }
                let Some(function) = Function::parse(&name) else {
                    return Err(ParseError::new(
                        self.rule_name,
                        token.span.start,
                        "one of size, is_null, startsWith, endsWith",
                    ));
                };
                self.enter_nesting(token.span.start)?;
                self.advance()?;
                let arguments = self.parse_call_arguments();
                self.exit_nesting();
                let arguments = arguments?;
                let end = self.expect_right_paren()?.end;
                Ok(Expr::new(
                    Span::new(token.span.start, end),
                    ExprKind::Call {
                        function,
                        arguments,
                    },
                ))
            }
            TokenKind::LeftParen => {
                self.enter_nesting(token.span.start)?;
                self.advance()?;
                let expression = self.parse_conditional();
                self.exit_nesting();
                let mut expression = expression?;
                let end = self.expect_right_paren()?.end;
                expression.span = Span::new(token.span.start, end);
                Ok(expression)
            }
            TokenKind::LeftBracket => self.parse_list(),
            _ => Err(self.error_at_current("an expression")),
        }
    }

    fn parse_call_arguments(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut arguments = Vec::new();
        if matches!(self.current.kind, TokenKind::RightParen) {
            return Ok(arguments);
        }
        loop {
            arguments.push(self.parse_conditional()?);
            if !matches!(self.current.kind, TokenKind::Comma) {
                return Ok(arguments);
            }
            self.advance()?;
            if matches!(self.current.kind, TokenKind::RightParen) {
                return Err(self.error_at_current("an expression after the comma"));
            }
        }
    }

    fn parse_list(&mut self) -> Result<Expr, ParseError> {
        let start = self.current.span.start;
        self.enter_nesting(start)?;
        self.advance()?;
        let result = self.parse_list_contents(start);
        self.exit_nesting();
        result
    }

    fn parse_list_contents(&mut self, start: usize) -> Result<Expr, ParseError> {
        let mut items = Vec::new();
        if matches!(self.current.kind, TokenKind::RightBracket) {
            let end = self.current.span.end;
            self.advance()?;
            return Ok(Expr::new(Span::new(start, end), ExprKind::List(items)));
        }

        loop {
            let item = self.parse_conditional()?;
            if items.len() == MAX_LIST_ITEMS {
                return Err(ParseError::new(
                    self.rule_name,
                    item.span.start,
                    format!("a list literal with at most {MAX_LIST_ITEMS} items"),
                ));
            }
            items.push(item);
            if !matches!(self.current.kind, TokenKind::Comma) {
                break;
            }
            self.advance()?;
            if matches!(self.current.kind, TokenKind::RightBracket) {
                return Err(self.error_at_current("an expression after the comma"));
            }
        }
        let end = self.expect_right_bracket()?.end;
        Ok(Expr::new(Span::new(start, end), ExprKind::List(items)))
    }

    fn parse_nested_expression(&mut self, offset: usize) -> Result<Expr, ParseError> {
        self.enter_nesting(offset)?;
        let expression = self.parse_conditional();
        self.exit_nesting();
        expression
    }

    fn enter_nesting(&mut self, offset: usize) -> Result<(), ParseError> {
        if self.syntactic_nesting >= MAX_AST_DEPTH {
            return Err(ParseError::new(
                self.rule_name,
                offset,
                format!("an expression nesting depth of at most {MAX_AST_DEPTH}"),
            ));
        }
        self.syntactic_nesting += 1;
        Ok(())
    }

    fn exit_nesting(&mut self) {
        debug_assert!(self.syntactic_nesting > 0);
        self.syntactic_nesting -= 1;
    }

    fn expect_colon(&mut self) -> Result<Span, ParseError> {
        if !matches!(self.current.kind, TokenKind::Colon) {
            return Err(self.error_at_current(":"));
        }
        let span = self.current.span;
        self.advance()?;
        Ok(span)
    }

    fn expect_right_paren(&mut self) -> Result<Span, ParseError> {
        if !matches!(self.current.kind, TokenKind::RightParen) {
            return Err(self.error_at_current("a closing )"));
        }
        let span = self.current.span;
        self.advance()?;
        Ok(span)
    }

    fn expect_right_bracket(&mut self) -> Result<Span, ParseError> {
        if !matches!(self.current.kind, TokenKind::RightBracket) {
            return Err(self.error_at_current("a closing ]"));
        }
        let span = self.current.span;
        self.advance()?;
        Ok(span)
    }

    fn advance(&mut self) -> Result<(), ParseError> {
        self.current = self.lexer.next()?;
        Ok(())
    }

    fn error_at_current(&self, expectation: impl Into<String>) -> ParseError {
        ParseError::new(self.rule_name, self.current.span.start, expectation)
    }
}

fn expression_depth(expression: &Expr) -> usize {
    let mut maximum = 0;
    let mut pending = vec![(expression, 1)];

    while let Some((current, depth)) = pending.pop() {
        maximum = maximum.max(depth);
        match &current.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::List(items)
            | ExprKind::Call {
                arguments: items, ..
            } => pending.extend(items.iter().map(|item| (item, depth + 1))),
            ExprKind::Member { target, .. } | ExprKind::Index { target, .. } => {
                pending.push((target, depth + 1));
            }
            ExprKind::Unary { operand, .. } => pending.push((operand, depth + 1)),
            ExprKind::Binary { left, right, .. } => {
                pending.push((left, depth + 1));
                pending.push((right, depth + 1));
            }
            ExprKind::Conditional {
                condition,
                when_true,
                when_false,
            } => {
                pending.push((condition, depth + 1));
                pending.push((when_true, depth + 1));
                pending.push((when_false, depth + 1));
            }
        }
    }

    maximum
}
