//! AMLL's word chunking: what a "word element" actually is.
//!
//! The player never animates a raw word. `rebuildElement` calls
//! `chunkAndSplitLyricWords` first, and `buildWord` then treats each returned
//! entry as one element: a *chunk*. Adjacent non-space, non-ruby, non-CJK atoms
//! merge into a single chunk, and everything else (whitespace, a CJK character,
//! a ruby word) stands alone. That distinction is not cosmetic -- it decides
//! which text carries the long-note emphasis and over what duration:
//!
//! ```text
//! die-happy line 12:  Ah-ooh ah-ooh
//! raw words:          Ah- 72626..72874   ooh 72874..75570   ...
//! chunks:             [Ah-, ooh] -> "Ah-ooh" 72626..75570   (4 216 ms)
//! ```
//!
//! A port that emphasises the single word `ooh` would animate 3 904 ms of a
//! 4 216 ms element, start the glow 312 ms late and split the arch in two.
//! [`chunk_and_split_lyric_words`] exists so the emphasis plan is built from the
//! same element the reference builds; it is pinned by
//! `native/tests/fixtures/oracle/emphasis/amll-chunks.json`.

use crate::lyrics::AmllWord;
use serde::Serialize;

/// One element of a line, as the player builds it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum WordChunk {
    /// A single atom: whitespace, one CJK character, or a ruby word.
    Atom(ChunkAtom),
    /// Several atoms merged into one element.
    Group(Vec<ChunkAtom>),
}

impl WordChunk {
    pub fn atoms(&self) -> &[ChunkAtom] {
        match self {
            Self::Atom(atom) => std::slice::from_ref(atom),
            Self::Group(atoms) => atoms,
        }
    }

    /// Whether adjacent atoms were merged. The reference pushes a bare atom for
    /// a group of one, so this is exactly "was this a group in the source".
    pub fn is_grouped(&self) -> bool {
        matches!(self, Self::Group(_))
    }

    /// The element's text, start and end: the values `buildWord` uses for the
    /// emphasis duration and delay, and the text the word element displays.
    pub fn merged(&self) -> ChunkAtom {
        let atoms = self.atoms();
        ChunkAtom {
            word: atoms.iter().map(|atom| atom.word.as_str()).collect(),
            start_time: atoms
                .iter()
                .fold(f64::INFINITY, |acc, atom| acc.min(atom.start_time)),
            end_time: atoms
                .iter()
                .fold(f64::NEG_INFINITY, |acc, atom| acc.max(atom.end_time)),
        }
    }
}

/// One timed piece inside a chunk.
///
/// Times are `f64` because the reference interpolates them: a word with
/// whitespace is split per part and every part gets a share of the word's
/// duration proportional to its length, which lands on fractions. Rounding here
/// would move a chunk boundary by up to a millisecond and the emphasis delay
/// with it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkAtom {
    pub word: String,
    pub start_time: f64,
    pub end_time: f64,
}

/// AMLL's `isCJK`, which is **not** the same predicate the script splitter uses.
///
/// The reference's chunker tests `/^[\p{Unified_Ideograph}\u0800-\u9FFC]+$/u`
/// and every character must satisfy it. The `\u0800-\u9FFC` band is the part
/// people misread: it is a plain code-point range, so it admits kana
/// (`\u3040`), Hangul jamo (`\u1100`), and the CJK symbols block along with the
/// ideographs. Only the tails are excluded -- Hangul syllables (`\uAC00`) and
/// fullwidth forms (`\uFF00`) sit outside it -- so a Korean or fullwidth word
/// merges with its neighbours while a Japanese one does not.
///
/// [`crate::lyrics::is_cjk`] answers a different question: which face draws the
/// glyph. It counts Hangul and fullwidth forms as CJK for font selection. The
/// two predicates disagree by design and must not be shared -- verified against
/// the reference with `.tmp` probes over kana, Hangul, ideographs and
/// fullwidth text.
pub fn is_chunker_cjk(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|ch| {
            let code = ch as u32;
            (0x0800..=0x9FFC).contains(&code)
                // \p{Unified_Ideograph} outside the BMP. The property's blocks
                // are 2A6DF/2B73F/2B81F/2CEAF/2EBEF/3134F/323AF; the union is
                // contiguous enough that one range is exact for them.
                || (0x20000..=0x323AF).contains(&code)
        })
}

