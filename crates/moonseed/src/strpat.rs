//! Lua 5.4.9's pattern matcher (`lstrlib.c`) as resumable state machines.
//!
//! C's `match` recurses; here every recursive call is a frame on an explicit
//! stack of at most [`MAX_DEPTH`] entries, and every return pops a frame and
//! resumes its continuation. The depth counter, the order of checks and the
//! points where errors are raised are those of the C code, so the same
//! patterns match, fail and raise the same errors at the same moments.
//!
//! Positions are byte offsets (`u32`) into the subject or the pattern. A read
//! at the end of either yields `0`, as C reads the terminating `'\0'`.
//! Character classes follow the C locale: bytes of 128 and above are in no
//! class.
//!
//! Work is charged to a `budget`: one per machine transition, plus
//! `1 + len / 64` for bulk compares. Machines return `Pending` only between
//! transitions, where their whole state can be encoded.

pub(crate) const MAX_CAPTURES: usize = 32;
/// C's `MAXCCALLS`: how many `match` calls may be active at once.
pub(crate) const MAX_DEPTH: u32 = 200;

const L_ESC: u8 = b'%';
const SPECIALS: &[u8] = b"^$*+?.([%-";

/// An error raised by the matcher or by `add_s`. [`PatternError::message`]
/// gives PUC's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PatternError {
    /// `malformed pattern (ends with '%')`
    EndsWithPercent,
    /// `malformed pattern (missing ']')`
    MissingBracket,
    /// `malformed pattern (missing arguments to '%b')`
    MissingBalanceArgs,
    /// `missing '[' after '%f' in pattern`
    MissingFrontierBracket,
    /// `invalid capture index %N`, with N as the message shows it (0 to 9).
    InvalidCaptureIndex(u8),
    /// `invalid pattern capture`
    InvalidPatternCapture,
    /// `unfinished capture`
    UnfinishedCapture,
    /// `too many captures`
    TooManyCaptures,
    /// `pattern too complex`
    TooComplex,
    /// `invalid use of '%' in replacement string`
    InvalidReplacement,
}

impl PatternError {
    /// The message PUC Lua raises (no position prefix: these errors come
    /// from C functions).
    pub(crate) fn message(&self) -> String {
        match self {
            PatternError::EndsWithPercent => "malformed pattern (ends with '%')".into(),
            PatternError::MissingBracket => "malformed pattern (missing ']')".into(),
            PatternError::MissingBalanceArgs => {
                "malformed pattern (missing arguments to '%b')".into()
            }
            PatternError::MissingFrontierBracket => "missing '[' after '%f' in pattern".into(),
            PatternError::InvalidCaptureIndex(n) => format!("invalid capture index %{n}"),
            PatternError::InvalidPatternCapture => "invalid pattern capture".into(),
            PatternError::UnfinishedCapture => "unfinished capture".into(),
            PatternError::TooManyCaptures => "too many captures".into(),
            PatternError::TooComplex => "pattern too complex".into(),
            PatternError::InvalidReplacement => "invalid use of '%' in replacement string".into(),
        }
    }

    fn encode(&self) -> (u64, u64) {
        match *self {
            PatternError::EndsWithPercent => (0, 0),
            PatternError::MissingBracket => (1, 0),
            PatternError::MissingBalanceArgs => (2, 0),
            PatternError::MissingFrontierBracket => (3, 0),
            PatternError::InvalidCaptureIndex(n) => (4, u64::from(n)),
            PatternError::InvalidPatternCapture => (5, 0),
            PatternError::UnfinishedCapture => (6, 0),
            PatternError::TooManyCaptures => (7, 0),
            PatternError::TooComplex => (8, 0),
            PatternError::InvalidReplacement => (9, 0),
        }
    }

    fn decode(code: u64, arg: u64) -> Option<Self> {
        Some(match code {
            0 => PatternError::EndsWithPercent,
            1 => PatternError::MissingBracket,
            2 => PatternError::MissingBalanceArgs,
            3 => PatternError::MissingFrontierBracket,
            4 if arg <= 9 => PatternError::InvalidCaptureIndex(arg as u8),
            5 => PatternError::InvalidPatternCapture,
            6 => PatternError::UnfinishedCapture,
            7 => PatternError::TooManyCaptures,
            8 => PatternError::TooComplex,
            9 => PatternError::InvalidReplacement,
            _ => return None,
        })
    }
}

/// The result of one [`Matcher::run`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Pending,
    /// The match ends at `end` (exclusive).
    Matched {
        end: u32,
    },
    Failed,
    Error(PatternError),
}

/// C's capture length: `CAP_POSITION`, `CAP_UNFINISHED`, or a length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CapLen {
    Position,
    Open,
    Closed(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capture {
    pub(crate) start: u32,
    pub(crate) len: CapLen,
}

/// A capture as Lua returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureValue {
    /// The subject bytes `start..end`.
    Bytes { start: u32, end: u32 },
    /// A position capture, 1-based.
    Position(i64),
}

/// The result of a search: `start` is 0-based and `end` exclusive, so Lua's
/// `find` returns `start + 1, end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchOutcome {
    Pending,
    Found { start: u32, end: u32 },
    NotFound,
    Error(PatternError),
}

fn len32(bytes: &[u8]) -> u32 {
    u32::try_from(bytes.len()).unwrap_or(u32::MAX)
}

/// The byte at `i`, or 0 past the end (C's terminating `'\0'`).
fn at(bytes: &[u8], i: u32) -> u8 {
    bytes.get(i as usize).copied().unwrap_or(0)
}

/// Extra charge for a scan over a bracket class `p..ep`.
fn set_cost(pat: &[u8], p: u32, ep: u32) -> i64 {
    if at(pat, p) == b'[' {
        i64::from(ep.saturating_sub(p) / 64)
    } else {
        0
    }
}

fn bulk_cost(len: u32) -> i64 {
    1 + i64::from(len / 64)
}

// C-locale <ctype.h> for bytes 0..=255.
fn is_alpha(c: u8) -> bool {
    c.is_ascii_alphabetic()
}
fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}
fn is_lower(c: u8) -> bool {
    c.is_ascii_lowercase()
}
fn is_upper(c: u8) -> bool {
    c.is_ascii_uppercase()
}
fn is_alnum(c: u8) -> bool {
    is_alpha(c) || is_digit(c)
}
fn is_cntrl(c: u8) -> bool {
    c < 32 || c == 127
}
fn is_graph(c: u8) -> bool {
    (33..=126).contains(&c)
}
fn is_punct(c: u8) -> bool {
    is_graph(c) && !is_alnum(c)
}
fn is_space(c: u8) -> bool {
    c == b' ' || (9..=13).contains(&c)
}
fn is_xdigit(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

/// C's `tolower` in the C locale.
pub(crate) fn to_lower(c: u8) -> u8 {
    c.to_ascii_lowercase()
}

/// C's `toupper` in the C locale.
pub(crate) fn to_upper(c: u8) -> u8 {
    c.to_ascii_uppercase()
}

/// C's `match_class`.
fn match_class(c: u8, cl: u8) -> bool {
    let res = match to_lower(cl) {
        b'a' => is_alpha(c),
        b'c' => is_cntrl(c),
        b'd' => is_digit(c),
        b'g' => is_graph(c),
        b'l' => is_lower(c),
        b'p' => is_punct(c),
        b's' => is_space(c),
        b'u' => is_upper(c),
        b'w' => is_alnum(c),
        b'x' => is_xdigit(c),
        b'z' => c == 0,
        _ => return cl == c,
    };
    if is_lower(cl) { res } else { !res }
}

/// C's `matchbracketclass`: `p` is at the `'['`, `ec` at the closing `']'`.
fn match_bracket_class(pat: &[u8], c: u8, mut p: u32, ec: u32) -> bool {
    let mut sig = true;
    if at(pat, p.saturating_add(1)) == b'^' {
        sig = false;
        p = p.saturating_add(1);
    }
    loop {
        p = p.saturating_add(1);
        if p >= ec {
            break;
        }
        if at(pat, p) == L_ESC {
            p = p.saturating_add(1);
            if match_class(c, at(pat, p)) {
                return sig;
            }
        } else if at(pat, p.saturating_add(1)) == b'-' && p.saturating_add(2) < ec {
            p = p.saturating_add(2);
            if at(pat, p - 2) <= c && c <= at(pat, p) {
                return sig;
            }
        } else if at(pat, p) == c {
            return sig;
        }
    }
    !sig
}

/// C's `classend`: the end of the single-character class at `p`.
fn class_end(pat: &[u8], p: u32, budget: &mut i64) -> Result<u32, PatternError> {
    let plen = len32(pat);
    let mut q = p.saturating_add(1);
    match at(pat, p) {
        L_ESC => {
            if q >= plen {
                return Err(PatternError::EndsWithPercent);
            }
            Ok(q.saturating_add(1))
        }
        b'[' => {
            if at(pat, q) == b'^' {
                q = q.saturating_add(1);
            }
            loop {
                if q >= plen {
                    return Err(PatternError::MissingBracket);
                }
                let c = at(pat, q);
                q = q.saturating_add(1);
                if c == L_ESC && q < plen {
                    q = q.saturating_add(1);
                }
                if at(pat, q) == b']' {
                    break;
                }
            }
            let ep = q.saturating_add(1);
            *budget -= set_cost(pat, p, ep);
            Ok(ep)
        }
        _ => Ok(q),
    }
}

/// C's `singlematch`.
fn single_match(subj: &[u8], pat: &[u8], s: u32, p: u32, ep: u32, budget: &mut i64) -> bool {
    let Some(&c) = subj.get(s as usize) else {
        return false;
    };
    match at(pat, p) {
        b'.' => true,
        L_ESC => match_class(c, at(pat, p.saturating_add(1))),
        b'[' => {
            *budget -= set_cost(pat, p, ep);
            match_bracket_class(pat, c, p, ep.saturating_sub(1))
        }
        x => x == c,
    }
}

/// A suspended `match` call waiting for the call it made to return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frame {
    /// `start_capture`: undo the capture on failure.
    StartCapture,
    /// `end_capture`: reopen capture `l` on failure.
    EndCapture { l: u8 },
    /// The `?` case: on failure continue this call at `(s, p)`.
    Optional { s: u32, p: u32 },
    /// `max_expand`, trying `i` repetitions from `s`; `p` is `ep + 1`.
    MaxExpand { s: u32, i: u32, p: u32 },
    /// `min_expand` at `s` for the class `p..ep`.
    MinExpand { s: u32, p: u32, ep: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Entry of `match(ms, s, p)`: the depth check.
    Call {
        s: u32,
        p: u32,
    },
    /// The `init:` label of `match`.
    Body {
        s: u32,
        p: u32,
    },
    /// `max_expand`'s counting loop: `i` repetitions of `p..ep` from `s` so far.
    Count {
        s: u32,
        p: u32,
        ep: u32,
        i: u32,
    },
    /// `matchbalance`'s scan: `p` at the two delimiters, `cur` the last byte seen.
    Balance {
        p: u32,
        cur: u32,
        cont: u32,
    },
    /// The current `match` call returns this.
    Return(Option<u32>),
    Done(Outcome),
}

/// One attempt of C's `match(ms, s, p)` from a subject position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Matcher {
    mode: Mode,
    stack: Vec<Frame>,
    level: u8,
    captures: [Capture; MAX_CAPTURES],
}

