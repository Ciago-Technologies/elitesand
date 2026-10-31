//! CiaQL lexer, recursive-descent parser and semantic validator.
//!
//! Spec defects in `parse_ciaql`:
//! * The stage loop is an `if / else if` chain over FILTER, TAKE, CHECKPOINT_FRAME,
//!   HYDRATE_FRAME with **no final `else`**: `MUTATE`, `PROJECT`, `GROUP_BY`,
//!   `AGGREGATE`/`REDUCE` and `SIMILARITY` are silently skipped. Example 1 "succeeds"
//!   and debits nothing; Example 3 returns ungrouped rows.
//! * `TX_ACTORS[...]` is not a recognized source, so Example 2 fails outright.
//! * `query.split("|>")` and `body.split("AND")` cut inside string literals and
//!   identifiers (`state.BRAND`, `"a|>b"`), and `OR`, `IN`, `CONTAINS`, parentheses are
//!   not parsed at all.
//!
//! Fix: a real tokenizer (spans, literal-aware), one parse function per grammar rule,
//! an `Err` for any unknown stage keyword, and a validator for rules the grammar alone
//! cannot express (MUTATE only on transactional sources, `actor(..)` qualifiers must be
//! participants, GROUP_BY must feed an aggregate, TAKE last, ...).
//!
//! Grammar extension (needed by the spec's own Example 1, which the spec grammar does
//! not derive): `Mutation ::= FieldPath ("=" | "+=" | "-=") Value
//!                          | FieldPath "." "append" "(" Value ")"`
//! and `Value` adds object literals `{ key: Value, ... }` for the append argument.
//!
//! All AST strings borrow from the query (`&'a str`); allocation is bounded by the
//! limits below, so a hostile query cannot exhaust the control-plane budget.

use core::fmt;

pub const MAX_QUERY_BYTES: usize = 64 * 1024;
pub const MAX_TOKENS: usize = 16384;
pub const MAX_STAGES: usize = 32;
pub const MAX_LIST: usize = 256;
pub const MAX_DEPTH: usize = 16;
pub const MAX_VECTOR_DIMS: usize = 4096;
pub const MAX_TX_ACTORS: usize = 64;

// =================================================================================
// Errors
// =================================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    pub at: u32,
    pub end: u32,
    pub msg: &'static str,
}

impl ParseError {
    /// 1-based (line, column) of the error in `src`.
    pub fn line_col(&self, src: &str) -> (usize, usize) {
        let upto = &src[..(self.at as usize).min(src.len())];
        let line = upto.matches('\n').count() + 1;
        let col = upto.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        (line, col)
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}..{}", self.msg, self.at, self.end)
    }
}
impl std::error::Error for ParseError {}

type PResult<T> = Result<T, ParseError>;

// =================================================================================
// Lexer
// =================================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok<'a> {
    Ident(&'a str),
    /// Raw integer text (decimal or `0x` hex, underscores allowed, optional `-`).
    Int(&'a str),
    /// Raw float text.
    Float(&'a str),
    /// String contents between the quotes, escapes not yet processed.
    Str(&'a str),
    /// `$ref:0x...` hex digits.
    Ref(&'a str),
    At,
    Pipe,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    Comma,
    Colon,
    Dot,
    DotDot,
    Star,
    EqEq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
    Assign,
    AddAssign,
    SubAssign,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    pub tok: Tok<'a>,
    pub at: u32,
    pub end: u32,
}

fn err(at: usize, end: usize, msg: &'static str) -> ParseError {
    ParseError {
        at: at as u32,
        end: end as u32,
        msg,
    }
}

pub fn lex(src: &str) -> PResult<Vec<Token<'_>>> {
    if src.len() > MAX_QUERY_BYTES {
        return Err(err(0, src.len(), "query exceeds 64 KiB"));
    }
    let b = src.as_bytes();
    let mut out = Vec::with_capacity((src.len() / 4).min(MAX_TOKENS));
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let two = |s: &[u8]| b.get(i..i + 2) == Some(s);
        let tok = if two(b"|>") {
            i += 2;
            Tok::Pipe
        } else if two(b"..") {
            i += 2;
            Tok::DotDot
        } else if two(b"==") {
            i += 2;
            Tok::EqEq
        } else if two(b"!=") {
            i += 2;
            Tok::Ne
        } else if two(b">=") {
            i += 2;
            Tok::Ge
        } else if two(b"<=") {
            i += 2;
            Tok::Le
        } else if two(b"+=") {
            i += 2;
            Tok::AddAssign
        } else if two(b"-=") {
            i += 2;
            Tok::SubAssign
        } else if c == b'"' {
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err(err(start, b.len(), "unterminated string literal")),
                    Some(b'\\') => i += 2,
                    Some(b'"') => break,
                    Some(_) => i += 1,
                }
            }
            i += 1;
            let raw = &src[start + 1..i - 1];
            unescape(raw).map_err(|_| err(start, i, "invalid string escape"))?;
            Tok::Str(raw)
        } else if c == b'$' {
            let rest = &src[i..];
            let Some(hex) = rest
                .strip_prefix("$ref:0x")
                .or_else(|| rest.strip_prefix("$ref:0X"))
            else {
                return Err(err(start, start + 1, "expected `$ref:0x<hex>`"));
            };
            let n = hex
                .bytes()
                .take_while(|c| c.is_ascii_hexdigit() || *c == b'_')
                .count();
            if n == 0 {
                return Err(err(start, start + 7, "empty `$ref` handle"));
            }
            i += 7 + n;
            if b.get(i) != Some(&b'.') {
                return Err(err(start, i, "reference requires a 128-bit MAC"));
            }
            i += 1;
            let mac_start = i;
            while b.get(i).is_some_and(u8::is_ascii_hexdigit) {
                i += 1;
            }
            if i - mac_start != 32 {
                return Err(err(start, i, "reference MAC must contain 32 hex digits"));
            }
            Tok::Ref(&src[start + 7..i])
        } else if c.is_ascii_digit() || (c == b'-' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            if c == b'-' {
                i += 1;
            }
            let mut float = false;
            if b[i] == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X')) {
                i += 2;
                while i < b.len() && (b[i].is_ascii_hexdigit() || b[i] == b'_') {
                    i += 1;
                }
            } else {
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                    i += 1;
                }
                // `1..5` is Int DotDot Int, not a float.
                if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    float = true;
                    i += 1;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                        i += 1;
                    }
                }
                if matches!(b.get(i), Some(b'e' | b'E')) {
                    let mut j = i + 1;
                    if matches!(b.get(j), Some(b'+' | b'-')) {
                        j += 1;
                    }
                    if b.get(j).is_some_and(u8::is_ascii_digit) {
                        float = true;
                        i = j;
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
            }
            if b.get(i)
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
            {
                return Err(err(start, i + 1, "malformed number"));
            }
            if float {
                Tok::Float(&src[start..i])
            } else {
                Tok::Int(&src[start..i])
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            Tok::Ident(&src[start..i])
        } else {
            i += 1;
            match c {
                b'@' => Tok::At,
                b'(' => Tok::LParen,
                b')' => Tok::RParen,
                b'{' => Tok::LBrace,
                b'}' => Tok::RBrace,
                b'[' => Tok::LBrack,
                b']' => Tok::RBrack,
                b',' => Tok::Comma,
                b':' => Tok::Colon,
                b'.' => Tok::Dot,
                b'*' => Tok::Star,
                b'>' => Tok::Gt,
                b'<' => Tok::Lt,
                b'=' => Tok::Assign,
                _ => return Err(err(start, i, "unexpected character")),
            }
        };
        if out.len() >= MAX_TOKENS {
            return Err(err(start, i, "too many tokens"));
        }
        out.push(Token {
            tok,
            at: start as u32,
            end: i as u32,
        });
    }
    out.push(Token {
        tok: Tok::Eof,
        at: b.len() as u32,
        end: b.len() as u32,
    });
    Ok(out)
}

