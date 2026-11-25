//! Traditional → Simplified normalisation used **only** in the matching layer.
//!
//! LRCLIB and Hong Kong/Taiwan submissions routinely carry traditional titles
//! (`我懷念的`) while SMTC and the mainland sources carry simplified ones
//! (`我怀念的`): the bigram overlap would be zero and the correct record would be
//! thrown out as noise. Content is never rewritten — this runs on comparison
//! strings only.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::data::t2s::T2S_PAIRS;

static T2S: LazyLock<HashMap<char, char>> =
    LazyLock::new(|| T2S_PAIRS.iter().copied().collect());

/// Per-character simplification; characters with no entry pass through.
pub fn to_simplified(text: &str) -> String {
    text.chars()
        .map(|c| T2S.get(&c).copied().unwrap_or(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::to_simplified;

    #[test]
    fn converts_traditional_and_leaves_the_rest() {
        assert_eq!(to_simplified("我懷念的"), "我怀念的");
        assert_eq!(to_simplified("Lust For Life"), "Lust For Life");
        assert_eq!(to_simplified(""), "");
    }
}
