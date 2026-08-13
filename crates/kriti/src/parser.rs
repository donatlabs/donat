//! Reading a template.
//!
//! The grammar is JSON with three additions in value position — an expression,
//! a conditional and a loop, each written between `{{` and `}}` — plus
//! interpolation inside string literals. Everything nests: an expression may
//! contain a template, a template may contain a string, and that string may
//! interpolate another expression. That is why this is one recursive
//! descent over the source rather than a lexer and a parser: the meaning of
//! `}}` depends on what opened it, and a token stream would have to guess.

use serde_json::Value as Json;

/// A value position in the template.
#[derive(Debug, Clone)]
pub enum Node {
    /// Literal JSON containing no holes.
    Json(Json),
    /// A string literal, which may interpolate.
    Str(Vec<StrPart>),
    Array(Vec<Node>),
    Object(Vec<(Vec<StrPart>, Node)>),
    Expr(Expr),
    /// `if` / `elif` … / `else`. The last arm's condition is absent.
    If {
        arms: Vec<(Expr, Node)>,
        otherwise: Option<Box<Node>>,
    },
    /// `range i, x := source` — evaluates to an array of the body per element.
    Range {
        index: Option<String>,
        value: String,
        source: Expr,
        body: Box<Node>,
    },
}

#[derive(Debug, Clone)]
pub enum StrPart {
    Text(String),
    Hole(Expr),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Lit(Json),
    Str(Vec<StrPart>),
    Array(Vec<Expr>),
    Object(Vec<(Vec<StrPart>, Expr)>),
    /// A binding, and whether a missing one is `null` rather than an error.
    Var {
        name: String,
        optional: bool,
    },
    Path {
        base: Box<Expr>,
        steps: Vec<Step>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Not(Box<Expr>),
    Call {
        name: String,
        args: Vec<Expr>,
    },
    /// A whole template used where a value is expected — `concat({{ range … }})`.
    Template(Box<Node>),
}

#[derive(Debug, Clone)]
pub struct Step {
    pub kind: StepKind,
    /// `?.`, `?[` — a missing value here is `null`, and the rest of the chain
    /// is skipped rather than applied to it.
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub enum StepKind {
    Field(String),
    Index(i64),
    Key(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `??`
    Default,
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
}

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

pub fn parse(source: &str) -> Result<Node, ParseError> {
    let mut parser = Parser {
        src: source.as_bytes(),
        pos: 0,
    };
    parser.skip_trivia();
    let node = parser.node()?;
    parser.skip_trivia();
    if parser.pos < parser.src.len() {
        return Err(parser.error("trailing input after the template"));
    }
    Ok(node)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            offset: self.pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        self.src[self.pos.min(self.src.len())..].starts_with(text.as_bytes())
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.starts_with(text) {
            self.pos += text.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, text: &str) -> Result<(), ParseError> {
        if self.eat(text) {
            Ok(())
        } else {
            Err(self.error(format!("expected {text:?}")))
        }
    }

    /// Whitespace and `#` comments, which are allowed wherever whitespace is.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b'#') => {
                    while let Some(byte) = self.peek() {
                        self.pos += 1;
                        if byte == b'\n' {
                            break;
                        }
                    }
                }
                _ => return,
            }
        }
    }

    // ----- value positions -------------------------------------------------

    fn node(&mut self) -> Result<Node, ParseError> {
        self.skip_trivia();
        if self.starts_with("{{") {
            return self.block();
        }
        match self.peek() {
            Some(b'{') => self.object_node(),
            Some(b'[') => self.array_node(),
            Some(b'"') => Ok(Node::Str(self.string_parts()?)),
            Some(_) => Ok(Node::Json(self.json_scalar()?)),
            None => Err(self.error("expected a value")),
        }
    }

    /// `{{ … }}` in value position: an expression, a conditional or a loop.
    fn block(&mut self) -> Result<Node, ParseError> {
        self.expect("{{")?;
        self.skip_trivia();
        if self.keyword("if") {
            return self.if_block();
        }
        if self.keyword("range") {
            return self.range_block();
        }
        let expr = self.expr()?;
        self.skip_trivia();
        self.expect("}}")?;
        Ok(Node::Expr(expr))
    }

    /// A bare keyword — matched only when what follows cannot continue an
    /// identifier, so a binding called `iffy` is not read as `if`.
    fn keyword(&mut self, word: &str) -> bool {
        if !self.starts_with(word) {
            return false;
        }
        match self.src.get(self.pos + word.len()) {
            Some(byte) if is_ident_byte(*byte) => false,
            _ => {
                self.pos += word.len();
                true
            }
        }
    }

    fn if_block(&mut self) -> Result<Node, ParseError> {
        let mut arms = Vec::new();
        let mut otherwise = None;
        loop {
            let condition = self.expr()?;
            self.skip_trivia();
            self.expect("}}")?;
            let body = self.node()?;
            arms.push((condition, body));

            self.skip_trivia();
            self.expect("{{")?;
            self.skip_trivia();
            if self.keyword("elif") {
                continue;
            }
            if self.keyword("else") {
                self.skip_trivia();
                self.expect("}}")?;
                otherwise = Some(Box::new(self.node()?));
                self.skip_trivia();
                self.expect("{{")?;
                self.skip_trivia();
            }
            if self.keyword("end") {
                self.skip_trivia();
                self.expect("}}")?;
                return Ok(Node::If { arms, otherwise });
            }
            return Err(self.error("expected 'elif', 'else' or 'end'"));
        }
    }

    fn range_block(&mut self) -> Result<Node, ParseError> {
        self.skip_trivia();
        let first = self.ident()?;
        self.skip_trivia();
        let (index, value) = if self.eat(",") {
            self.skip_trivia();
            let second = self.ident()?;
            (if first == "_" { None } else { Some(first) }, second)
        } else {
            // `range x := …` binds only the element.
            (None, first)
        };
        self.skip_trivia();
        self.expect(":=")?;
        let source = self.expr()?;
        self.skip_trivia();
        self.expect("}}")?;
        let body = Box::new(self.node()?);
        self.skip_trivia();
        self.expect("{{")?;
        self.skip_trivia();
        if !self.keyword("end") {
            return Err(self.error("expected 'end' to close 'range'"));
        }
        self.skip_trivia();
        self.expect("}}")?;
        Ok(Node::Range {
            index,
            value,
            source,
            body,
        })
    }

    fn array_node(&mut self) -> Result<Node, ParseError> {
        self.expect("[")?;
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            if self.eat("]") {
                return Ok(Node::Array(items));
            }
            items.push(self.node()?);
            self.skip_trivia();
            if self.eat(",") {
                continue;
            }
            self.expect("]")?;
            return Ok(Node::Array(items));
        }
    }

    fn object_node(&mut self) -> Result<Node, ParseError> {
        self.expect("{")?;
        let mut members = Vec::new();
        loop {
            self.skip_trivia();
            if self.eat("}") {
                return Ok(Node::Object(members));
            }
            let key = self.string_parts()?;
            self.skip_trivia();
            self.expect(":")?;
            let value = self.node()?;
            members.push((key, value));
            self.skip_trivia();
            if self.eat(",") {
                continue;
            }
            self.expect("}")?;
            return Ok(Node::Object(members));
        }
    }

    // ----- scalars and strings ---------------------------------------------

    /// A JSON scalar (number, `true`, `false`, `null`), read exactly as JSON
    /// reads it.
    fn json_scalar(&mut self) -> Result<Json, ParseError> {
        let start = self.pos;
        if self.keyword("true") {
            return Ok(Json::Bool(true));
        }
        if self.keyword("false") {
            return Ok(Json::Bool(false));
        }
        if self.keyword("null") {
            return Ok(Json::Null);
        }
        while let Some(byte) = self.peek() {
            if byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(self.error("expected a value"));
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        serde_json::from_str(text).map_err(|_| ParseError {
            message: format!("{text:?} is not a number"),
            offset: start,
        })
    }

    /// A string literal, split into text and interpolations.
    fn string_parts(&mut self) -> Result<Vec<StrPart>, ParseError> {
        self.expect("\"")?;
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            if self.starts_with("{{") {
                if !text.is_empty() {
                    parts.push(StrPart::Text(std::mem::take(&mut text)));
                }
                self.pos += 2;
                let expr = self.expr()?;
                self.skip_trivia();
                self.expect("}}")?;
                parts.push(StrPart::Hole(expr));
                continue;
            }
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    if !text.is_empty() || parts.is_empty() {
                        parts.push(StrPart::Text(text));
                    }
                    return Ok(parts);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let escape = self
                        .peek()
                        .ok_or_else(|| self.error("unterminated escape"))?;
                    self.pos += 1;
                    match escape {
                        b'"' => text.push('"'),
                        b'\\' => text.push('\\'),
                        b'/' => text.push('/'),
                        b'b' => text.push('\u{8}'),
                        b'f' => text.push('\u{c}'),
                        b'n' => text.push('\n'),
                        b'r' => text.push('\r'),
                        b't' => text.push('\t'),
                        b'u' => {
                            let hex = self
                                .src
                                .get(self.pos..self.pos + 4)
                                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                                .ok_or_else(|| self.error("truncated \\u escape"))?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| self.error("invalid \\u escape"))?;
                            self.pos += 4;
                            text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => {
                            return Err(self.error(format!("unknown escape \\{}", other as char)));
                        }
                    }
                }
                Some(_) => {
                    // Copy one UTF-8 character.
                    let start = self.pos;
                    self.pos += 1;
                    while self.peek().is_some_and(|byte| (0x80..0xC0).contains(&byte)) {
                        self.pos += 1;
                    }
                    text.push_str(
                        std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("\u{fffd}"),
                    );
                }
            }
        }
    }

    fn ident(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'$') {
            self.pos += 1;
        }
        while self.peek().is_some_and(is_ident_byte) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.error("expected an identifier"));
        }
        Ok(String::from_utf8_lossy(&self.src[start..self.pos]).into_owned())
    }

    // ----- expressions -----------------------------------------------------

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.default_expr()
    }

    /// `??` — the loosest binding, so `a.b ?? c || d` defaults the whole right.
    fn default_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.or_expr()?;
        loop {
            self.skip_trivia();
            if self.eat("??") {
                let rhs = self.or_expr()?;
                lhs = Expr::Binary {
                    op: BinOp::Default,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                };
            } else {
                return Ok(lhs);
            }
        }
    }

    fn or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.and_expr()?;
        loop {
            self.skip_trivia();
            if self.eat("||") {
                let rhs = self.and_expr()?;
                lhs = Expr::Binary {
                    op: BinOp::Or,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                };
            } else {
                return Ok(lhs);
            }
        }
    }

    fn and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.compare_expr()?;
        loop {
            self.skip_trivia();
            if self.eat("&&") {
                let rhs = self.compare_expr()?;
                lhs = Expr::Binary {
                    op: BinOp::And,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                };
            } else {
                return Ok(lhs);
            }
        }
    }

    fn compare_expr(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.unary_expr()?;
        self.skip_trivia();
        // Two characters before one, or `<=` reads as `<` and leaves `=`.
        let op = if self.eat("==") {
            BinOp::Eq
        } else if self.eat("!=") {
            BinOp::Ne
        } else if self.eat("<=") {
            BinOp::Le
        } else if self.eat(">=") {
            BinOp::Ge
        } else if self.eat("<") {
            BinOp::Lt
        } else if self.eat(">") {
            BinOp::Gt
        } else if self.keyword("in") {
            BinOp::In
        } else {
            return Ok(lhs);
        };
        let rhs = self.unary_expr()?;
        Ok(Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        })
    }

    fn unary_expr(&mut self) -> Result<Expr, ParseError> {
        self.skip_trivia();
        if self.keyword("not") {
            let inner = self.unary_expr()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.postfix_expr()
    }

    /// An atom plus whatever path steps follow it.
    fn postfix_expr(&mut self) -> Result<Expr, ParseError> {
        let base = self.atom()?;
        let mut steps = Vec::new();
        loop {
            // No `skip_trivia` here: `a .b` is not a path step, and treating it
            // as one would swallow the separator in `[a .b]`.
            let optional = if self.starts_with("?.") || self.starts_with("?[") {
                self.pos += 1;
                true
            } else {
                false
            };
            if self.eat(".") {
                let name = self.ident()?;
                steps.push(Step {
                    kind: StepKind::Field(name),
                    optional,
                });
                continue;
            }
            if self.eat("[") {
                self.skip_trivia();
                let kind = if self.peek() == Some(b'"') || self.peek() == Some(b'\'') {
                    StepKind::Key(self.quoted_key()?)
                } else {
                    let start = self.pos;
                    while self
                        .peek()
                        .is_some_and(|byte| byte.is_ascii_digit() || byte == b'-')
                    {
                        self.pos += 1;
                    }
                    let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
                    StepKind::Index(text.parse().map_err(|_| ParseError {
                        message: "expected an array index".to_string(),
                        offset: start,
                    })?)
                };
                self.skip_trivia();
                self.expect("]")?;
                steps.push(Step { kind, optional });
                continue;
            }
            if optional {
                // A lone `?` was consumed above only when a step followed it.
                return Err(self.error("expected '.' or '[' after '?'"));
            }
            if steps.is_empty() {
                return Ok(base);
            }
            return Ok(Expr::Path {
                base: Box::new(base),
                steps,
            });
        }
    }

    /// `'a key'` or `"a key"` inside `[...]`.
    fn quoted_key(&mut self) -> Result<String, ParseError> {
        let quote = self.peek().unwrap();
        self.pos += 1;
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte == quote {
                let text = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
                self.pos += 1;
                return Ok(text);
            }
            self.pos += 1;
        }
        Err(self.error("unterminated key"))
    }

    fn atom(&mut self) -> Result<Expr, ParseError> {
        self.skip_trivia();
        if self.starts_with("{{") {
            self.pos += 2;
            self.skip_trivia();
            let node = if self.keyword("if") {
                self.if_block()?
            } else if self.keyword("range") {
                self.range_block()?
            } else {
                let expr = self.expr()?;
                self.skip_trivia();
                self.expect("}}")?;
                return Ok(expr);
            };
            return Ok(Expr::Template(Box::new(node)));
        }
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                let inner = self.expr()?;
                self.skip_trivia();
                self.expect(")")?;
                Ok(inner)
            }
            Some(b'"') => Ok(Expr::Str(self.string_parts()?)),
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_trivia();
                    if self.eat("]") {
                        return Ok(Expr::Array(items));
                    }
                    items.push(self.expr()?);
                    self.skip_trivia();
                    if self.eat(",") {
                        continue;
                    }
                    self.expect("]")?;
                    return Ok(Expr::Array(items));
                }
            }
            Some(b'{') => {
                self.pos += 1;
                let mut members = Vec::new();
                loop {
                    self.skip_trivia();
                    if self.eat("}") {
                        return Ok(Expr::Object(members));
                    }
                    let key = self.string_parts()?;
                    self.skip_trivia();
                    self.expect(":")?;
                    let value = self.expr()?;
                    members.push((key, value));
                    self.skip_trivia();
                    if self.eat(",") {
                        continue;
                    }
                    self.expect("}")?;
                    return Ok(Expr::Object(members));
                }
            }
            Some(byte) if byte == b'$' || byte.is_ascii_alphabetic() || byte == b'_' => {
                if self.starts_with("true") || self.starts_with("false") || self.starts_with("null")
                {
                    let literal = self.json_scalar()?;
                    return Ok(Expr::Lit(literal));
                }
                let name = self.ident()?;
                // A call, when what follows is an argument list.
                if self.peek() == Some(b'(') {
                    self.pos += 1;
                    let mut args = Vec::new();
                    loop {
                        self.skip_trivia();
                        if self.eat(")") {
                            break;
                        }
                        args.push(self.expr()?);
                        self.skip_trivia();
                        if self.eat(",") {
                            continue;
                        }
                        self.expect(")")?;
                        break;
                    }
                    return Ok(Expr::Call { name, args });
                }
                // `$foo?` — a binding that may be absent.
                let optional =
                    self.starts_with("?") && !self.starts_with("?.") && !self.starts_with("?[");
                if optional {
                    self.pos += 1;
                }
                Ok(Expr::Var { name, optional })
            }
            Some(_) => Ok(Expr::Lit(self.json_scalar()?)),
            None => Err(self.error("expected an expression")),
        }
    }
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}