// =================================================================================
// AST
// =================================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct Query<'a> {
    pub source: Source,
    pub stages: Vec<Stage<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Actor(u64),
    /// Deduplicated, in query order.
    TxActors(Vec<u64>),
    ScanRange {
        start: u64,
        end: u64,
    },
    ScanAll,
    BackupStore {
        destination: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    State,
    Meta,
    Frame,
    Episodes,
    Semantic,
}

impl Root {
    fn from_ident(s: &str) -> Option<Root> {
        Some(match s {
            "state" => Root::State,
            "meta" => Root::Meta,
            "frame" => Root::Frame,
            "episodes" => Root::Episodes,
            "semantic" => Root::Semantic,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldPath<'a> {
    /// `actor(0x..)` qualifier (TX_ACTORS queries only).
    pub actor: Option<u64>,
    pub alias: Option<&'a str>,
    pub root: Root,
    /// Segments after the root, e.g. `["wallet", "credits"]`.
    pub path: Vec<&'a str>,
    pub at: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Literal<'a> {
    Int(i64),
    /// Integers above `i64::MAX` (ids, hashes).
    UInt(u64),
    Float(f64),
    /// Raw string contents; see `unescape`.
    Str(&'a str),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefLiteral {
    pub id: u64,
    pub mac: [u8; 16],
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    Lit(Literal<'a>),
    Ref(RefLiteral),
    Array(Vec<Value<'a>>),
    Object(Vec<(&'a str, Value<'a>)>),
    Call { name: &'a str, args: Vec<Value<'a>> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    In,
    Contains,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Predicate<'a> {
    pub field: FieldPath<'a>,
    pub op: CompOp,
    /// `Value::Array` for `IN`, otherwise `Value::Lit`.
    pub rhs: Value<'a>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BoolExpr<'a> {
    Pred(Predicate<'a>),
    And(Vec<BoolExpr<'a>>),
    Or(Vec<BoolExpr<'a>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Cosine,
    L2,
    Dot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mutation<'a> {
    Assign {
        target: FieldPath<'a>,
        op: AssignOp,
        value: Value<'a>,
    },
    Append {
        target: FieldPath<'a>,
        value: Value<'a>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectItem<'a> {
    pub alias: &'a str,
    pub field: FieldPath<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    CountDistinct,
    P50,
    P95,
    P99,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggItem<'a> {
    pub alias: &'a str,
    pub func: AggFunc,
    pub arg: Option<FieldPath<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stage<'a> {
    Filter(BoolExpr<'a>),
    Similarity {
        vector: Vec<f32>,
        metric: Metric,
        threshold: f32,
    },
    Mutate(Vec<Mutation<'a>>),
    Project(Vec<ProjectItem<'a>>),
    GroupBy(Vec<FieldPath<'a>>),
    Reduce(Vec<AggItem<'a>>),
    CheckpointFrame {
        intent: &'a str,
        constraints: Vec<&'a str>,
    },
    HydrateFrame {
        frame_hash: u64,
    },
    Take(u64),
    SearchBm25 {
        field: Vec<&'a str>,
        query: &'a str,
        top_k: u64,
    },
    ExportTo {
        format: &'a str,
        destination: &'a str,
    },
    JoinActor {
        target: FieldPath<'a>,
        alias: &'a str,
    },
    LinkAdd {
        target: u64,
        cascade: bool,
    },
}

impl Stage<'_> {
    pub fn name(&self) -> &'static str {
        match self {
            Stage::Filter(_) => "FILTER",
            Stage::Similarity { .. } => "SIMILARITY",
            Stage::Mutate(_) => "MUTATE",
            Stage::Project(_) => "PROJECT",
            Stage::GroupBy(_) => "GROUP_BY",
            Stage::Reduce(_) => "AGGREGATE",
            Stage::CheckpointFrame { .. } => "CHECKPOINT_FRAME",
            Stage::HydrateFrame { .. } => "HYDRATE_FRAME",
            Stage::Take(_) => "TAKE",
            Stage::SearchBm25 { .. } => "SEARCH",
            Stage::ExportTo { .. } => "EXPORT",
            Stage::JoinActor { .. } => "JOIN",
            Stage::LinkAdd { .. } => "LINK",
        }
    }
}

/// Process `\" \\ \n \t \r` escapes. Borrowed when there are none.
pub fn unescape(raw: &str) -> Result<std::borrow::Cow<'_, str>, &'static str> {
    if !raw.contains('\\') {
        return Ok(std::borrow::Cow::Borrowed(raw));
    }
    let mut s = String::with_capacity(raw.len());
    let mut it = raw.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            s.push(c);
            continue;
        }
        s.push(match it.next() {
            Some('"') => '"',
            Some('\\') => '\\',
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            _ => return Err("invalid escape"),
        });
    }
    Ok(std::borrow::Cow::Owned(s))
}

// =================================================================================
// Parser
// =================================================================================

struct Parser<'a> {
    toks: Vec<Token<'a>>,
    pos: usize,
    depth: usize,
}

fn parse_uint(raw: &str, at: u32, end: u32) -> PResult<u64> {
    let mk = |m| ParseError { at, end, msg: m };
    if raw.starts_with('-') {
        return Err(mk("expected a non-negative integer"));
    }
    let digits: String = raw.chars().filter(|&c| c != '_').collect();
    if raw.starts_with('_') || raw.ends_with('_') || raw.contains("__") {
        return Err(mk("misplaced `_` in number"));
    }
    let r = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(h) if !h.is_empty() => u64::from_str_radix(h, 16),
        Some(_) => return Err(mk("empty hex literal")),
        None => digits.parse::<u64>(),
    };
    r.map_err(|_| mk("integer out of range"))
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Tok<'a> {
        self.toks[self.pos].tok
    }
    fn peek2(&self) -> Tok<'a> {
        self.toks.get(self.pos + 1).map_or(Tok::Eof, |t| t.tok)
    }
    fn tok(&self) -> Token<'a> {
        self.toks[self.pos]
    }
    fn bump(&mut self) -> Token<'a> {
        let t = self.toks[self.pos];
        if t.tok != Tok::Eof {
            self.pos += 1;
        }
        t
    }
    fn fail<T>(&self, msg: &'static str) -> PResult<T> {
        let t = self.tok();
        Err(ParseError {
            at: t.at,
            end: t.end,
            msg,
        })
    }
    fn expect(&mut self, want: Tok<'static>, msg: &'static str) -> PResult<Token<'a>> {
        if self.peek() == want {
            Ok(self.bump())
        } else {
            self.fail(msg)
        }
    }
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s.eq_ignore_ascii_case(kw))
    }
    fn expect_kw(&mut self, kw: &'static str, msg: &'static str) -> PResult<()> {
        if self.is_kw(kw) {
            self.bump();
            Ok(())
        } else {
            self.fail(msg)
        }
    }
    fn ident(&mut self, msg: &'static str) -> PResult<&'a str> {
        match self.peek() {
            Tok::Ident(s) => {
                self.bump();
                Ok(s)
            }
            _ => self.fail(msg),
        }
    }
    fn uint(&mut self) -> PResult<u64> {
        let t = self.tok();
        match t.tok {
            Tok::Int(raw) => {
                self.bump();
                parse_uint(raw, t.at, t.end)
            }
            _ => self.fail("expected an integer"),
        }
    }
    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.fail("nesting too deep")
        } else {
            Ok(())
        }
    }
    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Comma-separated list until `close`, at least one element, bounded length.
    fn list<T>(
        &mut self,
        close: Tok<'static>,
        item: impl FnMut(&mut Self) -> PResult<T>,
    ) -> PResult<Vec<T>> {
        self.list_bounded(close, MAX_LIST, item)
    }
    fn list_bounded<T>(
        &mut self,
        close: Tok<'static>,
        limit: usize,
        mut item: impl FnMut(&mut Self) -> PResult<T>,
    ) -> PResult<Vec<T>> {
        let mut v = Vec::new();
        loop {
            if v.len() == limit {
                return self.fail("list too long");
            }
            v.push(item(self)?);
            match self.peek() {
                Tok::Comma => {
                    self.bump();
                }
                t if t == close => {
                    self.bump();
                    return Ok(v);
                }
                _ => return self.fail("expected `,` or closing delimiter"),
            }
        }
    }

    // ---------------------------------------------------------------- source

    fn source(&mut self) -> PResult<Source> {
        if self.peek() == Tok::At {
            self.bump();
            return Ok(Source::Actor(self.uint()?));
        }
        let keyword = match self.peek() {
            Tok::Ident(k) => k.to_ascii_uppercase(),
            _ => String::new(),
        };
        match keyword.as_str() {
            "ACTOR" => {
                self.bump();
                self.expect(Tok::LParen, "expected `(` after ACTOR")?;
                let id = self.uint()?;
                self.expect(Tok::RParen, "expected `)`")?;
                Ok(Source::Actor(id))
            }
            "TX_ACTORS" | "TX" => {
                self.bump();
                let open = self.expect(Tok::LBrack, "expected `[` after TX_ACTORS")?;
                let ids = self.list(Tok::RBrack, |p| {
                    if p.peek() == Tok::At {
                        p.bump();
                    }
                    p.uint()
                })?;
                let mut dedup: Vec<u64> = Vec::with_capacity(ids.len());
                for id in ids {
                    if dedup.contains(&id) {
                        return Err(ParseError {
                            at: open.at,
                            end: self.toks[self.pos - 1].end,
                            msg: "duplicate actor in TX_ACTORS",
                        });
                    }
                    dedup.push(id);
                }
                if dedup.len() < 2 || dedup.len() > MAX_TX_ACTORS {
                    return Err(ParseError {
                        at: open.at,
                        end: self.toks[self.pos - 1].end,
                        msg: "TX_ACTORS needs 2..=64 distinct actors",
                    });
                }
                Ok(Source::TxActors(dedup))
            }
            "BACKUP" => {
                self.bump();
                self.expect_kw("STORE", "expected STORE")?;
                self.expect_kw("TO", "expected TO")?;
                let destination = unescape(self.string()?)
                    .map_err(|_| err(0, 0, "invalid destination escape"))?
                    .into_owned();
                Ok(Source::BackupStore { destination })
            }
            "SCAN" => {
                self.bump();
                self.expect_kw("ACTORS", "expected `ACTORS` after SCAN")?;
                self.expect(Tok::LBrack, "expected `[`")?;
                if self.peek() == Tok::Star {
                    self.bump();
                    self.expect(Tok::RBrack, "expected `]`")?;
                    return Ok(Source::ScanAll);
                }
                let at = self.tok().at;
                let start = self.uint()?;
                self.expect(Tok::DotDot, "expected `..` in scan range")?;
                let end = self.uint()?;
                let close = self.expect(Tok::RBrack, "expected `]`")?;
                if start > end {
                    return Err(ParseError {
                        at,
                        end: close.end,
                        msg: "scan range start > end",
                    });
                }
                Ok(Source::ScanRange { start, end })
            }
            _ => self.fail("expected source: ACTOR(..), TX_ACTORS[..] or SCAN ACTORS[..]"),
        }
    }

    // ---------------------------------------------------------------- paths & values

    fn field_path(&mut self) -> PResult<FieldPath<'a>> {
        let at = self.tok().at;
        let mut actor = None;
        let mut alias = None;
        if self.peek() == Tok::At {
            self.bump();
            alias = Some(self.ident("expected join alias")?);
            self.expect(Tok::Dot, "expected . after alias")?;
        }
        if alias.is_none() && self.is_kw("actor") && self.peek2() == Tok::LParen {
            self.bump();
            self.bump();
            actor = Some(self.uint()?);
            self.expect(Tok::RParen, "expected `)` after actor id")?;
            self.expect(Tok::Dot, "expected `.` after actor(..)")?;
        }
        let root_name = self.ident("expected field path")?;
        let known_root = Root::from_ident(root_name);
        if known_root.is_none() && self.peek() == Tok::Dot {
            return self
                .fail("field path must start with state, meta, frame, episodes or semantic");
        }
        let root = known_root.unwrap_or(Root::State);
        let mut path = if known_root.is_some() {
            Vec::new()
        } else {
            vec![root_name]
        };
        while self.peek() == Tok::Dot {
            self.bump();
            if path.len() == MAX_DEPTH {
                return self.fail("field path too deep");
            }
            path.push(self.ident("expected identifier after `.`")?);
        }
        Ok(FieldPath {
            actor,
            alias,
            root,
            path,
            at,
        })
    }

    fn literal(&mut self) -> PResult<Literal<'a>> {
        let t = self.tok();
        let lit = match t.tok {
            Tok::Str(s) => Literal::Str(s),
            Tok::Int(raw) => {
                if let Some(neg) = raw.strip_prefix('-') {
                    let v = parse_uint(neg, t.at, t.end)?;
                    if v > i64::MAX as u64 + 1 {
                        return Err(err(t.at as usize, t.end as usize, "integer out of range"));
                    }
                    Literal::Int((v as i64).wrapping_neg())
                } else {
                    let v = parse_uint(raw, t.at, t.end)?;
                    if v <= i64::MAX as u64 {
                        Literal::Int(v as i64)
                    } else {
                        Literal::UInt(v)
                    }
                }
            }
            Tok::Float(raw) => {
                let s: String = raw.chars().filter(|&c| c != '_').collect();
                let v: f64 = s
                    .parse()
                    .map_err(|_| err(t.at as usize, t.end as usize, "malformed float"))?;
                if !v.is_finite() {
                    return Err(err(t.at as usize, t.end as usize, "float out of range"));
                }
                Literal::Float(v)
            }
            Tok::Ident("true") => Literal::Bool(true),
            Tok::Ident("false") => Literal::Bool(false),
            Tok::Ident("null") => Literal::Null,
            _ => return self.fail("expected a literal"),
        };
        self.bump();
        Ok(lit)
    }

    fn value(&mut self) -> PResult<Value<'a>> {
        self.enter()?;
        let t = self.tok();
        let v = match t.tok {
            Tok::Ref(hex) => {
                self.bump();
                let (id, mac) = hex.split_once('.').ok_or(err(
                    t.at as usize,
                    t.end as usize,
                    "reference requires MAC",
                ))?;
                let id = parse_uint(&format!("0x{id}"), t.at, t.end)?;
                if id == 0 || mac.len() != 32 {
                    return self.fail("invalid reference");
                }
                let mut bytes = [0u8; 16];
                for (i, out) in bytes.iter_mut().enumerate() {
                    *out = u8::from_str_radix(&mac[i * 2..i * 2 + 2], 16)
                        .map_err(|_| err(t.at as usize, t.end as usize, "invalid MAC"))?;
                }
                Value::Ref(RefLiteral { id, mac: bytes })
            }
            Tok::LBrack => {
                self.bump();
                if self.peek() == Tok::RBrack {
                    self.bump();
                    Value::Array(Vec::new())
                } else {
                    Value::Array(self.list(Tok::RBrack, |p| p.value())?)
                }
            }
            Tok::LBrace => {
                self.bump();
                let items = self.list(Tok::RBrace, |p| {
                    let k = p.ident("expected object key")?;
                    p.expect(Tok::Colon, "expected `:` after object key")?;
                    Ok((k, p.value()?))
                })?;
                for (i, (k, _)) in items.iter().enumerate() {
                    if items[..i].iter().any(|(k2, _)| k2 == k) {
                        return Err(err(
                            t.at as usize,
                            self.toks[self.pos - 1].end as usize,
                            "duplicate object key",
                        ));
                    }
                }
                Value::Object(items)
            }
            Tok::Ident(name)
                if self.peek2() == Tok::LParen && !matches!(name, "true" | "false" | "null") =>
            {
                return self.fail("unknown function: no expression functions are enabled");
            }
            _ => Value::Lit(self.literal()?),
        };
        self.leave();
        Ok(v)
    }

    // ---------------------------------------------------------------- FILTER

    fn predicate(&mut self) -> PResult<Predicate<'a>> {
        let field = self.field_path()?;
        let op = match self.peek() {
            Tok::EqEq => CompOp::Eq,
            Tok::Ne => CompOp::Ne,
            Tok::Gt => CompOp::Gt,
            Tok::Lt => CompOp::Lt,
            Tok::Ge => CompOp::Ge,
            Tok::Le => CompOp::Le,
            Tok::Ident(k) if k.eq_ignore_ascii_case("IN") => CompOp::In,
            Tok::Ident(k) if k.eq_ignore_ascii_case("CONTAINS") => CompOp::Contains,
            Tok::Assign => return self.fail("`=` is assignment; use `==` in FILTER"),
            _ => return self.fail("expected comparison operator"),
        };
        self.bump();
        let rhs = if op == CompOp::In {
            self.expect(Tok::LBrack, "IN expects a `[...]` list")?;
            Value::Array(self.list(Tok::RBrack, |p| Ok(Value::Lit(p.literal()?)))?)
        } else {
            Value::Lit(self.literal()?)
        };
        Ok(Predicate { field, op, rhs })
    }

