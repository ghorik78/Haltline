//! Breakpoint helpers that live in the adapter rather than in GDB:
//! hit-count conditions and log point messages.

use anyhow::{bail, Result};

/// A DAP `hitCondition`.
///
/// * `N` or `>=N` — break from the N-th hit on (GDB's ignore-count semantics)
/// * `==N` — break on the N-th hit only
/// * `>N`, `<N`, `<=N` — compare the hit count
/// * `%N` — break on every N-th hit
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitCondition {
    AtLeast(u64),
    Equal(u64),
    Greater(u64),
    Less(u64),
    LessEqual(u64),
    EveryNth(u64),
}

impl HitCondition {
    pub fn parse(text: &str) -> Result<Option<Self>> {
        let t = text.trim();
        if t.is_empty() {
            return Ok(None);
        }
        let (ctor, number): (fn(u64) -> HitCondition, &str) = if let Some(n) = t.strip_prefix(">=")
        {
            (HitCondition::AtLeast, n)
        } else if let Some(n) = t.strip_prefix("==") {
            (HitCondition::Equal, n)
        } else if let Some(n) = t.strip_prefix("<=") {
            (HitCondition::LessEqual, n)
        } else if let Some(n) = t.strip_prefix('>') {
            (HitCondition::Greater, n)
        } else if let Some(n) = t.strip_prefix('<') {
            (HitCondition::Less, n)
        } else if let Some(n) = t.strip_prefix('%') {
            (HitCondition::EveryNth, n)
        } else if let Some(n) = t.strip_prefix('=') {
            (HitCondition::Equal, n)
        } else {
            (HitCondition::AtLeast, t)
        };
        let Some(n) = crate::util::parse_u64(number) else {
            bail!("invalid hit condition `{t}` (use N, ==N, >N, >=N, <N, <=N or %N)");
        };
        if matches!(ctor(0), HitCondition::EveryNth(_)) && n == 0 {
            bail!("`%0` is not a valid hit condition");
        }
        Ok(Some(ctor(n)))
    }

    /// Whether the debugger should stop on hit number `hits` (1-based).
    pub fn matches(self, hits: u64) -> bool {
        match self {
            HitCondition::AtLeast(n) => hits >= n,
            HitCondition::Equal(n) => hits == n,
            HitCondition::Greater(n) => hits > n,
            HitCondition::Less(n) => hits < n,
            HitCondition::LessEqual(n) => hits <= n,
            HitCondition::EveryNth(n) => hits.is_multiple_of(n),
        }
    }
}

/// A piece of a log point message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogPart {
    Text(String),
    /// An expression written as `{expr}`.
    Expr(String),
}

/// Parses `"x = {x}, sum = {a + b}"`; `{{` and `}}` are literal braces.
pub fn parse_log_message(message: &str) -> Vec<LogPart> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                text.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                text.push('}');
            }
            '{' => {
                let mut expr = String::new();
                let mut depth = 1;
                for c in chars.by_ref() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    expr.push(c);
                }
                if depth != 0 {
                    // Unterminated: keep the text verbatim.
                    text.push('{');
                    text.push_str(&expr);
                    continue;
                }
                if !text.is_empty() {
                    parts.push(LogPart::Text(std::mem::take(&mut text)));
                }
                parts.push(LogPart::Expr(expr.trim().to_string()));
            }
            c => text.push(c),
        }
    }
    if !text.is_empty() {
        parts.push(LogPart::Text(text));
    }
    parts
}

/// Fills a parsed message using `eval` for expressions.
pub fn format_log_message(parts: &[LogPart], mut eval: impl FnMut(&str) -> String) -> String {
    parts
        .iter()
        .map(|p| match p {
            LogPart::Text(t) => t.clone(),
            LogPart::Expr(e) => eval(e),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hit_conditions() {
        assert_eq!(HitCondition::parse("").unwrap(), None);
        assert_eq!(
            HitCondition::parse("5").unwrap(),
            Some(HitCondition::AtLeast(5))
        );
        assert_eq!(
            HitCondition::parse(">= 5").unwrap(),
            Some(HitCondition::AtLeast(5))
        );
        assert_eq!(
            HitCondition::parse("==3").unwrap(),
            Some(HitCondition::Equal(3))
        );
        assert_eq!(
            HitCondition::parse("=3").unwrap(),
            Some(HitCondition::Equal(3))
        );
        assert_eq!(
            HitCondition::parse(">2").unwrap(),
            Some(HitCondition::Greater(2))
        );
        assert_eq!(
            HitCondition::parse("<2").unwrap(),
            Some(HitCondition::Less(2))
        );
        assert_eq!(
            HitCondition::parse("<=2").unwrap(),
            Some(HitCondition::LessEqual(2))
        );
        assert_eq!(
            HitCondition::parse("%0x10").unwrap(),
            Some(HitCondition::EveryNth(16))
        );
        assert!(HitCondition::parse("%0").is_err());
        assert!(HitCondition::parse("often").is_err());
    }

    #[test]
    fn evaluates_hit_conditions() {
        let hits = |c: HitCondition| (1..=10).filter(|h| c.matches(*h)).collect::<Vec<_>>();
        assert_eq!(hits(HitCondition::AtLeast(8)), [8, 9, 10]);
        assert_eq!(hits(HitCondition::Equal(3)), [3]);
        assert_eq!(hits(HitCondition::Greater(8)), [9, 10]);
        assert_eq!(hits(HitCondition::Less(3)), [1, 2]);
        assert_eq!(hits(HitCondition::LessEqual(2)), [1, 2]);
        assert_eq!(hits(HitCondition::EveryNth(4)), [4, 8]);
    }

    #[test]
    fn parses_log_messages() {
        assert_eq!(
            parse_log_message("i = {i}, sum = {a + b}!"),
            vec![
                LogPart::Text("i = ".into()),
                LogPart::Expr("i".into()),
                LogPart::Text(", sum = ".into()),
                LogPart::Expr("a + b".into()),
                LogPart::Text("!".into()),
            ]
        );
        assert_eq!(
            parse_log_message("{{literal}} {x}"),
            vec![
                LogPart::Text("{literal} ".into()),
                LogPart::Expr("x".into())
            ]
        );
        assert_eq!(
            parse_log_message("p = {(struct s){0}.a}"),
            vec![
                LogPart::Text("p = ".into()),
                LogPart::Expr("(struct s){0}.a".into())
            ]
        );
        assert_eq!(
            parse_log_message("open {x"),
            vec![LogPart::Text("open {x".into())]
        );
        assert!(parse_log_message("").is_empty());
    }

    #[test]
    fn formats_log_messages() {
        let parts = parse_log_message("a={a} b={b}");
        let text = format_log_message(&parts, |e| format!("<{e}>"));
        assert_eq!(text, "a=<a> b=<b>");
    }
}