impl Default for Matcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Matcher {
    pub(crate) fn new() -> Self {
        Matcher {
            mode: Mode::Done(Outcome::Failed),
            stack: Vec::new(),
            level: 0,
            captures: [Capture {
                start: 0,
                len: CapLen::Open,
            }; MAX_CAPTURES],
        }
    }

    /// C's `reprepstate` followed by the call `match(ms, s, p)`.
    pub(crate) fn start(&mut self, s: u32, p: u32) {
        self.mode = Mode::Call { s, p };
        self.stack.clear();
        self.level = 0;
    }

    pub(crate) fn run(&mut self, subject: &[u8], pattern: &[u8], budget: &mut i64) -> Outcome {
        loop {
            if let Mode::Done(outcome) = self.mode {
                return outcome;
            }
            if *budget <= 0 {
                return Outcome::Pending;
            }
            *budget -= 1;
            self.mode = self.step(subject, pattern, budget);
        }
    }

    pub(crate) fn level(&self) -> usize {
        usize::from(self.level)
    }

    pub(crate) fn capture(&self, i: usize) -> Option<Capture> {
        if i < self.level() {
            self.captures.get(i).copied()
        } else {
            None
        }
    }

    /// C's `get_onecapture`: capture `i` (0-based), or the whole match
    /// `s..e` when `i == 0` and there are no captures.
    pub(crate) fn capture_value(
        &self,
        i: usize,
        s: u32,
        e: u32,
    ) -> Result<CaptureValue, PatternError> {
        let Some(cap) = self.capture(i) else {
            if i != 0 {
                let shown = u8::try_from(i.saturating_add(1)).unwrap_or(u8::MAX);
                return Err(PatternError::InvalidCaptureIndex(shown));
            }
            return Ok(CaptureValue::Bytes { start: s, end: e });
        };
        match cap.len {
            CapLen::Open => Err(PatternError::UnfinishedCapture),
            CapLen::Position => Ok(CaptureValue::Position(i64::from(cap.start) + 1)),
            CapLen::Closed(len) => Ok(CaptureValue::Bytes {
                start: cap.start,
                end: cap.start.saturating_add(len),
            }),
        }
    }

    /// C's `push_captures` count: the level, or 1 for the whole match when
    /// there are no captures and `whole` is wanted (`match`, `gmatch`, `gsub`).
    pub(crate) fn result_count(&self, whole: bool) -> usize {
        if self.level == 0 && whole {
            1
        } else {
            self.level()
        }
    }

    fn push(&mut self, frame: Frame, s: u32, p: u32) -> Mode {
        self.stack.push(frame);
        Mode::Call { s, p }
    }

    fn step(&mut self, subj: &[u8], pat: &[u8], budget: &mut i64) -> Mode {
        match self.mode {
            Mode::Call { s, p } => {
                // `if (ms->matchdepth-- == 0)` in `match`.
                if self.stack.len() >= MAX_DEPTH as usize {
                    Mode::Done(Outcome::Error(PatternError::TooComplex))
                } else {
                    Mode::Body { s, p }
                }
            }
            Mode::Body { s, p } => self.body(subj, pat, s, p, budget),
            // `max_expand`'s counting loop, a unit a byte, in one step
            // while the budget lasts.
            Mode::Count { s, p, ep, mut i } => loop {
                let at_s = s.saturating_add(i);
                if !single_match(subj, pat, at_s, p, ep, budget) {
                    let next = ep.saturating_add(1);
                    break self.push(Frame::MaxExpand { s, i, p: next }, at_s, next);
                }
                i = i.saturating_add(1);
                if *budget <= 0 {
                    break Mode::Count { s, p, ep, i };
                }
                *budget -= 1;
            },
            Mode::Balance { p, cur, cont } => {
                let cur = cur.saturating_add(1);
                let Some(&c) = subj.get(cur as usize) else {
                    return Mode::Return(None);
                };
                if c == at(pat, p.saturating_add(1)) {
                    let cont = cont.saturating_sub(1);
                    if cont == 0 {
                        return Mode::Body {
                            s: cur.saturating_add(1),
                            p: p.saturating_add(2),
                        };
                    }
                    Mode::Balance { p, cur, cont }
                } else if c == at(pat, p) {
                    Mode::Balance {
                        p,
                        cur,
                        cont: cont.saturating_add(1),
                    }
                } else {
                    Mode::Balance { p, cur, cont }
                }
            }
            Mode::Return(res) => self.ret(subj, pat, res, budget),
            Mode::Done(outcome) => Mode::Done(outcome),
        }
    }

    /// A `match` call returned `res`: resume its caller.
    fn ret(&mut self, subj: &[u8], pat: &[u8], res: Option<u32>, budget: &mut i64) -> Mode {
        let Some(frame) = self.stack.pop() else {
            return Mode::Done(match res {
                Some(end) => Outcome::Matched { end },
                None => Outcome::Failed,
            });
        };
        match frame {
            // start_capture
            Frame::StartCapture => {
                if res.is_none() {
                    self.level = self.level.saturating_sub(1);
                }
                Mode::Return(res)
            }
            // end_capture
            Frame::EndCapture { l } => {
                if res.is_none()
                    && let Some(cap) = self.captures.get_mut(usize::from(l))
                {
                    cap.len = CapLen::Open;
                }
                Mode::Return(res)
            }
            // match, case '?'
            Frame::Optional { s, p } => match res {
                Some(_) => Mode::Return(res),
                None => Mode::Body { s, p },
            },
            // max_expand
            Frame::MaxExpand { s, i, p } => {
                if res.is_some() || i == 0 {
                    return Mode::Return(res);
                }
                let mut i = i - 1;
                // Where the rest of the pattern starts with a literal the
                // subject lacks, `match(ms, s + i, p)` fails at once, unless
                // the call itself passes the depth bound: skip those in one
                // go, charged as a bulk scan, whatever the budget left.
                if let Some(c) = first_literal(pat, p)
                    && self.stack.len() + 1 < MAX_DEPTH as usize
                {
                    let from = i;
                    while i > 0 && at(subj, s.saturating_add(i)) != c {
                        i -= 1;
                    }
                    *budget -= i64::from((from - i) / 64);
                }
                self.push(Frame::MaxExpand { s, i, p }, s.saturating_add(i), p)
            }
            // min_expand
            Frame::MinExpand { s, p, ep } => {
                if res.is_some() {
                    return Mode::Return(res);
                }
                if single_match(subj, pat, s, p, ep, budget) {
                    let s = s.saturating_add(1);
                    self.push(Frame::MinExpand { s, p, ep }, s, ep.saturating_add(1))
                } else {
                    Mode::Return(None)
                }
            }
        }
    }

    /// One pass through the `switch` of C's `match`, from `init:`.
    fn body(&mut self, subj: &[u8], pat: &[u8], s: u32, p: u32, budget: &mut i64) -> Mode {
        let plen = len32(pat);
        if p >= plen {
            return Mode::Return(Some(s));
        }
        match at(pat, p) {
            b'(' => {
                if at(pat, p.saturating_add(1)) == b')' {
                    self.start_capture(s, p.saturating_add(2), CapLen::Position)
                } else {
                    self.start_capture(s, p.saturating_add(1), CapLen::Open)
                }
            }
            b')' => self.end_capture(s, p.saturating_add(1)),
            b'$' if p.saturating_add(1) == plen => {
                Mode::Return(if s == len32(subj) { Some(s) } else { None })
            }
            L_ESC => match at(pat, p.saturating_add(1)) {
                b'b' => {
                    // matchbalance
                    let a = p.saturating_add(2);
                    if a.saturating_add(1) >= plen {
                        return Mode::Done(Outcome::Error(PatternError::MissingBalanceArgs));
                    }
                    if at(subj, s) != at(pat, a) {
                        Mode::Return(None)
                    } else {
                        Mode::Balance {
                            p: a,
                            cur: s,
                            cont: 1,
                        }
                    }
                }
                b'f' => {
                    let fp = p.saturating_add(2);
                    if at(pat, fp) != b'[' {
                        return Mode::Done(Outcome::Error(PatternError::MissingFrontierBracket));
                    }
                    let ep = match class_end(pat, fp, budget) {
                        Ok(ep) => ep,
                        Err(e) => return Mode::Done(Outcome::Error(e)),
                    };
                    *budget -= 2 * set_cost(pat, fp, ep);
                    let previous = if s == 0 { 0 } else { at(subj, s - 1) };
                    let ec = ep.saturating_sub(1);
                    if !match_bracket_class(pat, previous, fp, ec)
                        && match_bracket_class(pat, at(subj, s), fp, ec)
                    {
                        Mode::Body { s, p: ep }
                    } else {
                        Mode::Return(None)
                    }
                }
                d @ b'0'..=b'9' => self.match_capture(subj, s, p, d, budget),
                _ => self.single(subj, pat, s, p, budget),
            },
            _ => self.single(subj, pat, s, p, budget),
        }
    }