    fn bool_atom(&mut self) -> PResult<BoolExpr<'a>> {
        if self.peek() == Tok::LParen {
            self.enter()?;
            self.bump();
            let e = self.bool_or()?;
            self.expect(Tok::RParen, "expected `)`")?;
            self.leave();
            Ok(e)
        } else {
            Ok(BoolExpr::Pred(self.predicate()?))
        }
    }

    fn bool_and(&mut self) -> PResult<BoolExpr<'a>> {
        let mut v = vec![self.bool_atom()?];
        while self.is_kw("AND") {
            self.bump();
            if v.len() == MAX_LIST {
                return self.fail("too many AND terms");
            }
            v.push(self.bool_atom()?);
        }
        Ok(if v.len() == 1 {
            v.pop().unwrap()
        } else {
            BoolExpr::And(v)
        })
    }

    /// `AND` binds tighter than `OR`.
    fn bool_or(&mut self) -> PResult<BoolExpr<'a>> {
        let mut v = vec![self.bool_and()?];
        while self.is_kw("OR") {
            self.bump();
            if v.len() == MAX_LIST {
                return self.fail("too many OR terms");
            }
            v.push(self.bool_and()?);
        }
        Ok(if v.len() == 1 {
            v.pop().unwrap()
        } else {
            BoolExpr::Or(v)
        })
    }

    // ---------------------------------------------------------------- MUTATE

    fn mutation(&mut self) -> PResult<Mutation<'a>> {
        let mut target = self.field_path()?;
        if self.peek() == Tok::LParen {
            // Method call: `<path>.append(<value>)`.
            let method = target.path.pop();
            if method != Some("append") {
                return self.fail("only `.append(..)` is callable in MUTATE");
            }
            self.bump();
            let value = self.value()?;
            self.expect(Tok::RParen, "append takes exactly one argument")?;
            return Ok(Mutation::Append { target, value });
        }
        let op = match self.peek() {
            Tok::Assign => AssignOp::Set,
            Tok::AddAssign => AssignOp::Add,
            Tok::SubAssign => AssignOp::Sub,
            Tok::EqEq => return self.fail("`==` is comparison; use `=` in MUTATE"),
            _ => return self.fail("expected `=`, `+=` or `-=`"),
        };
        self.bump();
        let value = self.value()?;
        if matches!(value, Value::Array(_) | Value::Object(_)) && op != AssignOp::Set {
            return self.fail("`+=` / `-=` need a scalar right-hand side");
        }
        Ok(Mutation::Assign { target, op, value })
    }

    // ---------------------------------------------------------------- stages

    fn string(&mut self) -> PResult<&'a str> {
        match self.peek() {
            Tok::Str(s) => {
                self.bump();
                unescape(s).map_err(|_| err(0, 0, "invalid escape"))?;
                Ok(s)
            }
            _ => self.fail("expected string literal"),
        }
    }
    fn stage(&mut self) -> PResult<Stage<'a>> {
        if self.peek() == Tok::LBrace {
            self.bump();
            return Ok(Stage::Mutate(self.list(Tok::RBrace, |p| p.mutation())?));
        }
        let kw_tok = self.tok();
        let Tok::Ident(kw) = kw_tok.tok else {
            return self.fail("expected stage keyword");
        };
        self.bump();
        match kw.to_ascii_uppercase().as_str() {
            "FILTER" | "WHERE" => {
                let e = self.bool_or()?;
                Ok(Stage::Filter(e))
            }
            "SIMILARITY" => {
                self.expect(Tok::LParen, "expected `(` after SIMILARITY")?;
                self.expect_kw("vector", "expected `vector:`")?;
                self.expect(Tok::Colon, "expected `:`")?;
                let vt = self.expect(Tok::LBrack, "expected `[` vector literal")?;
                let vector =
                    self.list_bounded(Tok::RBrack, MAX_VECTOR_DIMS, |p| match p.literal()? {
                        Literal::Float(f) => Ok(f as f32),
                        Literal::Int(i) => Ok(i as f32),
                        _ => p.fail("vector elements must be numbers"),
                    })?;
                if vector.len() > MAX_VECTOR_DIMS || vector.iter().any(|x| !x.is_finite()) {
                    return Err(err(
                        vt.at as usize,
                        self.toks[self.pos - 1].end as usize,
                        "vector too long or not finite",
                    ));
                }
                self.expect(Tok::Comma, "expected `,`")?;
                self.expect_kw("metric", "expected `metric:`")?;
                self.expect(Tok::Colon, "expected `:`")?;
                let metric = match self
                    .ident("expected COSINE, L2 or DOT")?
                    .to_ascii_uppercase()
                    .as_str()
                {
                    "COSINE" => Metric::Cosine,
                    "L2" => Metric::L2,
                    "DOT" => Metric::Dot,
                    _ => {
                        return Err(err(
                            self.toks[self.pos - 1].at as usize,
                            self.toks[self.pos - 1].end as usize,
                            "expected COSINE, L2 or DOT",
                        ));
                    }
                };
                self.expect(Tok::Comma, "expected `,`")?;
                self.expect_kw("threshold", "expected `threshold:`")?;
                self.expect(Tok::Colon, "expected `:`")?;
                let tt = self.tok();
                let threshold = match self.literal()? {
                    Literal::Float(f) => f as f32,
                    Literal::Int(i) => i as f32,
                    _ => {
                        return Err(err(
                            tt.at as usize,
                            tt.end as usize,
                            "threshold must be a number",
                        ));
                    }
                };
                if !threshold.is_finite() {
                    return Err(err(
                        tt.at as usize,
                        tt.end as usize,
                        "threshold is not finite",
                    ));
                }
                if metric == Metric::Cosine && !(-1.0..=1.0).contains(&threshold) {
                    return Err(err(
                        tt.at as usize,
                        tt.end as usize,
                        "COSINE threshold must be in [-1, 1]",
                    ));
                }
                self.expect(Tok::RParen, "expected `)` closing SIMILARITY")?;
                Ok(Stage::Similarity {
                    vector,
                    metric,
                    threshold,
                })
            }
            "MUTATE" => {
                self.expect(Tok::LBrace, "expected `{` after MUTATE")?;
                Ok(Stage::Mutate(self.list(Tok::RBrace, |p| p.mutation())?))
            }
            "PROJECT" | "SELECT" => {
                self.expect(Tok::LBrace, "expected `{` after PROJECT")?;
                let items = self.list(Tok::RBrace, |p| {
                    let alias = p.ident("expected projection alias")?;
                    p.expect(Tok::Colon, "expected `:` after alias")?;
                    Ok(ProjectItem {
                        alias,
                        field: p.field_path()?,
                    })
                })?;
                for (i, it) in items.iter().enumerate() {
                    if items[..i].iter().any(|o| o.alias == it.alias) {
                        return Err(err(
                            kw_tok.at as usize,
                            self.toks[self.pos - 1].end as usize,
                            "duplicate PROJECT alias",
                        ));
                    }
                }
                Ok(Stage::Project(items))
            }
            "GROUP_BY" | "GROUP" => {
                if kw.eq_ignore_ascii_case("GROUP") {
                    self.expect_kw("BY", "expected BY")?;
                }
                self.expect(Tok::LParen, "expected `(` after GROUP_BY")?;
                Ok(Stage::GroupBy(self.list(Tok::RParen, |p| p.field_path())?))
            }
            "REDUCE" | "AGGREGATE" => {
                self.expect(Tok::LBrace, "expected `{`")?;
                let items = self.list(Tok::RBrace, |p| {
                    let alias = p.ident("expected aggregate alias")?;
                    p.expect(Tok::Colon, "expected `:` after alias")?;
                    let ft = p.tok();
                    let func = match p
                        .ident("expected aggregate function")?
                        .to_ascii_uppercase()
                        .as_str()
                    {
                        "COUNT" => AggFunc::Count,
                        "SUM" => AggFunc::Sum,
                        "AVG" => AggFunc::Avg,
                        "MIN" => AggFunc::Min,
                        "MAX" => AggFunc::Max,
                        "COUNT_DISTINCT" => AggFunc::CountDistinct,
                        "P50" => AggFunc::P50,
                        "P95" => AggFunc::P95,
                        "P99" => AggFunc::P99,
                        _ => {
                            return Err(err(
                                ft.at as usize,
                                ft.end as usize,
                                "unknown aggregate function",
                            ));
                        }
                    };
                    p.expect(Tok::LParen, "expected `(`")?;
                    let arg = if p.peek() == Tok::RParen {
                        None
                    } else {
                        Some(p.field_path()?)
                    };
                    p.expect(Tok::RParen, "expected `)`")?;
                    if arg.is_none() && func != AggFunc::Count {
                        return Err(err(
                            ft.at as usize,
                            ft.end as usize,
                            "only COUNT() may omit its argument",
                        ));
                    }
                    Ok(AggItem { alias, func, arg })
                })?;
                Ok(Stage::Reduce(items))
            }
            "CHECKPOINT_FRAME" | "CHECKPOINT" => {
                self.expect(Tok::LParen, "expected `(`")?;
                self.expect_kw("intent", "expected `intent:`")?;
                self.expect(Tok::Colon, "expected `:`")?;
                let intent = match self.peek() {
                    Tok::Str(s) => {
                        self.bump();
                        s
                    }
                    _ => return self.fail("intent must be a string"),
                };
                let mut constraints = Vec::new();
                if self.peek() == Tok::Comma {
                    self.bump();
                    self.expect_kw("constraints", "expected `constraints:`")?;
                    self.expect(Tok::Colon, "expected `:`")?;
                    self.expect(Tok::LBrack, "expected `[`")?;
                    constraints = self.list(Tok::RBrack, |p| match p.peek() {
                        Tok::Str(s) => {
                            p.bump();
                            Ok(s)
                        }
                        _ => p.fail("constraints must be strings"),
                    })?;
                }
                self.expect(Tok::RParen, "expected `)`")?;
                Ok(Stage::CheckpointFrame {
                    intent,
                    constraints,
                })
            }
            "HYDRATE_FRAME" => {
                self.expect(Tok::LParen, "expected `(`")?;
                let frame_hash = self.uint()?;
                self.expect(Tok::RParen, "expected `)`")?;
                Ok(Stage::HydrateFrame { frame_hash })
            }
            "TAKE" | "LIMIT" => {
                let t = self.tok();
                let n = self.uint()?;
                if n == 0 {
                    return Err(err(t.at as usize, t.end as usize, "TAKE must be >= 1"));
                }
                Ok(Stage::Take(n))
            }
            "SEARCH" => {
                self.expect(Tok::LParen, "expected (`")?;
                let (mut field, mut query, mut top_k) = (None, None, None);
                loop {
                    let key = self.ident("expected search argument")?;
                    self.expect(Tok::Colon, "expected :")?;
                    match key {
                        "field" if field.is_none() => {
                            let mut parts = vec![self.ident("expected field")?];
                            while self.peek() == Tok::Dot {
                                self.bump();
                                parts.push(self.ident("expected field segment")?);
                            }
                            field = Some(parts);
                        }
                        "query" if query.is_none() => query = Some(self.string()?),
                        "top_k" if top_k.is_none() => {
                            let n = self.uint()?;
                            if !(1..=1000).contains(&n) {
                                return self.fail("top_k must be in 1..=1000");
                            }
                            top_k = Some(n)
                        }
                        _ => return self.fail("unknown or duplicate search argument"),
                    }
                    if self.peek() != Tok::Comma {
                        break;
                    }
                    self.bump();
                }
                self.expect(Tok::RParen, "expected )")?;
                Ok(Stage::SearchBm25 {
                    field: field.ok_or(err(
                        kw_tok.at as usize,
                        kw_tok.end as usize,
                        "missing field",
                    ))?,
                    query: query.ok_or(err(
                        kw_tok.at as usize,
                        kw_tok.end as usize,
                        "missing query",
                    ))?,
                    top_k: top_k.unwrap_or(10),
                })
            }
            "EXPORT" | "EXPORT_TO" => {
                if self.is_kw("TO") {
                    self.bump();
                }
                self.expect(Tok::LParen, "expected (")?;
                let (mut format, mut destination) = (None, None);
                loop {
                    let key = self.ident("expected export argument")?;
                    self.expect(Tok::Colon, "expected :")?;
                    match key {
                        "format" if format.is_none() => format = Some(self.string()?),
                        "destination" if destination.is_none() => {
                            destination = Some(self.string()?)
                        }
                        _ => return self.fail("unknown or duplicate export argument"),
                    }
                    if self.peek() != Tok::Comma {
                        break;
                    }
                    self.bump();
                }
                self.expect(Tok::RParen, "expected )")?;
                Ok(Stage::ExportTo {
                    format: format.ok_or(err(0, 0, "missing format"))?,
                    destination: destination.ok_or(err(0, 0, "missing destination"))?,
                })
            }
            "JOIN" => {
                if self.peek() == Tok::At {
                    self.bump();
                }
                self.expect_kw("actor", "expected actor(field)")?;
                self.expect(Tok::LParen, "expected (")?;
                let target = self.field_path()?;
                self.expect(Tok::RParen, "expected )")?;
                self.expect_kw("AS", "expected AS")?;
                if self.peek() == Tok::At {
                    self.bump();
                }
                let alias = self.ident("expected alias")?;
                Ok(Stage::JoinActor { target, alias })
            }
            "LINK" => {
                self.expect_kw("ADD", "expected ADD")?;
                self.expect(Tok::LParen, "expected (")?;
                self.expect_kw("target", "expected target")?;
                self.expect(Tok::Colon, "expected :")?;
                if self.peek() == Tok::At {
                    self.bump();
                }
                let target = self.uint()?;
                self.expect(Tok::Comma, "expected ,")?;
                self.expect_kw("on_delete", "expected on_delete")?;
                self.expect(Tok::Colon, "expected :")?;
                let cascade = match self
                    .ident("expected RESTRICT or CASCADE")?
                    .to_ascii_uppercase()
                    .as_str()
                {
                    "CASCADE" => true,
                    "RESTRICT" => false,
                    _ => return self.fail("expected RESTRICT or CASCADE"),
                };
                self.expect(Tok::RParen, "expected )")?;
                Ok(Stage::LinkAdd { target, cascade })
            }
            // The fix for the spec's silent drop: anything else is an error.
            _ => Err(err(
                kw_tok.at as usize,
                kw_tok.end as usize,
                "unknown stage keyword",
            )),
        }
    }
}