/// UTF-16 code-unit length, which is what the reference's `.length` counts.
///
/// `partRealLen` feeds the per-part time interpolation, so a non-BMP character
/// must count as two units or the atom's timestamps drift.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Is this part whitespace-only? The reference reads `!part.trim()`.
fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// `chunkAndSplitLyricWords` from the shipped bundle, 1:1.
///
/// Two behaviours are easy to get wrong and are called out where they happen:
/// the whitespace split *keeps* the separators as their own atoms, and a
/// multi-character CJK word is exploded into one atom per character with
/// interpolated times.
pub fn chunk_and_split_lyric_words(words: &[AmllWord]) -> Vec<WordChunk> {
    let mut result: Vec<WordChunk> = Vec::new();
    let mut group: Vec<ChunkAtom> = Vec::new();

    fn flush(result: &mut Vec<WordChunk>, group: &mut Vec<ChunkAtom>) {
        match group.len() {
            0 => {}
            1 => result.push(WordChunk::Atom(group.remove(0))),
            _ => result.push(WordChunk::Group(std::mem::take(group))),
        }
        group.clear();
    }

    fn process_atom(
        result: &mut Vec<WordChunk>,
        group: &mut Vec<ChunkAtom>,
        atom: ChunkAtom,
    ) {
        let blank = is_blank(&atom.word);
        let cjk = is_chunker_cjk(&atom.word);
        if !blank && !cjk {
            group.push(atom);
        } else {
            flush(result, group);
            result.push(WordChunk::Atom(atom));
        }
    }

    for word in words {
        let start = word.start_time as f64;
        let end = word.end_time as f64;
        let content = word.word.trim();

        if content.is_empty() {
            // A whitespace-only word still becomes an atom (it is rendered as a
            // text node), it just never joins a group.
            process_atom(
                &mut result,
                &mut group,
                ChunkAtom {
                    word: word.word.clone(),
                    start_time: start,
                    end_time: end,
                },
            );
            continue;
        }

        // Split on whitespace runs, keeping the runs: `"Ah- ooh"` becomes
        // `["Ah-", " ", "ooh"]`, and the space is its own atom with zero width
        // in time. Dropping it would concatenate the sung text.
        let parts: Vec<&str> = split_keeping_whitespace(&word.word);

        // Interpolation denominator ignores whitespace, and never divides by
        // zero: the reference writes `.length || 1`.
        let total_length = word.word.chars().filter(|ch| !ch.is_whitespace()).count().max(1);
        let time_per_unit = (end - start) / total_length as f64;

        let mut current_offset = 0usize;
        for part in parts {
            if is_blank(part) {
                let at = start + current_offset as f64 * time_per_unit;
                process_atom(
                    &mut result,
                    &mut group,
                    ChunkAtom {
                        word: part.to_string(),
                        start_time: at,
                        end_time: at,
                    },
                );
                continue;
            }

            if is_chunker_cjk(part) && utf16_len(part) > 1 {
                // One atom per character, each a unit long. `part.split("")` in
                // the reference yields UTF-16 units; ideographs are all BMP, so
                // chars and code units agree here.
                for ch in part.chars() {
                    let at = start + current_offset as f64 * time_per_unit;
                    process_atom(
                        &mut result,
                        &mut group,
                        ChunkAtom {
                            word: ch.to_string(),
                            start_time: at,
                            end_time: at + time_per_unit,
                        },
                    );
                    current_offset += 1;
                }
            } else {
                let part_length = utf16_len(part);
                let at = start + current_offset as f64 * time_per_unit;
                let duration = part_length as f64 * time_per_unit;
                process_atom(
                    &mut result,
                    &mut group,
                    ChunkAtom {
                        word: part.to_string(),
                        start_time: at,
                        end_time: at + duration,
                    },
                );
                current_offset += part_length;
            }
        }
    }

    flush(&mut result, &mut group);
    result
}