    /// The `default:` case of `match`: a single-character class and its suffix.
    fn single(&mut self, subj: &[u8], pat: &[u8], s: u32, p: u32, budget: &mut i64) -> Mode {
        let ep = match class_end(pat, p, budget) {
            Ok(ep) => ep,
            Err(e) => return Mode::Done(Outcome::Error(e)),
        };
        let suffix = at(pat, ep);
        let next = ep.saturating_add(1);
        if !single_match(subj, pat, s, p, ep, budget) {
            return if matches!(suffix, b'*' | b'?' | b'-') {
                Mode::Body { s, p: next }
            } else {
                Mode::Return(None)
            };
        }
        match suffix {
            b'?' => self.push(Frame::Optional { s, p: next }, s.saturating_add(1), next),
            b'+' => Mode::Count {
                s: s.saturating_add(1),
                p,
                ep,
                i: 0,
            },
            b'*' => Mode::Count { s, p, ep, i: 0 },
            b'-' => self.push(Frame::MinExpand { s, p, ep }, s, next),
            _ => Mode::Body {
                s: s.saturating_add(1),
                p: ep,
            },
        }
    }

    /// C's `start_capture`.
    fn start_capture(&mut self, s: u32, p: u32, what: CapLen) -> Mode {
        let level = self.level();
        let Some(cap) = self.captures.get_mut(level) else {
            return Mode::Done(Outcome::Error(PatternError::TooManyCaptures));
        };
        *cap = Capture {
            start: s,
            len: what,
        };
        self.level += 1;
        self.push(Frame::StartCapture, s, p)
    }

    /// C's `end_capture`, with `capture_to_close`.
    fn end_capture(&mut self, s: u32, p: u32) -> Mode {
        let level = self.level();
        let open = self
            .captures
            .iter()
            .take(level)
            .rposition(|cap| cap.len == CapLen::Open);
        let Some(l) = open else {
            return Mode::Done(Outcome::Error(PatternError::InvalidPatternCapture));
        };
        if let Some(cap) = self.captures.get_mut(l) {
            cap.len = CapLen::Closed(s.saturating_sub(cap.start));
        }
        self.push(Frame::EndCapture { l: l as u8 }, s, p)
    }

    /// C's `match_capture` with `check_capture`, for `%d` at `p`.
    fn match_capture(&mut self, subj: &[u8], s: u32, p: u32, digit: u8, budget: &mut i64) -> Mode {
        let invalid = Mode::Done(Outcome::Error(PatternError::InvalidCaptureIndex(
            digit - b'0',
        )));
        if digit == b'0' {
            return invalid;
        }
        let l = usize::from(digit - b'1');
        let Some(cap) = self.capture(l) else {
            return invalid;
        };
        let len = match cap.len {
            CapLen::Open => return invalid,
            // (size_t)CAP_POSITION exceeds any remaining length.
            CapLen::Position => return Mode::Return(None),
            CapLen::Closed(len) => len,
        };
        *budget -= bulk_cost(len);
        let slen = len32(subj);
        if slen.saturating_sub(s) < len {
            return Mode::Return(None);
        }
        let range = |a: u32| subj.get(a as usize..(a as usize).saturating_add(len as usize));
        match (range(cap.start), range(s)) {
            (Some(x), Some(y)) if x == y => Mode::Body {
                s: s.saturating_add(len),
                p: p.saturating_add(2),
            },
            _ => Mode::Return(None),
        }
    }

    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        let w = |x: u32| u64::from(x);
        let mode = match self.mode {
            Mode::Call { s, p } => [0, w(s), w(p), 0, 0],
            Mode::Body { s, p } => [1, w(s), w(p), 0, 0],
            Mode::Count { s, p, ep, i } => [2, w(s), w(p), w(ep), w(i)],
            Mode::Balance { p, cur, cont } => [3, w(p), w(cur), w(cont), 0],
            Mode::Return(res) => [4, encode_opt(res), 0, 0, 0],
            Mode::Done(outcome) => {
                let (a, b, c) = match outcome {
                    Outcome::Pending => (0, 0, 0),
                    Outcome::Matched { end } => (1, w(end), 0),
                    Outcome::Failed => (2, 0, 0),
                    Outcome::Error(e) => {
                        let (code, arg) = e.encode();
                        (3, code, arg)
                    }
                };
                [5, a, b, c, 0]
            }
        };
        out.extend_from_slice(&mode);
        out.push(u64::from(self.level));
        for cap in self.captures.iter().take(self.level()) {
            let (tag, len) = match cap.len {
                CapLen::Position => (0, 0),
                CapLen::Open => (1, 0),
                CapLen::Closed(len) => (2, len),
            };
            out.extend_from_slice(&[w(cap.start), tag, w(len)]);
        }
        out.push(self.stack.len() as u64);
        for frame in &self.stack {
            let words = match *frame {
                Frame::StartCapture => [0, 0, 0, 0],
                Frame::EndCapture { l } => [1, u64::from(l), 0, 0],
                Frame::Optional { s, p } => [2, w(s), w(p), 0],
                Frame::MaxExpand { s, i, p } => [3, w(s), w(i), w(p)],
                Frame::MinExpand { s, p, ep } => [4, w(s), w(p), w(ep)],
            };
            out.extend_from_slice(&words);
        }
    }

    pub(crate) fn decode(words: &mut &[u64], subject_len: u32, pattern_len: u32) -> Option<Self> {
        let (sl, pl) = (subject_len, pattern_len);
        let pos = |x: u64, max: u32| u32::try_from(x).ok().filter(|&x| x <= max);
        let [tag, a, b, c, d] = take_n::<5>(words)?;
        let mode = match tag {
            0 | 1 => {
                let (s, p) = (pos(a, sl)?, pos(b, pl)?);
                if tag == 0 {
                    Mode::Call { s, p }
                } else {
                    Mode::Body { s, p }
                }
            }
            2 => {
                let (s, p, ep) = (pos(a, sl)?, pos(b, pl)?, pos(c, pl)?);
                let i = pos(d, sl)?;
                if p >= ep || s.checked_add(i)? > sl {
                    return None;
                }
                Mode::Count { s, p, ep, i }
            }
            3 => {
                let p = pos(a, pl)?;
                if p.checked_add(1)? >= pl {
                    return None;
                }
                let cont = pos(c, sl.saturating_add(1))?;
                if cont == 0 {
                    return None;
                }
                Mode::Balance {
                    p,
                    cur: pos(b, sl)?,
                    cont,
                }
            }
            4 => Mode::Return(decode_opt(a, sl)?),
            5 => Mode::Done(match a {
                1 => Outcome::Matched { end: pos(b, sl)? },
                2 => Outcome::Failed,
                3 => Outcome::Error(PatternError::decode(b, c)?),
                _ => return None,
            }),
            _ => return None,
        };
        let level = take(words)?;
        if level > MAX_CAPTURES as u64 {
            return None;
        }
        let mut captures = [Capture {
            start: 0,
            len: CapLen::Open,
        }; MAX_CAPTURES];
        for cap in captures.iter_mut().take(level as usize) {
            let [start, tag, len] = take_n::<3>(words)?;
            let start = pos(start, sl)?;
            let len = match tag {
                0 => CapLen::Position,
                1 => CapLen::Open,
                2 => {
                    let len = pos(len, sl)?;
                    if start.checked_add(len)? > sl {
                        return None;
                    }
                    CapLen::Closed(len)
                }
                _ => return None,
            };
            *cap = Capture { start, len };
        }
        let depth = take(words)?;
        // A full stack is reachable only at the call that fails the depth check.
        let full_ok = matches!(mode, Mode::Call { .. } | Mode::Done(_));
        if depth > u64::from(MAX_DEPTH) || depth == u64::from(MAX_DEPTH) && !full_ok {
            return None;
        }
        let mut stack = Vec::with_capacity(depth as usize);
        for _ in 0..depth {
            let [tag, a, b, c] = take_n::<4>(words)?;
            stack.push(match tag {
                0 => Frame::StartCapture,
                1 if a < MAX_CAPTURES as u64 => Frame::EndCapture { l: a as u8 },
                2 => Frame::Optional {
                    s: pos(a, sl)?,
                    p: pos(b, pl)?,
                },
                3 => {
                    let (s, i, p) = (pos(a, sl)?, pos(b, sl)?, pos(c, pl)?);
                    if s.checked_add(i)? > sl {
                        return None;
                    }
                    Frame::MaxExpand { s, i, p }
                }
                4 => {
                    let (s, p, ep) = (pos(a, sl)?, pos(b, pl)?, pos(c, pl)?);
                    if p >= ep || ep >= pl {
                        return None;
                    }
                    Frame::MinExpand { s, p, ep }
                }
                _ => return None,
            });
        }
        Some(Matcher {
            mode,
            stack,
            level: level as u8,
            captures,
        })
    }
}

fn take(words: &mut &[u64]) -> Option<u64> {
    let (&first, rest) = words.split_first()?;
    *words = rest;
    Some(first)
}

fn take_n<const N: usize>(words: &mut &[u64]) -> Option<[u64; N]> {
    let head = words.get(..N)?;
    let arr: [u64; N] = head.try_into().ok()?;
    *words = words.get(N..)?;
    Some(arr)
}