/// Parse and validate. This is the only entry point; there is no "lenient" mode.
pub fn parse(src: &str) -> PResult<Query<'_>> {
    let toks = lex(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let source = p.source()?;
    let mut stages = Vec::new();
    let mut stage_at = Vec::new();
    while p.peek() != Tok::Eof {
        if p.peek() == Tok::Pipe {
            p.bump();
        }
        if stages.len() == MAX_STAGES {
            return p.fail("too many stages");
        }
        stage_at.push((p.tok().at, p.tok().end));
        stages.push(p.stage()?);
    }
    if p.peek() != Tok::Eof {
        return p.fail("expected `|>` or end of query");
    }
    let q = Query { source, stages };
    preflight(&q)?;
    validate(&q, &stage_at)?;
    Ok(q)
}

// =================================================================================
// Semantic validation
// =================================================================================

fn visit_paths<'q, 'a>(
    s: &'q Stage<'a>,
    f: &mut dyn FnMut(&'q FieldPath<'a>) -> PResult<()>,
) -> PResult<()> {
    fn bool_paths<'q, 'a>(
        e: &'q BoolExpr<'a>,
        f: &mut dyn FnMut(&'q FieldPath<'a>) -> PResult<()>,
    ) -> PResult<()> {
        match e {
            BoolExpr::Pred(p) => f(&p.field),
            BoolExpr::And(v) | BoolExpr::Or(v) => v.iter().try_for_each(|e| bool_paths(e, f)),
        }
    }
    match s {
        Stage::JoinActor { target, .. } => f(target),
        Stage::Filter(e) => bool_paths(e, f),
        Stage::Mutate(ms) => ms.iter().try_for_each(|m| match m {
            Mutation::Assign { target, .. } | Mutation::Append { target, .. } => f(target),
        }),
        Stage::Project(items) => items.iter().try_for_each(|i| f(&i.field)),
        Stage::GroupBy(ps) => ps.iter().try_for_each(&mut *f),
        Stage::Reduce(items) => items.iter().filter_map(|i| i.arg.as_ref()).try_for_each(f),
        _ => Ok(()),
    }
}

