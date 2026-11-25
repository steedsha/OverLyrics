//! Hand-confirmed official English ↔ Chinese title pairs.
//!
//! Without these, a cross-language correct match scores 30 % on the string
//! layer and gets filtered out by the identity gate. The table is deliberately
//! tiny and hand-curated — it is not a translation service.

use crate::zhconv::to_simplified;

/// Official English title ↔ official Chinese title.
pub const PAIRS: &[(&str, &str)] = &[
    ("Green Light", "绿光"),
    ("Kite", "风筝"),
    ("Practice", "练习"),
    ("I Am Fine", "我不难过"),
    ("Step Aside", "搁浅"),
    ("Abscondence", "逃亡"),
    ("That's the Way I Am", "我是我"),
];

/// Lowercase, collapse whitespace, simplify.
fn norm(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_space = false;
    for c in lowered.chars() {
        if c.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    let trimmed = out.trim_end().to_string();
    to_simplified(&trimmed)
}

/// True when `a` and `b` are a confirmed pair (in either direction).
pub fn bridged(a: &str, b: &str) -> bool {
    let na = norm(a);
    let nb = norm(b);
    if na.is_empty() || nb.is_empty() || na == nb {
        return false;
    }
    PAIRS.iter().any(|(en, zh)| {
        let (e, z) = (norm(en), norm(zh));
        (na == e && nb == z) || (na == z && nb == e)
    })
}

/// Number of curated pairs, surfaced by the QA goldens.
pub fn size() -> usize {
    PAIRS.len()
}