fn encode_opt(x: Option<u32>) -> u64 {
    x.map_or(u64::MAX, u64::from)
}

/// `Some(None)` for the encoded `None`, `None` when invalid.
fn decode_opt(word: u64, max: u32) -> Option<Option<u32>> {
    if word == u64::MAX {
        return Some(None);
    }
    u32::try_from(word).ok().filter(|&x| x <= max).map(Some)
}

fn encode_search(outcome: SearchOutcome, out: &mut Vec<u64>) {
    let words = match outcome {
        SearchOutcome::Pending => [0, 0, 0],
        SearchOutcome::Found { start, end } => [1, u64::from(start), u64::from(end)],
        SearchOutcome::NotFound => [2, 0, 0],
        SearchOutcome::Error(e) => {
            let (code, arg) = e.encode();
            [3, code, arg]
        }
    };
    out.extend_from_slice(&words);
}

fn decode_search(words: &mut &[u64], subject_len: u32) -> Option<SearchOutcome> {
    let [tag, a, b] = take_n::<3>(words)?;
    let pos = |x: u64| u32::try_from(x).ok().filter(|&x| x <= subject_len);
    Some(match tag {
        1 => {
            let (start, end) = (pos(a)?, pos(b)?);
            if start > end {
                return None;
            }
            SearchOutcome::Found { start, end }
        }
        2 => SearchOutcome::NotFound,
        3 => SearchOutcome::Error(PatternError::decode(a, b)?),
        _ => return None,
    })
}

/// C's `nospecials`: whether the pattern has none of `^$*+?.([%-`, looking
/// past embedded zeros.
/// The byte a match must start with, when the pattern at `p` begins with
/// a plain character that must occur: not a class, a set, a capture, the
/// end anchor, or an item that may match nothing. At a position without
/// it, C's matcher fails on that first item without reading further, so a
/// scan may skip such positions and the results, errors included, are the
/// same.
fn first_literal(pattern: &[u8], p: u32) -> Option<u8> {
    let p = p as usize;
    let c = *pattern.get(p)?;
    if matches!(c, b'%' | b'.' | b'[' | b'(' | b')') || c == b'$' && p + 1 == pattern.len() {
        return None;
    }
    if matches!(pattern.get(p + 1), Some(b'*' | b'?' | b'-')) {
        return None;
    }
    Some(c)
}

/// Where the next match attempt from `from` can start, when the pattern
/// starts with a literal: the next position holding it, or `None` when no
/// position before the end does. Charged as a bulk scan.
fn next_candidate(
    subject: &[u8],
    pattern: &[u8],
    p: u32,
    from: u32,
    budget: &mut i64,
) -> Option<Option<u32>> {
    let c = first_literal(pattern, p)?;
    let rest = subject.get(from as usize..).unwrap_or_default();
    let found = rest.iter().position(|byte| *byte == c);
    let scanned = found.unwrap_or(rest.len());
    *budget -= (scanned / 64) as i64;
    Some(found.map(|offset| from + offset as u32))
}

pub(crate) fn no_specials(pattern: &[u8]) -> bool {
    !pattern.iter().any(|c| SPECIALS.contains(c))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// About to start a match attempt (costs 1).
    Restart,
    Matching,
    Finished(SearchOutcome),
}

fn encode_phase(phase: Phase, out: &mut Vec<u64>) {
    match phase {
        Phase::Restart => out.extend_from_slice(&[0, 0, 0, 0]),
        Phase::Matching => out.extend_from_slice(&[1, 0, 0, 0]),
        Phase::Finished(outcome) => {
            out.push(2);
            encode_search(outcome, out);
        }
    }
}

fn decode_phase(words: &mut &[u64], subject_len: u32) -> Option<Phase> {
    let tag = take(words)?;
    match tag {
        0 | 1 => {
            let [a, b, c] = take_n::<3>(words)?;
            if a != 0 || b != 0 || c != 0 {
                return None;
            }
            Some(if tag == 0 {
                Phase::Restart
            } else {
                Phase::Matching
            })
        }
        2 => Some(Phase::Finished(decode_search(words, subject_len)?)),
        _ => None,
    }
}

/// The loop of C's `str_find_aux` in pattern mode (for `find` and `match`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Search {
    s1: u32,
    p0: u32,
    anchor: bool,
    phase: Phase,
    matcher: Matcher,
}

impl Search {
    /// `init` is the 0-based start, already clipped by the caller (C
    /// returns fail before searching when it exceeds the subject length).
    pub(crate) fn new(pattern: &[u8], init: u32) -> Self {
        let anchor = at(pattern, 0) == b'^';
        Search {
            s1: init,
            p0: u32::from(anchor),
            anchor,
            phase: Phase::Restart,
            matcher: Matcher::new(),
        }
    }

    pub(crate) fn run(
        &mut self,
        subject: &[u8],
        pattern: &[u8],
        budget: &mut i64,
    ) -> SearchOutcome {
        let slen = len32(subject);
        loop {
            match self.phase {
                Phase::Finished(outcome) => return outcome,
                Phase::Restart => {
                    if self.s1 > slen {
                        self.phase = Phase::Finished(SearchOutcome::NotFound);
                        continue;
                    }
                    if *budget <= 0 {
                        return SearchOutcome::Pending;
                    }
                    *budget -= 1;
                    if !self.anchor {
                        match next_candidate(subject, pattern, self.p0, self.s1, budget) {
                            Some(None) => {
                                self.phase = Phase::Finished(SearchOutcome::NotFound);
                                continue;
                            }
                            Some(Some(at)) => self.s1 = at,
                            None => {}
                        }
                    }
                    self.matcher.start(self.s1, self.p0);
                    self.phase = Phase::Matching;
                }
                Phase::Matching => {
                    self.phase = match self.matcher.run(subject, pattern, budget) {
                        Outcome::Pending => return SearchOutcome::Pending,
                        Outcome::Matched { end } => Phase::Finished(SearchOutcome::Found {
                            start: self.s1,
                            end,
                        }),
                        Outcome::Error(e) => Phase::Finished(SearchOutcome::Error(e)),
                        Outcome::Failed => {
                            // `while (s1++ < ms.src_end && !anchor)`
                            if self.s1 < slen && !self.anchor {
                                self.s1 += 1;
                                Phase::Restart
                            } else {
                                Phase::Finished(SearchOutcome::NotFound)
                            }
                        }
                    };
                }
            }
        }
    }

    /// The matcher, holding the captures of a found match.
    pub(crate) fn matcher(&self) -> &Matcher {
        &self.matcher
    }

    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        out.extend_from_slice(&[u64::from(self.s1), u64::from(self.p0)]);
        encode_phase(self.phase, out);
        self.matcher.encode(out);
    }

    pub(crate) fn decode(words: &mut &[u64], subject_len: u32, pattern_len: u32) -> Option<Self> {
        let [s1, p0] = take_n::<2>(words)?;
        let s1 = u32::try_from(s1)
            .ok()
            .filter(|&s| s <= subject_len.saturating_add(1))?;
        if p0 > 1 || p0 > u64::from(pattern_len) {
            return None;
        }
        let phase = decode_phase(words, subject_len)?;
        let matcher = Matcher::decode(words, subject_len, pattern_len)?;
        Some(Search {
            s1,
            p0: p0 as u32,
            anchor: p0 == 1,
            phase,
            matcher,
        })
    }
}

/// `find` with `plain` or a pattern without specials: C's `lmemfind`,
/// resumable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlainSearch {
    init: u32,
    /// Where the next `memchr` starts.
    next: u32,
    result: SearchOutcome,
}

impl PlainSearch {
    /// `init` is the 0-based start, already clipped by the caller.
    pub(crate) fn new(init: u32) -> Self {
        PlainSearch {
            init,
            next: init,
            result: SearchOutcome::Pending,
        }
    }

    pub(crate) fn run(&mut self, subject: &[u8], needle: &[u8], budget: &mut i64) -> SearchOutcome {
        while self.result == SearchOutcome::Pending {
            if *budget <= 0 {
                return SearchOutcome::Pending;
            }
            *budget -= 1;
            self.result = self.step(subject, needle, budget);
        }
        self.result
    }

    fn step(&mut self, subject: &[u8], needle: &[u8], budget: &mut i64) -> SearchOutcome {
        let slen = len32(subject);
        let l2 = len32(needle);
        if self.init > slen {
            return SearchOutcome::NotFound;
        }
        if l2 == 0 {
            // Empty strings are everywhere.
            return SearchOutcome::Found {
                start: self.init,
                end: self.init,
            };
        }
        let (Some(last), Some(&first)) = (slen.checked_sub(l2), needle.first()) else {
            return SearchOutcome::NotFound;
        };
        if self.next > last {
            return SearchOutcome::NotFound;
        }
        let window = subject
            .get(self.next as usize..=last as usize)
            .unwrap_or(&[]);
        let found = window.iter().position(|&c| c == first);
        let scanned = found.map_or(window.len(), |i| i + 1);
        *budget -= bulk_cost(u32::try_from(scanned).unwrap_or(u32::MAX)) - 1;
        let Some(offset) = found else {
            return SearchOutcome::NotFound;
        };
        let k = self.next.saturating_add(offset as u32);
        *budget -= bulk_cost(l2 - 1);
        let tail = subject.get(k as usize + 1..k as usize + l2 as usize);
        if tail == needle.get(1..) {
            return SearchOutcome::Found {
                start: k,
                end: k.saturating_add(l2),
            };
        }
        self.next = k.saturating_add(1);
        SearchOutcome::Pending
    }

    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        out.extend_from_slice(&[u64::from(self.init), u64::from(self.next)]);
        encode_search(self.result, out);
    }

    pub(crate) fn decode(words: &mut &[u64], subject_len: u32, needle_len: u32) -> Option<Self> {
        let _ = needle_len;
        let [init, next] = take_n::<2>(words)?;
        let pos = |x: u64| {
            u32::try_from(x)
                .ok()
                .filter(|&x| x <= subject_len.saturating_add(1))
        };
        let (init, next) = (pos(init)?, pos(next)?);
        if next < init {
            return None;
        }
        let result = if words.first() == Some(&0) {
            if take_n::<3>(words)? != [0, 0, 0] {
                return None;
            }
            SearchOutcome::Pending
        } else {
            decode_search(words, subject_len)?
        };
        Some(PlainSearch { init, next, result })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GmatchPhase {
    /// Between calls.
    Idle,
    Restart {
        cur: u32,
    },
    Matching {
        cur: u32,
    },
    Exhausted,
}

/// The state of a `gmatch` iterator (C's `GMatchState`) and of the call in
/// progress. The pattern is used whole: in 5.4.9 a leading `'^'` is not an
/// anchor for `gmatch`, so it matches a literal `'^'`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Gmatch {
    src: u32,
    lastmatch: Option<u32>,
    phase: GmatchPhase,
    matcher: Matcher,
}