fn validate(q: &Query<'_>, at: &[(u32, u32)]) -> PResult<()> {
    let e = |i: usize, msg| {
        Err(ParseError {
            at: at[i].0,
            end: at[i].1,
            msg,
        })
    };
    let tx = match &q.source {
        Source::TxActors(ids) => Some(ids.as_slice()),
        _ => None,
    };
    let scan = matches!(q.source, Source::ScanRange { .. } | Source::ScanAll);
    let mut mutates = 0;
    let mut aliases = Vec::new();
    for (i, s) in q.stages.iter().enumerate() {
        match s {
            Stage::Take(_)
                if i + 1 != q.stages.len()
                    && !(i + 2 == q.stages.len()
                        && matches!(q.stages.last(), Some(Stage::ExportTo { .. }))) =>
            {
                return e(i, "TAKE must be the last stage");
            }
            Stage::JoinActor { alias, .. } if alias.is_empty() || aliases.contains(alias) => {
                return e(i, "duplicate or empty join alias");
            }
            Stage::LinkAdd { target, .. }
                if *target == 0 || !matches!(q.source, Source::Actor(_)) =>
            {
                return e(i, "LINK requires a single actor and nonzero target");
            }
            Stage::Mutate(ms) => {
                mutates += 1;
                if mutates > 1 {
                    return e(i, "at most one MUTATE per query (one transaction)");
                }
                if scan {
                    return e(i, "MUTATE is not allowed on SCAN sources (use TX_ACTORS)");
                }
                for m in ms {
                    match m {
                        Mutation::Assign { target, op, value } => {
                            if target.root != Root::State
                                || target.path.is_empty()
                                || target.alias.is_some()
                            {
                                return Err(ParseError {
                                    at: target.at,
                                    end: target.at + 1,
                                    msg: "only state.<field> can be assigned",
                                });
                            }
                            if *op != AssignOp::Set
                                && !matches!(
                                    value,
                                    Value::Lit(
                                        Literal::Int(_) | Literal::UInt(_) | Literal::Float(_)
                                    )
                                )
                            {
                                return Err(ParseError {
                                    at: target.at,
                                    end: target.at + 1,
                                    msg: "`+=` / `-=` need a numeric literal",
                                });
                            }
                        }
                        Mutation::Append { target, value } => {
                            if !matches!(target.root, Root::Episodes | Root::Semantic)
                                || !target.path.is_empty()
                            {
                                return Err(ParseError {
                                    at: target.at,
                                    end: target.at + 1,
                                    msg: "append is only valid on `episodes` or `semantic`",
                                });
                            }
                            if !matches!(value, Value::Object(_)) {
                                return Err(ParseError {
                                    at: target.at,
                                    end: target.at + 1,
                                    msg: "append expects an object literal",
                                });
                            }
                        }
                    }
                }
            }
            Stage::GroupBy(_) if !matches!(q.stages.get(i + 1), Some(Stage::Reduce(_))) => {
                return e(i, "GROUP_BY must be followed by AGGREGATE/REDUCE");
            }
            Stage::CheckpointFrame { .. } | Stage::HydrateFrame { .. }
                if !matches!(q.source, Source::Actor(_)) =>
            {
                return e(i, "frame stages require a single ACTOR source");
            }
            Stage::Similarity { .. } if tx.is_some() => {
                return e(i, "SIMILARITY is not supported on TX_ACTORS");
            }
            Stage::ExportTo { .. } if i + 1 != q.stages.len() => {
                return e(i, "EXPORT must be the last stage");
            }
            _ => {}
        }
        visit_paths(s, &mut |p| {
            if let Some(alias) = p.alias {
                if !aliases.contains(&alias) || tx.is_some() {
                    return Err(ParseError {
                        at: p.at,
                        end: p.at + 1,
                        msg: "unknown join alias",
                    });
                }
            }
            match (tx, p.actor) {
                (Some(ids), Some(a)) if ids.contains(&a) => Ok(()),
                (Some(_), Some(_)) => Err(ParseError {
                    at: p.at,
                    end: p.at + 1,
                    msg: "actor(..) is not a participant of TX_ACTORS",
                }),
                (Some(_), None) if p.root != Root::Meta => Err(ParseError {
                    at: p.at,
                    end: p.at + 1,
                    msg: "field paths in TX_ACTORS queries must be qualified with actor(..)",
                }),
                (None, Some(_)) => Err(ParseError {
                    at: p.at,
                    end: p.at + 1,
                    msg: "actor(..) qualifier is only valid with TX_ACTORS",
                }),
                _ => Ok(()),
            }
        })?;
        if let Stage::JoinActor { alias, .. } = s {
            aliases.push(*alias);
        }
    }
    Ok(())
}