/// `String.prototype.split(/(\s+)/)` with empty pieces dropped.
fn split_keeping_whitespace(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_space: Option<bool> = None;
    for (byte, ch) in text.char_indices() {
        let space = ch.is_whitespace();
        match in_space {
            None => in_space = Some(space),
            Some(previous) if previous == space => {}
            Some(_) => {
                parts.push(&text[start..byte]);
                start = byte;
                in_space = Some(space);
            }
        }
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: i64, end: i64) -> AmllWord {
        AmllWord {
            start_time: start,
            end_time: end,
            word: text.to_string(),
        }
    }

    #[test]
    fn adjacent_latin_words_merge_into_one_element() {
        let chunks = chunk_and_split_lyric_words(&[
            word("Ah-", 72626, 72874),
            word("ooh", 72874, 75570),
        ]);
        assert_eq!(chunks.len(), 1);
        let merged = chunks[0].merged();
        assert_eq!(merged.word, "Ah-ooh");
        assert_eq!(merged.start_time, 72626.0);
        assert_eq!(merged.end_time, 75570.0);
    }

    #[test]
    fn whitespace_stays_its_own_atom_and_never_groups() {
        let chunks =
            chunk_and_split_lyric_words(&[word("Ah-", 0, 100), word(" ", 100, 100), word("ooh", 100, 500)]);
        assert_eq!(chunks.len(), 3);
        assert!(!chunks[1].is_grouped());
        assert_eq!(chunks[1].merged().word, " ");
        assert_eq!(chunks[1].merged().start_time, 100.0);
    }

    #[test]
    fn a_cjk_word_is_split_into_one_atom_per_character() {
        let chunks = chunk_and_split_lyric_words(&[word("身边", 1000, 3000)]);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].merged().word, "身");
        assert_eq!(chunks[1].merged().word, "边");
        assert_eq!(chunks[0].merged().start_time, 1000.0);
        assert_eq!(chunks[0].merged().end_time, 2000.0);
        assert_eq!(chunks[1].merged().end_time, 3000.0);
    }

    #[test]
    fn a_part_inside_a_word_gets_its_share_of_the_duration() {
        // "ab cd": two non-space parts of length 2 each, total 4 units.
        let chunks = chunk_and_split_lyric_words(&[word("ab cd", 0, 400)]);
        let texts: Vec<String> = chunks.iter().map(|c| c.merged().word).collect();
        assert_eq!(texts, ["ab", " ", "cd"]);
        assert_eq!(chunks[0].merged().end_time, 200.0);
        assert_eq!(chunks[1].merged().start_time, 200.0);
        assert_eq!(chunks[2].merged().start_time, 200.0);
        assert_eq!(chunks[2].merged().end_time, 400.0);
    }

    #[test]
    fn the_chunker_predicate_admits_kana_but_rejects_hangul_syllables() {
        // Measured against the reference regex: `\u0800-\u9FFC` is a plain
        // code-point band, so kana falls inside it and is treated as CJK --
        // a Japanese word is split per character, not merged. Hangul syllables
        // at U+AC00 do not, so a Korean word merges. Guessing "kana is not an
        // ideograph, therefore not CJK" produces the opposite grouping.
        assert!(is_chunker_cjk("かな"));
        assert!(is_chunker_cjk("あ"));
        assert!(is_chunker_cjk("身"));
        assert!(!is_chunker_cjk("한"));
        assert!(!is_chunker_cjk("Ａ"));
        assert!(!is_chunker_cjk("abc"));

        let kana = chunk_and_split_lyric_words(&[word("かな", 0, 200)]);
        assert_eq!(kana.len(), 2, "kana is split per character");
        assert_eq!(kana[0].merged().word, "か");

        let hangul = chunk_and_split_lyric_words(&[word("한글", 0, 200)]);
        assert_eq!(hangul.len(), 1, "hangul syllables merge into one chunk");
    }

    #[test]
    fn a_long_whitespace_run_is_one_atom() {
        let chunks = chunk_and_split_lyric_words(&[word("a   b", 0, 400)]);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[1].merged().word, "   ");
    }
}