impl Gmatch {
    /// `init` is the 0-based start; C clips it to `len + 1`.
    pub(crate) fn new(init: u32) -> Self {
        Gmatch {
            src: init,
            lastmatch: None,
            phase: GmatchPhase::Idle,
            matcher: Matcher::new(),
        }
    }

    /// Starts one call of the iterator (`gmatch_aux`). `run` in the idle
    /// state does this itself.
    pub(crate) fn begin_call(&mut self) {
        if self.phase != GmatchPhase::Exhausted {
            self.phase = GmatchPhase::Restart { cur: self.src };
        }
    }

    /// Runs the current call: `Found` with the match bounds (captures in
    /// [`Gmatch::matcher`]) or `NotFound` once exhausted, which it then stays.
    pub(crate) fn run(
        &mut self,
        subject: &[u8],
        pattern: &[u8],
        budget: &mut i64,
    ) -> SearchOutcome {
        let slen = len32(subject);
        loop {
            match self.phase {
                GmatchPhase::Idle => self.begin_call(),
                GmatchPhase::Exhausted => return SearchOutcome::NotFound,
                GmatchPhase::Restart { cur } => {
                    if cur > slen {
                        self.phase = GmatchPhase::Exhausted;
                        continue;
                    }
                    if *budget <= 0 {
                        return SearchOutcome::Pending;
                    }
                    *budget -= 1;
                    let cur = match next_candidate(subject, pattern, 0, cur, budget) {
                        Some(None) => {
                            self.phase = GmatchPhase::Exhausted;
                            continue;
                        }
                        Some(Some(at)) => at,
                        None => cur,
                    };
                    self.matcher.start(cur, 0);
                    self.phase = GmatchPhase::Matching { cur };
                }
                GmatchPhase::Matching { cur } => match self.matcher.run(subject, pattern, budget) {
                    Outcome::Pending => return SearchOutcome::Pending,
                    Outcome::Error(e) => return SearchOutcome::Error(e),
                    Outcome::Matched { end } if Some(end) != self.lastmatch => {
                        self.src = end;
                        self.lastmatch = Some(end);
                        self.phase = GmatchPhase::Idle;
                        return SearchOutcome::Found { start: cur, end };
                    }
                    Outcome::Matched { .. } | Outcome::Failed => {
                        self.phase = GmatchPhase::Restart {
                            cur: cur.saturating_add(1),
                        };
                    }
                },
            }
        }
    }

    pub(crate) fn matcher(&self) -> &Matcher {
        &self.matcher
    }

    /// The iterator's state between calls, all a `gmatch` closure keeps
    /// (ADR 0035): where the next scan starts, the end of the last match,
    /// and whether the iterator is exhausted.
    pub(crate) fn between_calls(&self) -> (u32, Option<u32>, bool) {
        (
            self.src,
            self.lastmatch,
            self.phase == GmatchPhase::Exhausted,
        )
    }

    /// An iterator between calls, from [`Gmatch::between_calls`].
    pub(crate) fn resume(src: u32, lastmatch: Option<u32>, exhausted: bool) -> Self {
        Gmatch {
            src,
            lastmatch,
            phase: if exhausted {
                GmatchPhase::Exhausted
            } else {
                GmatchPhase::Idle
            },
            matcher: Matcher::new(),
        }
    }

    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        let (tag, cur) = match self.phase {
            GmatchPhase::Idle => (0, 0),
            GmatchPhase::Restart { cur } => (1, cur),
            GmatchPhase::Matching { cur } => (2, cur),
            GmatchPhase::Exhausted => (3, 0),
        };
        out.extend_from_slice(&[
            u64::from(self.src),
            encode_opt(self.lastmatch),
            tag,
            u64::from(cur),
        ]);
        self.matcher.encode(out);
    }

    pub(crate) fn decode(words: &mut &[u64], subject_len: u32, pattern_len: u32) -> Option<Self> {
        let [src, lastmatch, tag, cur] = take_n::<4>(words)?;
        let past_end = subject_len.saturating_add(1);
        let pos = |x: u64| u32::try_from(x).ok().filter(|&x| x <= past_end);
        let src = pos(src)?;
        let lastmatch = decode_opt(lastmatch, subject_len)?;
        let cur = pos(cur)?;
        let phase = match tag {
            0 => GmatchPhase::Idle,
            1 if cur >= src => GmatchPhase::Restart { cur },
            2 if cur >= src && cur <= subject_len => GmatchPhase::Matching { cur },
            3 => GmatchPhase::Exhausted,
            _ => return None,
        };
        let matcher = Matcher::decode(words, subject_len, pattern_len)?;
        Some(Gmatch {
            src,
            lastmatch,
            phase,
            matcher,
        })
    }
}

/// The loop of C's `str_gsub`. Each `Found` is a replacement site; the
/// caller writes `subject[unmatched_from()..start]` and then the
/// replacement. After `NotFound` it writes `subject[unmatched_from()..]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Gsub {
    src: u32,
    lastmatch: Option<u32>,
    unmatched: u32,
    n: i64,
    max: i64,
    anchor: bool,
    phase: Phase,
    matcher: Matcher,
}

impl Gsub {
    /// `max` is C's `max_s` (the subject length + 1 when absent).
    pub(crate) fn new(pattern: &[u8], max: i64) -> Self {
        Gsub {
            src: 0,
            lastmatch: None,
            unmatched: 0,
            n: 0,
            max,
            anchor: at(pattern, 0) == b'^',
            phase: Phase::Restart,
            matcher: Matcher::new(),
        }
    }

    pub(crate) fn run(
        &mut self,
        subject: &[u8],
        pattern: &[u8],
        budget: &mut i64,
    ) -> SearchOutcome {
        let slen = len32(subject);
        loop {
            match self.phase {
                Phase::Finished(outcome) => {
                    self.unmatched = self.lastmatch.unwrap_or(0);
                    return outcome;
                }
                Phase::Restart => {
                    if self.n >= self.max {
                        self.phase = Phase::Finished(SearchOutcome::NotFound);
                        continue;
                    }
                    if *budget <= 0 {
                        return SearchOutcome::Pending;
                    }
                    *budget -= 1;
                    // Skipped bytes stay in the unmatched run.
                    if !self.anchor {
                        match next_candidate(subject, pattern, 0, self.src, budget) {
                            Some(None) => {
                                self.src = slen;
                                self.phase = Phase::Finished(SearchOutcome::NotFound);
                                continue;
                            }
                            Some(Some(at)) => self.src = at,
                            None => {}
                        }
                    }
                    self.matcher.start(self.src, u32::from(self.anchor));
                    self.phase = Phase::Matching;
                }
                Phase::Matching => match self.matcher.run(subject, pattern, budget) {
                    Outcome::Pending => return SearchOutcome::Pending,
                    Outcome::Error(e) => return SearchOutcome::Error(e),
                    Outcome::Matched { end } if Some(end) != self.lastmatch => {
                        self.n = self.n.saturating_add(1);
                        self.unmatched = self.lastmatch.unwrap_or(0);
                        let start = self.src;
                        self.src = end;
                        self.lastmatch = Some(end);
                        self.phase = self.after_attempt();
                        return SearchOutcome::Found { start, end };
                    }
                    Outcome::Matched { .. } | Outcome::Failed => {
                        if self.src < slen {
                            self.src += 1;
                            self.phase = self.after_attempt();
                        } else {
                            self.phase = Phase::Finished(SearchOutcome::NotFound);
                        }
                    }
                },
            }
        }
    }

    fn after_attempt(&self) -> Phase {
        if self.anchor {
            Phase::Finished(SearchOutcome::NotFound)
        } else {
            Phase::Restart
        }
    }

    /// The number of matches so far (C's `n`).
    pub(crate) fn count(&self) -> i64 {
        self.n
    }

    /// Where the run of unmatched subject bytes before the last `Found`
    /// starts (the end of the previous match, or 0); after `NotFound`, where
    /// the unmatched tail starts.
    pub(crate) fn unmatched_from(&self) -> u32 {
        self.unmatched
    }

    pub(crate) fn matcher(&self) -> &Matcher {
        &self.matcher
    }

    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        out.extend_from_slice(&[
            u64::from(self.src),
            encode_opt(self.lastmatch),
            u64::from(self.unmatched),
            self.n as u64,
            self.max as u64,
            u64::from(self.anchor),
        ]);
        encode_phase(self.phase, out);
        self.matcher.encode(out);
    }

    pub(crate) fn decode(words: &mut &[u64], subject_len: u32, pattern_len: u32) -> Option<Self> {
        let [src, lastmatch, unmatched, n, max, anchor] = take_n::<6>(words)?;
        let pos = |x: u64| u32::try_from(x).ok().filter(|&x| x <= subject_len);
        let src = pos(src)?;
        let lastmatch = decode_opt(lastmatch, subject_len)?;
        let unmatched = pos(unmatched)?;
        let (n, max) = (n as i64, max as i64);
        if n < 0 || anchor > 1 || anchor > u64::from(pattern_len) {
            return None;
        }
        let phase = decode_phase(words, subject_len)?;
        let matcher = Matcher::decode(words, subject_len, pattern_len)?;
        Some(Gsub {
            src,
            lastmatch,
            unmatched,
            n,
            max,
            anchor: anchor == 1,
            phase,
            matcher,
        })
    }
}