/// The four queries from spec section 7.2, verbatim.
pub mod spec_examples {
    pub const EXAMPLE_1: &str = r#"ACTOR(0x0008F421)
|> FILTER (state.account.status == "ACTIVE" AND state.wallet.credits >= 50)
|> MUTATE {
     state.wallet.credits -= 50,
     state.metrics.api_calls += 1,
     episodes.append({ tool: "k8s_deploy", output_ref: $ref:0x99A4F012.abababababababababababababababab, status: 200 })
   }
|> CHECKPOINT_FRAME(intent: "Verify pod readiness in namespace prod-01")
|> PROJECT {
     remaining_credits: state.wallet.credits,
     tx_epoch: meta.last_tx_seq,
     handoff_frame: frame.hash
   }"#;

    pub const EXAMPLE_2: &str = r#"TX_ACTORS[0x0008F421, 0x0009B104]
|> FILTER (
     actor(0x0008F421).state.wallet.balance >= 1200.00 AND
     actor(0x0009B104).state.account.kyc_verified == true
   )
|> MUTATE {
     actor(0x0008F421).state.wallet.balance -= 1200.00,
     actor(0x0009B104).state.wallet.balance += 1200.00
   }
|> PROJECT {
     sender_balance: actor(0x0008F421).state.wallet.balance,
     receiver_balance: actor(0x0009B104).state.wallet.balance,
     global_tx_seq: meta.last_tx_seq
   }"#;

    pub const EXAMPLE_3: &str = r#"SCAN ACTORS[1..1_000_000]
|> FILTER (state.subscription.plan == "ENTERPRISE" AND state.ai.tokens_saved > 50000)
|> GROUP_BY (state.profile.region, state.subscription.billing_cycle)
|> AGGREGATE {
     tenant_count: COUNT(),
     unique_companies: COUNT_DISTINCT(state.profile.company_id),
     total_arr: SUM(state.subscription.annual_value),
     avg_tokens_saved: AVG(state.ai.tokens_saved),
     p99_agent_latency_ns: P99(state.metrics.agent_step_ns)
   }
|> TAKE 25"#;

    pub const EXAMPLE_4: &str = r#"ACTOR(0x0008F421)
|> SIMILARITY(vector: [0.118, -0.742, 0.509, 0.214], metric: COSINE, threshold: 0.85)
|> FILTER (episodes.exit_code == 0 AND episodes.timestamp >= 1761800000)
|> PROJECT {
     step_id: episodes.step_id,
     fact_triple: semantic.distilled_fact,
     raw_payload_ptr: episodes.output_ref
   }
|> TAKE 5"#;
}

fn preflight(q: &Query<'_>) -> PResult<()> {
    enum Node<'q, 'a> {
        Expr(&'q BoolExpr<'a>, usize),
        Val(&'q Value<'a>, usize),
    }
    let mut stack = Vec::new();
    let mut nodes = 0usize;
    if q.stages.len() > MAX_STAGES {
        return Err(err(0, 0, "too many stages"));
    }
    for stage in &q.stages {
        match stage {
            Stage::Filter(e) => stack.push(Node::Expr(e, 1)),
            Stage::Mutate(ms) => {
                if ms.len() > MAX_LIST {
                    return Err(err(0, 0, "too many mutations"));
                }
                for m in ms {
                    match m {
                        Mutation::Assign { value, .. } | Mutation::Append { value, .. } => {
                            stack.push(Node::Val(value, 1))
                        }
                    }
                }
            }
            Stage::Project(v) if v.len() > MAX_LIST => {
                return Err(err(0, 0, "too many projections"));
            }
            Stage::GroupBy(v) if v.len() > MAX_LIST => {
                return Err(err(0, 0, "too many group keys"));
            }
            Stage::Reduce(v) if v.len() > MAX_LIST => return Err(err(0, 0, "too many aggregates")),
            _ => {}
        }
    }
    while let Some(n) = stack.pop() {
        nodes += 1;
        if nodes > MAX_TOKENS {
            return Err(err(0, 0, "AST node budget exceeded"));
        }
        match n {
            Node::Expr(e, d) => {
                if d > MAX_DEPTH {
                    return Err(err(0, 0, "AST nesting too deep"));
                }
                match e {
                    BoolExpr::Pred(p) => {
                        if p.field.path.len() > MAX_DEPTH {
                            return Err(err(0, 0, "field path too deep"));
                        }
                        stack.push(Node::Val(&p.rhs, d));
                    }
                    BoolExpr::And(es) | BoolExpr::Or(es) => {
                        if es.is_empty() || es.len() > MAX_LIST {
                            return Err(err(0, 0, "invalid boolean arity"));
                        }
                        for e in es {
                            stack.push(Node::Expr(e, d + 1));
                        }
                    }
                }
            }
            Node::Val(v, d) => {
                if d > MAX_DEPTH {
                    return Err(err(0, 0, "value nesting too deep"));
                }
                match v {
                    Value::Call { .. } => return Err(err(0, 0, "unknown function")),
                    Value::Array(vs) => {
                        if vs.len() > MAX_LIST {
                            return Err(err(0, 0, "array too long"));
                        }
                        for v in vs {
                            stack.push(Node::Val(v, d + 1));
                        }
                    }
                    Value::Object(vs) => {
                        if vs.len() > MAX_LIST {
                            return Err(err(0, 0, "object too long"));
                        }
                        for (_, v) in vs {
                            stack.push(Node::Val(v, d + 1));
                        }
                    }
                    Value::Ref(r) if r.id == 0 => return Err(err(0, 0, "invalid reference")),
                    Value::Lit(Literal::Float(f)) if !f.is_finite() => {
                        return Err(err(0, 0, "non-finite value"));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

// Query owns recursive public nodes. Tear them down iteratively, even if validation
// rejected a malicious programmatically-built AST before a bounded recursive walk.
impl Drop for Query<'_> {
    fn drop(&mut self) {
        enum Node<'a> {
            Expr(BoolExpr<'a>),
            Val(Value<'a>),
        }
        let mut stack = Vec::new();
        for stage in self.stages.drain(..) {
            match stage {
                Stage::Filter(e) => stack.push(Node::Expr(e)),
                Stage::Mutate(ms) => {
                    for m in ms {
                        match m {
                            Mutation::Assign { value, .. } | Mutation::Append { value, .. } => {
                                stack.push(Node::Val(value))
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        while let Some(n) = stack.pop() {
            match n {
                Node::Expr(BoolExpr::Pred(p)) => stack.push(Node::Val(p.rhs)),
                Node::Expr(BoolExpr::And(es) | BoolExpr::Or(es)) => {
                    stack.extend(es.into_iter().map(Node::Expr))
                }
                Node::Val(Value::Array(vs) | Value::Call { args: vs, .. }) => {
                    stack.extend(vs.into_iter().map(Node::Val))
                }
                Node::Val(Value::Object(vs)) => {
                    stack.extend(vs.into_iter().map(|(_, v)| Node::Val(v)))
                }
                _ => {}
            }
        }
    }
}

/// Recheck typed ASTs supplied by callers, including participant and stage bounds.
pub fn validate_query_ast(q: &Query<'_>) -> PResult<()> {
    preflight(q)?;
    if q.stages.len() > MAX_STAGES {
        return Err(err(0, 0, "too many stages"));
    }
    if let Source::TxActors(ids) = &q.source {
        if ids.len() < 2
            || ids.len() > MAX_TX_ACTORS
            || ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id))
        {
            return Err(err(0, 0, "invalid transaction participants"));
        }
    }
    for stage in &q.stages {
        match stage {
            Stage::SearchBm25 { top_k, .. } if !(1..=1000).contains(top_k) => {
                return Err(err(0, 0, "top_k must be in 1..=1000"));
            }
            Stage::Take(0) => return Err(err(0, 0, "TAKE must be >= 1")),
            Stage::Similarity {
                vector, threshold, ..
            } if vector.is_empty()
                || vector.len() > MAX_VECTOR_DIMS
                || !threshold.is_finite()
                || vector.iter().any(|x| !x.is_finite()) =>
            {
                return Err(err(0, 0, "invalid similarity vector or threshold"));
            }
            _ => {}
        }
    }
    validate(q, &vec![(0, 0); q.stages.len()])
}

/// Actor references are typed nodes, never searched for inside raw query text.
pub fn referenced_actors(q: &Query<'_>) -> PResult<Vec<u64>> {
    preflight(q)?;
    let mut ids = Vec::new();
    for s in &q.stages {
        if let Stage::LinkAdd { target, .. } = s {
            ids.push(*target);
        }
        let _ = visit_paths(s, &mut |path| {
            if let Some(id) = path.actor {
                ids.push(id)
            }
            Ok(())
        });
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::spec_examples::*;
    use super::*;

    fn fp<'a>(root: Root, path: &[&'a str]) -> (Root, Vec<&'a str>) {
        (root, path.to_vec())
    }

    #[test]
    fn example_1_keeps_mutate_and_project() {
        let q = parse(EXAMPLE_1).unwrap();
        assert_eq!(q.source, Source::Actor(0x0008_F421));
        let names: Vec<_> = q.stages.iter().map(Stage::name).collect();
        assert_eq!(names, ["FILTER", "MUTATE", "CHECKPOINT_FRAME", "PROJECT"]);
        let Stage::Mutate(ms) = &q.stages[1] else {
            panic!()
        };
        assert_eq!(ms.len(), 3);
        match &ms[0] {
            Mutation::Assign { target, op, value } => {
                assert_eq!(
                    (target.root, target.path.clone()),
                    fp(Root::State, &["wallet", "credits"])
                );
                assert_eq!(*op, AssignOp::Sub);
                assert_eq!(*value, Value::Lit(Literal::Int(50)));
            }
            m => panic!("{m:?}"),
        }
        match &ms[2] {
            Mutation::Append {
                target,
                value: Value::Object(kv),
            } => {
                assert_eq!(target.root, Root::Episodes);
                assert_eq!(
                    kv[1],
                    (
                        "output_ref",
                        Value::Ref(RefLiteral {
                            id: 0x99A4_F012,
                            mac: [0xab; 16]
                        })
                    )
                );
                assert_eq!(kv[2], ("status", Value::Lit(Literal::Int(200))));
            }
            m => panic!("{m:?}"),
        }
        let Stage::Project(items) = &q.stages[3] else {
            panic!()
        };
        assert_eq!(
            items.iter().map(|i| i.alias).collect::<Vec<_>>(),
            ["remaining_credits", "tx_epoch", "handoff_frame"]
        );
        assert_eq!(
            (items[2].field.root, items[2].field.path.clone()),
            fp(Root::Frame, &["hash"])
        );
    }

    #[test]
    fn example_2_tx_actors_with_qualified_paths() {
        let q = parse(EXAMPLE_2).unwrap();
        assert_eq!(q.source, Source::TxActors(vec![0x0008_F421, 0x0009_B104]));
        let Stage::Mutate(ms) = &q.stages[1] else {
            panic!()
        };
        let Mutation::Assign {
            target,
            op: AssignOp::Add,
            value: Value::Lit(Literal::Float(v)),
        } = &ms[1]
        else {
            panic!()
        };
        assert_eq!(target.actor, Some(0x0009_B104));
        assert_eq!(*v, 1200.0);
        let Stage::Filter(BoolExpr::And(terms)) = &q.stages[0] else {
            panic!()
        };
        assert_eq!(terms.len(), 2);
    }

    #[test]
    fn example_3_group_by_and_aggregate() {
        let q = parse(EXAMPLE_3).unwrap();
        assert_eq!(
            q.source,
            Source::ScanRange {
                start: 1,
                end: 1_000_000
            }
        );
        let Stage::GroupBy(g) = &q.stages[1] else {
            panic!()
        };
        assert_eq!(g.len(), 2);
        assert_eq!(
            (g[1].root, g[1].path.clone()),
            fp(Root::State, &["subscription", "billing_cycle"])
        );
        let Stage::Reduce(a) = &q.stages[2] else {
            panic!()
        };
        assert_eq!(
            a.iter().map(|x| x.func).collect::<Vec<_>>(),
            [
                AggFunc::Count,
                AggFunc::CountDistinct,
                AggFunc::Sum,
                AggFunc::Avg,
                AggFunc::P99
            ]
        );
        assert!(a[0].arg.is_none());
        assert_eq!(q.stages[3], Stage::Take(25));
    }

    #[test]
    fn example_4_similarity() {
        let q = parse(EXAMPLE_4).unwrap();
        let Stage::Similarity {
            vector,
            metric,
            threshold,
        } = &q.stages[0]
        else {
            panic!()
        };
        assert_eq!(vector, &[0.118f32, -0.742, 0.509, 0.214]);
        assert_eq!((*metric, *threshold), (Metric::Cosine, 0.85));
    }

    #[test]
    fn literal_and_identifier_safety() {
        // `BRAND` contains "AND"; the string contains `|>` and ` AND `.
        let q = parse(r#"ACTOR(1) |> FILTER (state.BRAND == "a |> b AND c" OR state.x IN [1, 2, 3]) |> TAKE 1"#).unwrap();
        let Stage::Filter(BoolExpr::Or(v)) = &q.stages[0] else {
            panic!("{:?}", q.stages[0])
        };
        let BoolExpr::Pred(p) = &v[0] else { panic!() };
        assert_eq!(p.field.path, ["BRAND"]);
        assert_eq!(p.rhs, Value::Lit(Literal::Str("a |> b AND c")));
        assert_eq!(q.stages.len(), 2);
    }

    #[test]
    fn precedence_and_parentheses() {
        let q =
            parse("ACTOR(1) |> FILTER (state.a == 1 OR state.b == 2 AND state.c == 3)").unwrap();
        let Stage::Filter(BoolExpr::Or(v)) = &q.stages[0] else {
            panic!()
        };
        assert!(matches!(&v[1], BoolExpr::And(x) if x.len() == 2));
        let q =
            parse("ACTOR(1) |> FILTER ((state.a == 1 OR state.b == 2) AND state.c == 3)").unwrap();
        assert!(
            matches!(&q.stages[0], Stage::Filter(BoolExpr::And(x)) if matches!(x[0], BoolExpr::Or(_)))
        );
    }

    #[test]
    fn rejections() {
        let cases: &[(&str, &str)] = &[
            ("ACTOR(1) |> FROBNICATE {}", "unknown stage keyword"),
            (
                "ACTOR(1) |> TAKE 5 |> PROJECT { a: state.x }",
                "TAKE must be the last stage",
            ),
            (
                "SCAN ACTORS[*] |> MUTATE { state.x = 1 }",
                "MUTATE is not allowed on SCAN sources (use TX_ACTORS)",
            ),
            (
                "SCAN ACTORS[*] |> GROUP_BY (state.r) |> TAKE 3",
                "GROUP_BY must be followed by AGGREGATE/REDUCE",
            ),
            (
                "ACTOR(1) |> MUTATE { meta.last_tx_seq = 1 }",
                "only state.<field> can be assigned",
            ),
            (
                "ACTOR(1) |> MUTATE { state.x += \"a\" }",
                "`+=` / `-=` need a numeric literal",
            ),
            (
                "ACTOR(1) |> MUTATE { state.x == 1 }",
                "`==` is comparison; use `=` in MUTATE",
            ),
            (
                "ACTOR(1) |> FILTER (state.x = 1)",
                "`=` is assignment; use `==` in FILTER",
            ),
            (
                "TX_ACTORS[1, 2] |> MUTATE { actor(3).state.x = 1 }",
                "actor(..) is not a participant of TX_ACTORS",
            ),
            (
                "TX_ACTORS[1, 2] |> MUTATE { state.x = 1 }",
                "field paths in TX_ACTORS queries must be qualified with actor(..)",
            ),
            ("TX_ACTORS[1, 1]", "duplicate actor in TX_ACTORS"),
            (
                "ACTOR(1) |> PROJECT { a: actor(1).state.x }",
                "actor(..) qualifier is only valid with TX_ACTORS",
            ),
            ("SCAN ACTORS[9..1]", "scan range start > end"),
            (
                "ACTOR(1) |> PROJECT { a: state.x, a: state.y }",
                "duplicate PROJECT alias",
            ),
            (
                "ACTOR(1) |> PROJECT { a: profile.x }",
                "field path must start with state, meta, frame, episodes or semantic",
            ),
            (
                "ACTOR(1) |> AGGREGATE { s: SUM() }",
                "only COUNT() may omit its argument",
            ),
            (
                "ACTOR(1) |> MUTATE { state.x = 1 } |> MUTATE { state.y = 2 }",
                "at most one MUTATE per query (one transaction)",
            ),
            (
                "ACTOR(1) |> PROJECT { a: state.x",
                "expected `,` or closing delimiter",
            ),
            ("ACTOR(0x1_0000_0000_0000_0000)", "integer out of range"),
            (
                "ACTOR(1) |> FILTER (state.s == \"open)",
                "unterminated string literal",
            ),
            (
                "ACTOR(1) |> SIMILARITY(vector: [1.0], metric: COSINE, threshold: 2.0)",
                "COSINE threshold must be in [-1, 1]",
            ),
            ("ACTOR(1) extra", "unknown stage keyword"),
        ];
        for (src, msg) in cases {
            let e = parse(src).expect_err(src);
            assert_eq!(e.msg, *msg, "query: {src}");
        }
        let e = parse("ACTOR(1)\n|> FROBNICATE {}").unwrap_err();
        assert_eq!(e.line_col("ACTOR(1)\n|> FROBNICATE {}"), (2, 4));
    }

    #[test]
    fn limits_are_enforced() {
        let deep = format!(
            "ACTOR(1) |> MUTATE {{ episodes.append({}1{}) }}",
            "[".repeat(40),
            "]".repeat(40)
        );
        assert_eq!(parse(&deep).unwrap_err().msg, "nesting too deep");
        let many = format!("ACTOR(1){}", " |> TAKE 1".repeat(MAX_STAGES + 1));
        assert!(parse(&many).is_err());
        assert!(unescape(r#"a\"b"#).unwrap() == "a\"b");
    }
}