/// C's `add_s`: appends the string replacement `repl` for the match `s..e`.
/// Position captures are written as decimal integers.
pub(crate) fn expand_replacement(
    repl: &[u8],
    subject: &[u8],
    m: &Matcher,
    s: u32,
    e: u32,
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<bool, PatternError> {
    let bytes = |a: u32, b: u32| subject.get(a as usize..b as usize).unwrap_or(&[]);
    // Each piece is checked against `limit` before it is appended.
    let add = |out: &mut Vec<u8>, piece: &[u8]| {
        if out.len().saturating_add(piece.len()) > limit {
            return false;
        }
        out.extend_from_slice(piece);
        true
    };
    let mut rest = repl;
    while let Some(i) = rest.iter().position(|&c| c == L_ESC) {
        if !add(out, rest.get(..i).unwrap_or(&[])) {
            return Ok(false);
        }
        let c = rest.get(i + 1).copied().unwrap_or(0);
        let added = match c {
            L_ESC => add(out, &[L_ESC]),
            b'0' => add(out, bytes(s, e)),
            b'1'..=b'9' => match m.capture_value(usize::from(c - b'1'), s, e)? {
                CaptureValue::Bytes { start, end } => add(out, bytes(start, end)),
                CaptureValue::Position(n) => add(out, n.to_string().as_bytes()),
            },
            _ => return Err(PatternError::InvalidReplacement),
        };
        if !added {
            return Ok(false);
        }
        rest = rest.get(i + 2..).unwrap_or(&[]);
    }
    Ok(add(out, rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    trait Machine: Sized {
        fn go(&mut self, subject: &[u8], pattern: &[u8], budget: &mut i64) -> SearchOutcome;
        fn enc(&self, out: &mut Vec<u64>);
        fn dec(words: &mut &[u64], sl: u32, pl: u32) -> Option<Self>;
        fn depth(&self) -> usize;
    }

    macro_rules! machine {
        ($t:ty) => {
            impl Machine for $t {
                fn go(&mut self, s: &[u8], p: &[u8], b: &mut i64) -> SearchOutcome {
                    self.run(s, p, b)
                }
                fn enc(&self, out: &mut Vec<u64>) {
                    self.encode(out)
                }
                fn dec(words: &mut &[u64], sl: u32, pl: u32) -> Option<Self> {
                    <$t>::decode(words, sl, pl)
                }
                fn depth(&self) -> usize {
                    self.matcher.stack.len()
                }
            }
        };
    }
    machine!(Search);
    machine!(Gmatch);
    machine!(Gsub);

    impl Machine for PlainSearch {
        fn go(&mut self, s: &[u8], p: &[u8], b: &mut i64) -> SearchOutcome {
            self.run(s, p, b)
        }
        fn enc(&self, out: &mut Vec<u64>) {
            self.encode(out)
        }
        fn dec(words: &mut &[u64], sl: u32, pl: u32) -> Option<Self> {
            PlainSearch::decode(words, sl, pl)
        }
        fn depth(&self) -> usize {
            0
        }
    }

    /// A budget schedule: refills of `chunk`, debt carried, optionally
    /// round-tripping the state through encode/decode at every pending point.
    struct Fuel {
        budget: i64,
        added: i64,
        chunk: i64,
        reencode: bool,
        cap: i64,
        max_depth: usize,
    }

    impl Fuel {
        fn new(chunk: i64, reencode: bool) -> Self {
            Fuel {
                budget: 0,
                added: 0,
                chunk,
                reencode,
                cap: i64::MAX,
                max_depth: 0,
            }
        }
        fn used(&self) -> i64 {
            self.added - self.budget
        }
    }

    /// `None` when the fuel cap was reached.
    fn drive<M: Machine>(m: &mut M, s: &[u8], p: &[u8], fuel: &mut Fuel) -> Option<SearchOutcome> {
        loop {
            let r = m.go(s, p, &mut fuel.budget);
            fuel.max_depth = fuel.max_depth.max(m.depth());
            if r != SearchOutcome::Pending {
                return Some(r);
            }
            if fuel.used() >= fuel.cap {
                return None;
            }
            if fuel.reencode {
                let mut words = Vec::new();
                m.enc(&mut words);
                let mut slice = &words[..];
                *m = M::dec(&mut slice, len32(s), len32(p)).expect("decode of a reachable state");
                assert!(slice.is_empty());
                let mut again = Vec::new();
                m.enc(&mut again);
                assert_eq!(words, again);
            }
            fuel.budget += fuel.chunk;
            fuel.added += fuel.chunk;
        }
    }

    #[derive(Clone, Debug)]
    enum Case {
        Find {
            s: Vec<u8>,
            p: Vec<u8>,
            init: Option<i64>,
            plain: bool,
        },
        Match {
            s: Vec<u8>,
            p: Vec<u8>,
            init: Option<i64>,
        },
        Gmatch {
            s: Vec<u8>,
            p: Vec<u8>,
            init: Option<i64>,
        },
        Gsub {
            s: Vec<u8>,
            p: Vec<u8>,
            repl: Vec<u8>,
            max: Option<i64>,
        },
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    /// C's `posrelatI`.
    fn posrelat(pos: i64, len: u32) -> i64 {
        let len = i64::from(len);
        if pos > 0 {
            pos
        } else if pos == 0 || pos < -len {
            1
        } else {
            len + pos + 1
        }
    }

    fn value(v: Result<CaptureValue, PatternError>, s: &[u8]) -> Result<String, PatternError> {
        Ok(match v? {
            CaptureValue::Bytes { start, end } => hex(&s[start as usize..end as usize]),
            CaptureValue::Position(n) => format!("p{n}"),
        })
    }

    fn captures(
        m: &Matcher,
        whole: bool,
        s: &[u8],
        st: u32,
        en: u32,
    ) -> Result<Vec<String>, PatternError> {
        (0..m.result_count(whole))
            .map(|i| value(m.capture_value(i, st, en), s))
            .collect()
    }

    fn err(e: PatternError) -> String {
        format!("E:{}", e.message())
    }

    fn outcome_err(r: SearchOutcome) -> Option<String> {
        match r {
            SearchOutcome::Error(e) => Some(err(e)),
            _ => None,
        }
    }

    /// Evaluates a case as the corresponding Lua call would; `None` when
    /// the fuel cap was hit.
    fn eval(case: &Case, fuel: &mut Fuel) -> Option<String> {
        Some(match case {
            Case::Find { s, p, init, plain } => {
                let ls = len32(s);
                let init = posrelat(init.unwrap_or(1), ls) - 1;
                if init > i64::from(ls) {
                    return Some("O:nil".into());
                }
                let init = init as u32;
                if *plain || no_specials(p) {
                    let mut m = PlainSearch::new(init);
                    match drive(&mut m, s, p, fuel)? {
                        SearchOutcome::Found { start, end } => format!("O:p{},p{end}", start + 1),
                        SearchOutcome::NotFound => "O:nil".into(),
                        r => panic!("{r:?}"),
                    }
                } else {
                    let mut m = Search::new(p, init);
                    match drive(&mut m, s, p, fuel)? {
                        SearchOutcome::Found { start, end } => {
                            match captures(m.matcher(), false, s, start, end) {
                                Ok(caps) => {
                                    let mut v = vec![format!("p{}", start + 1), format!("p{end}")];
                                    v.extend(caps);
                                    format!("O:{}", v.join(","))
                                }
                                Err(e) => err(e),
                            }
                        }
                        SearchOutcome::NotFound => "O:nil".into(),
                        r => outcome_err(r).unwrap(),
                    }
                }
            }
            Case::Match { s, p, init } => {
                let ls = len32(s);
                let init = posrelat(init.unwrap_or(1), ls) - 1;
                if init > i64::from(ls) {
                    return Some("O:nil".into());
                }
                let mut m = Search::new(p, init as u32);
                match drive(&mut m, s, p, fuel)? {
                    SearchOutcome::Found { start, end } => {
                        match captures(m.matcher(), true, s, start, end) {
                            Ok(caps) => format!("O:{}", caps.join(",")),
                            Err(e) => err(e),
                        }
                    }
                    SearchOutcome::NotFound => "O:nil".into(),
                    r => outcome_err(r).unwrap(),
                }
            }
            Case::Gmatch { s, p, init } => {
                let ls = len32(s);
                let init = (posrelat(init.unwrap_or(1), ls) - 1).min(i64::from(ls) + 1);
                let mut m = Gmatch::new(init as u32);
                let mut out = Vec::new();
                for _ in 0..40 {
                    m.begin_call();
                    match drive(&mut m, s, p, fuel)? {
                        SearchOutcome::Found { start, end } => {
                            match captures(m.matcher(), true, s, start, end) {
                                Ok(caps) => out.push(caps.join(",")),
                                Err(e) => return Some(err(e)),
                            }
                        }
                        SearchOutcome::NotFound => {
                            out.push("END".into());
                            break;
                        }
                        r => return Some(outcome_err(r).unwrap()),
                    }
                }
                format!("O:{}", out.join(";"))
            }
            Case::Gsub { s, p, repl, max } => {
                let ls = len32(s);
                let mut m = Gsub::new(p, max.unwrap_or(i64::from(ls) + 1));
                let mut out = Vec::new();
                loop {
                    match drive(&mut m, s, p, fuel)? {
                        SearchOutcome::Found { start, end } => {
                            out.extend_from_slice(&s[m.unmatched_from() as usize..start as usize]);
                            if let Err(e) = expand_replacement(
                                repl,
                                s,
                                m.matcher(),
                                start,
                                end,
                                &mut out,
                                usize::MAX,
                            ) {
                                return Some(err(e));
                            }
                        }
                        SearchOutcome::NotFound => {
                            out.extend_from_slice(&s[m.unmatched_from() as usize..]);
                            break;
                        }
                        r => return Some(outcome_err(r).unwrap()),
                    }
                }
                format!("O:{},p{}", hex(&out), m.count())
            }
        })
    }

    fn eval_plain(case: &Case) -> String {
        eval(case, &mut Fuel::new(i64::MAX / 4, false)).unwrap()
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn chance(&mut self, percent: usize) -> bool {
            self.below(100) < percent
        }
        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[self.below(items.len())]
        }
    }

    const SUBJECT_BYTES: &[u8] = b"aaaabbbcc ()A1_-]%^$.\0\x80\xff\nx";
    const LITERALS: &[u8] = b"aabbc ()A1_]^-\0\x80\xff\n";
    const CLASSES: &[u8] = b"acdglpsuwxzACDGLPSUWXZ";
    const ESCAPED: &[u8] = b".%()[]-^$*+?\0\xff";

    fn gen_set(rng: &mut Rng) -> Vec<u8> {
        let mut out = vec![b'['];
        if rng.chance(30) {
            out.push(b'^');
        }
        if rng.chance(15) {
            out.push(b']');
        }
        for _ in 0..1 + rng.below(3) {
            match rng.below(6) {
                0 => out.extend_from_slice(rng.pick::<&[u8]>(&[
                    &b"a-c"[..],
                    b"\0-\x10",
                    b"\x80-\xff",
                    b"0-9",
                    b"c-a",
                    b"a-",
                ])),
                1 => out.extend_from_slice(&[b'%', *rng.pick(CLASSES)]),
                2 => out.extend_from_slice(&[b'%', *rng.pick(ESCAPED)]),
                3 => out.push(b'-'),
                _ => out.push(*rng.pick(LITERALS)),
            }
        }
        out.push(b']');
        out
    }

    fn gen_atom(rng: &mut Rng) -> Vec<u8> {
        match rng.below(10) {
            0..=3 => vec![*rng.pick(LITERALS)],
            4 => vec![b'.'],
            5 | 6 => vec![b'%', *rng.pick(CLASSES)],
            7 => vec![b'%', *rng.pick(ESCAPED)],
            _ => gen_set(rng),
        }
    }

    fn gen_items(rng: &mut Rng, depth: usize, out: &mut Vec<u8>) {
        for _ in 0..rng.below(4 - depth.min(2)) + usize::from(depth == 0) {
            gen_item(rng, depth, out);
        }
    }

    fn gen_item(rng: &mut Rng, depth: usize, out: &mut Vec<u8>) {
        match rng.below(20) {
            0..=10 => {
                out.extend(gen_atom(rng));
                if rng.chance(40) {
                    out.push(*rng.pick(b"*+-?"));
                }
            }
            11 | 12 if depth < 3 => {
                out.push(b'(');
                gen_items(rng, depth + 1, out);
                out.push(b')');
            }
            13 => out.extend_from_slice(b"()"),
            14 => out.extend_from_slice(&[b'%', *rng.pick(b"1112230")]),
            15 => {
                out.extend_from_slice(b"%b");
                out.extend_from_slice(rng.pick::<&[u8]>(&[
                    &b"()"[..],
                    b"ab",
                    b"aa",
                    b"\0x",
                    b")(",
                ]));
            }
            16 => {
                out.extend_from_slice(b"%f");
                out.extend(gen_set(rng));
            }
            17 => out.push(*rng.pick(b"$^")),
            18 => out.extend_from_slice(rng.pick::<&[u8]>(&[
                &b"%"[..],
                b"[a",
                b"[",
                b"(",
                b")",
                b"%f",
                b"%fa",
                b"%b",
                b"%ba",
                b"[^",
                b"[%",
                b"%g",
            ])),
            _ => out.extend(gen_atom(rng)),
        }
    }

    fn gen_pattern(rng: &mut Rng) -> Vec<u8> {
        let mut p = Vec::new();
        if rng.chance(20) {
            p.push(b'^');
        }
        if !rng.chance(5) {
            gen_items(rng, 0, &mut p);
        }
        if rng.chance(15) {
            p.push(b'$');
        }
        p
    }

    fn gen_subject(rng: &mut Rng) -> Vec<u8> {
        let len = rng.below(15);
        (0..len).map(|_| *rng.pick(SUBJECT_BYTES)).collect()
    }

    fn gen_init(rng: &mut Rng, len: usize) -> Option<i64> {
        if rng.chance(50) {
            return None;
        }
        let span = len as i64 + 3;
        Some(rng.below(2 * span as usize + 1) as i64 - span)
    }

    fn gen_case(rng: &mut Rng) -> Case {
        let p = gen_pattern(rng);
        let s = if rng.chance(40) && !p.is_empty() {
            // Bytes drawn from the pattern make matches likelier.
            (0..rng.below(15)).map(|_| *rng.pick(&p)).collect()
        } else {
            gen_subject(rng)
        };
        let init = gen_init(rng, s.len());
        match rng.below(4) {
            0 => Case::Find {
                s,
                p,
                init,
                plain: rng.chance(15),
            },
            1 => Case::Match { s, p, init },
            2 => Case::Gmatch { s, p, init },
            _ => {
                let mut repl = Vec::new();
                for _ in 0..rng.below(4) {
                    repl.extend_from_slice(rng.pick::<&[u8]>(&[
                        &b"x"[..],
                        b"%%",
                        b"%0",
                        b"%1",
                        b"%2",
                        b"%3",
                        b"%9",
                        b"%a",
                        b"%",
                        b"\0",
                        b"-",
                    ]));
                }
                let max = if rng.chance(60) {
                    None
                } else {
                    Some(*rng.pick(&[-1, 0, 1, 2, 3]))
                };
                Case::Gsub { s, p, repl, max }
            }
        }
    }

    fn rep(piece: &[u8], n: usize) -> Vec<u8> {
        piece.repeat(n)
    }

    /// Hand-picked cases: error classes, capture and depth limits, frontier
    /// edges, gmatch's literal '^'.
    fn fixed_cases() -> Vec<Case> {
        let mut out = Vec::new();
        let find = |s: Vec<u8>, p: Vec<u8>| Case::Find {
            s,
            p,
            init: None,
            plain: false,
        };
        let mat = |s: Vec<u8>, p: Vec<u8>| Case::Match { s, p, init: None };
        for k in [31, 32, 33, 40] {
            out.push(find(b"abc".to_vec(), rep(b"()", k)));
            out.push(mat(rep(b"a", 40), rep(b"(a)", k)));
            out.push(mat(
                rep(b"a", 40),
                [rep(b"(", k), b"a".to_vec(), rep(b")", k)].concat(),
            ));
        }
        for k in 190..=210 {
            out.push(mat(rep(b"a", k), rep(b"a?", k)));
            out.push(mat(rep(b"a", k), rep(b"a-", k)));
            out.push(mat(b"a".to_vec(), [rep(b".-", k), b"x".to_vec()].concat()));
            out.push(mat(rep(b"a", k), rep(b"%w*", k)));
            if k >= 200 {
                // Below 200 this backtracks exponentially.
                out.push(mat(rep(b"a", k), [rep(b"a?", k), b"b".to_vec()].concat()));
            }
            out.push(mat(
                rep(b"a", 64),
                [b"(".to_vec(), rep(b"a?", k), b")".to_vec()].concat(),
            ));
        }
        for (s, p) in [
            (&b"THE (quick) fox"[..], &b"%f[%a]%a+"[..]),
            (b"THE (quick) fox", b"%f[%A]"),
            (b"abc", b"%f[%z]"),
            (b"abc", b"%f[%Z]"),
            (b"", b"%f[%z]"),
            (b"", b"%f[^%z]"),
            (b"a\0b", b"%f[%z]."),
            (b"x(a(b)c)y", b"%b()"),
            (b"((", b"%b()"),
            (b"", b"%b\0\0"),
            (b"", b""),
            (b"abc", b""),
            (b"abc", b"^$"),
            (b"", b"^$"),
            (b"a$b", b"$b"),
            (b"a$b", b"a$b"),
            (b"^a^a", b"^a"),
            (b"abc", b"%"),
            (b"abc", b"a%"),
            (b"b", b"a%"),
            (b"abc", b"[a"),
            (b"abc", b"[]"),
            (b"abc", b"[^]"),
            (b"abc", b"%b"),
            (b"abc", b"%ba"),
            (b"abc", b"%f"),
            (b"abc", b"%fa"),
            (b"abc", b"(a%1)"),
            (b"abc", b"%0"),
            (b"abc", b"(a)%2"),
            (b"abc", b")"),
            (b"abc", b"(()"),
            (b"abc", b"(a"),
            (b"aa", b"(a)%1"),
            (b"aa", b"()a%1"),
            (b"\xff\x80", b"[\x80-\xff]+"),
            (b"\xff\x80", b"%W+"),
            (b"\xe9A", b"%a+"),
            (b"a]b", b"[]]"),
            (b"a]b", b"[^]]+"),
            (b"a-b", b"[a-]+"),
            (b"a%]b", b"[%]]"),
        ] {
            for k in 0..4 {
                let (s, p) = (s.to_vec(), p.to_vec());
                out.push(match k {
                    0 => find(s, p),
                    1 => mat(s, p),
                    2 => Case::Gmatch { s, p, init: None },
                    _ => Case::Gsub {
                        s,
                        p,
                        repl: b"<%0>".to_vec(),
                        max: None,
                    },
                });
            }
        }
        for repl in [&b"%"[..], b"%%", b"%1", b"%2", b"%a", b"x%0y%1", b"%\0"] {
            out.push(Case::Gsub {
                s: b"hello world".to_vec(),
                p: b"(o)".to_vec(),
                repl: repl.to_vec(),
                max: None,
            });
            out.push(Case::Gsub {
                s: b"hello world".to_vec(),
                p: b"()o".to_vec(),
                repl: repl.to_vec(),
                max: None,
            });
            out.push(Case::Gsub {
                s: b"hello world".to_vec(),
                p: b"o".to_vec(),
                repl: repl.to_vec(),
                max: None,
            });
            out.push(Case::Gsub {
                s: b"hello world".to_vec(),
                p: b"(o".to_vec(),
                repl: repl.to_vec(),
                max: None,
            });
        }
        out
    }

    #[test]
    fn resumption_is_schedule_independent() {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let mut cases = fixed_cases();
        cases.extend((0..1500).map(|_| gen_case(&mut rng)));
        for case in &cases {
            let mut huge = Fuel::new(i64::MAX / 4, false);
            let expected = eval(case, &mut huge).unwrap();
            for chunk in [1, 7] {
                let mut fuel = Fuel::new(chunk, true);
                let got = eval(case, &mut fuel).unwrap();
                assert_eq!(got, expected, "{case:?} chunk {chunk}");
                assert_eq!(fuel.used(), huge.used(), "{case:?} chunk {chunk}");
            }
        }
    }

    fn fuzz_one<M: Machine + std::fmt::Debug>(words: &[u64], s: &[u8], p: &[u8]) {
        let mut slice = words;
        if let Some(mut m) = M::dec(&mut slice, len32(s), len32(p)) {
            let mut budget = 1_000_000;
            let _ = m.go(s, p, &mut budget);
            assert!(m.depth() <= MAX_DEPTH as usize);
        }
    }

    fn fuzz_all(words: &[u64], s: &[u8], p: &[u8]) {
        fuzz_one::<Search>(words, s, p);
        fuzz_one::<Gmatch>(words, s, p);
        fuzz_one::<Gsub>(words, s, p);
        fuzz_one::<PlainSearch>(words, s, p);
    }

    #[test]
    fn decode_rejects_or_runs_safely() {
        let mut rng = Rng(0xdead_beef_cafe_f00d);
        let s = b"aab(c)ab\0\xffaa".to_vec();
        let p = b"(a*)%b()(.-)%1%f[%w]()".to_vec();
        for _ in 0..20000 {
            let len = rng.below(40);
            let words: Vec<u64> = (0..len)
                .map(|_| match rng.below(3) {
                    0 => rng.next(),
                    1 => rng.next() % 4,
                    _ => rng.next() % 24,
                })
                .collect();
            fuzz_all(&words, &s, &p);
        }
        // Mutations of real pending states.
        let mut m = Search::new(&p, 0);
        let mut states = Vec::new();
        let mut budget = 0;
        while m.run(&s, &p, &mut budget) == SearchOutcome::Pending {
            let mut words = Vec::new();
            m.encode(&mut words);
            states.push(words);
            budget += 1;
        }
        for words in &states {
            for _ in 0..200 {
                let mut w = words.clone();
                let i = rng.below(w.len());
                w[i] = match rng.below(4) {
                    0 => rng.next(),
                    1 => w[i].wrapping_add(1),
                    2 => w[i].wrapping_sub(1),
                    _ => rng.next() % 32,
                };
                fuzz_all(&w, &s, &p);
            }
        }
    }

    fn find(s: &[u8], p: &[u8]) -> String {
        eval_plain(&Case::Find {
            s: s.to_vec(),
            p: p.to_vec(),
            init: None,
            plain: false,
        })
    }

    fn mat(s: &[u8], p: &[u8]) -> String {
        eval_plain(&Case::Match {
            s: s.to_vec(),
            p: p.to_vec(),
            init: None,
        })
    }

    fn gsub(s: &[u8], p: &[u8], repl: &[u8], max: Option<i64>) -> String {
        eval_plain(&Case::Gsub {
            s: s.to_vec(),
            p: p.to_vec(),
            repl: repl.to_vec(),
            max,
        })
    }

    #[test]
    fn classes_follow_the_c_locale() {
        assert!(match_class(b'a', b'a') && !match_class(b'a', b'A'));
        for c in 128..=255u8 {
            for cl in CLASSES
                .iter()
                .filter(|c| c.is_ascii_lowercase() && **c != b'z')
            {
                assert!(!match_class(c, *cl), "{c} {cl}");
            }
        }
        assert!(match_class(b'\x0b', b's') && match_class(b'~', b'p') && !match_class(b' ', b'g'));
        assert!(match_class(0, b'z') && match_class(b'.', b'.'));
        assert_eq!(
            (to_lower(b'A'), to_upper(b'z'), to_lower(0xc9)),
            (b'a', b'Z', 0xc9)
        );
    }

    #[test]
    fn find_reports_positions_and_captures() {
        assert_eq!(find(b"hello world", b"o w"), "O:p5,p7");
        assert_eq!(find(b"hello world", b"(l+)()"), "O:p3,p4,6c6c,p5");
        assert_eq!(find(b"hello", b"^l"), "O:nil");
        assert_eq!(mat(b"hello", b".-(l+)(.*)"), "O:6c6c,6f");
        assert_eq!(mat(b"key = val", b"(%w+)%s*=%s*(%w+)"), "O:6b6579,76616c");
    }

    #[test]
    fn back_references_balance_and_frontier() {
        assert_eq!(mat(b"xabab", b"(ab)%1"), "O:6162");
        assert_eq!(mat(b"f(a(b)c)d", b"%b()"), "O:28612862296329");
        assert_eq!(mat(b"THE (quick) fox", b"%f[%a]%a+$"), "O:666f78");
        assert_eq!(find(b"abc", b"%f[%z]"), "O:p4,p3");
    }

    #[test]
    fn errors_are_raised_lazily() {
        assert_eq!(find(b"b", b"a%"), "O:nil");
        assert_eq!(find(b"a", b"a%"), "E:malformed pattern (ends with '%')");
        assert_eq!(find(b"a", b"[a"), "E:malformed pattern (missing ']')");
        assert_eq!(
            find(b"a", b"%b"),
            "E:malformed pattern (missing arguments to '%b')"
        );
        assert_eq!(find(b"a", b"%fa"), "E:missing '[' after '%f' in pattern");
        assert_eq!(find(b"a", b"%1"), "E:invalid capture index %1");
        assert_eq!(mat(b"a", b")"), "E:invalid pattern capture");
        assert_eq!(find(b"a", b"(a"), "E:unfinished capture");
        assert_eq!(find(b"a", &rep(b"()", 33)), "E:too many captures");
    }

    #[test]
    fn depth_bound_matches_maxccalls() {
        // Each matched `a?` holds one frame; the whole pattern needs k + 1 calls.
        assert_eq!(
            mat(&rep(b"a", 199), &rep(b"a?", 199)),
            format!("O:{}", hex(&rep(b"a", 199)))
        );
        assert_eq!(
            mat(&rep(b"a", 200), &rep(b"a?", 200)),
            "E:pattern too complex"
        );
    }

    #[test]
    fn gmatch_treats_caret_literally_and_stays_exhausted() {
        let case = Case::Gmatch {
            s: b"^a^ab".to_vec(),
            p: b"^a".to_vec(),
            init: None,
        };
        assert_eq!(eval_plain(&case), "O:5e61;5e61;END");
        let mut g = Gmatch::new(0);
        let mut budget = 1000;
        assert_eq!(g.run(b"x", b"y", &mut budget), SearchOutcome::NotFound);
        g.begin_call();
        assert_eq!(g.run(b"x", b"y", &mut budget), SearchOutcome::NotFound);
    }

    #[test]
    fn gsub_counts_and_honours_anchor_and_max() {
        assert_eq!(
            gsub(b"aaa", b"^a", b"x", None),
            format!("O:{},p1", hex(b"xaa"))
        );
        assert_eq!(
            gsub(b"abc", b"", b"-", None),
            format!("O:{},p4", hex(b"-a-b-c-"))
        );
        assert_eq!(
            gsub(b"abc", b"%w", b"%0%0", Some(2)),
            format!("O:{},p2", hex(b"aabbc"))
        );
        assert_eq!(
            gsub(b"abc", b"b*", b"X", None),
            format!("O:{},p3", hex(b"XaXcX"))
        );
    }

    #[test]
    fn replacement_expansion() {
        assert_eq!(
            gsub(b"ab", b"(a)()", b"[%1%2%%]", None),
            format!("O:{},p1", hex(b"[a2%]b"))
        );
        assert_eq!(
            gsub(b"ab", b"a", b"<%1>", None),
            format!("O:{},p1", hex(b"<a>b"))
        );
        assert_eq!(gsub(b"ab", b"a", b"%2", None), "E:invalid capture index %2");
        assert_eq!(
            gsub(b"ab", b"a", b"%", None),
            "E:invalid use of '%' in replacement string"
        );
        assert_eq!(
            gsub(b"ab", b"x", b"%", None),
            format!("O:{},p0", hex(b"ab"))
        );
    }

    #[test]
    fn plain_search_and_specials() {
        assert!(no_specials(b"abc\0def") && !no_specials(b"abc\0d.f"));
        let plain = |s: &[u8], p: &[u8], init| {
            eval_plain(&Case::Find {
                s: s.to_vec(),
                p: p.to_vec(),
                init,
                plain: true,
            })
        };
        assert_eq!(plain(b"a.b.c", b".c", None), "O:p4,p5");
        assert_eq!(plain(b"abc", b"", Some(10)), "O:nil");
        assert_eq!(plain(b"abc", b"", Some(4)), "O:p4,p3");
        assert_eq!(plain(b"a\0b\0c", b"\0c", None), "O:p4,p5");
    }
}
