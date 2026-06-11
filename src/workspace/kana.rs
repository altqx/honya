//! Kana → romaji transliteration for loose name matching. The Orchestrator is
//! told to look characters up "by reading", which arrives as hiragana/katakana —
//! this bridges those queries to the stored `romaji` field. Hepburn-ish, lossy on
//! purpose: output is compared through `norm_romaji`-style folding, not displayed.

/// Transliterate hiragana/katakana in `s` to lowercase romaji; non-kana chars pass
/// through lowercased (so mixed strings still produce a usable key). Returns `None`
/// when `s` contains no kana at all — no extra match key is needed then.
pub(crate) fn kana_to_romaji(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().map(katakana_to_hiragana).collect();
    if !chars.iter().any(|&c| is_kana(c)) {
        return None;
    }

    let mut out = String::with_capacity(s.len() * 2);
    let mut geminate = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == 'っ' {
            geminate = true;
            i += 1;
            continue;
        }
        if c == 'ー' {
            // Long-vowel mark: repeat the previous vowel if there is one.
            if let Some(v) = out.chars().next_back().filter(|c| "aiueo".contains(*c)) {
                out.push(v);
            }
            i += 1;
            continue;
        }

        // Digraph first (きゃ → kya), then single kana, then pass-through.
        let next_small = chars.get(i + 1).copied().filter(|&n| is_small(n));
        let syl = next_small
            .and_then(|n| digraph(c, n).map(|r| (r, 2)))
            .or_else(|| mono(c).map(|r| (r.to_string(), 1)));

        match syl {
            Some((romaji, used)) => {
                if geminate
                    && let Some(first) = romaji.chars().next().filter(char::is_ascii_alphabetic)
                {
                    out.push(first);
                }
                geminate = false;
                out.push_str(&romaji);
                i += used;
            }
            None => {
                geminate = false;
                out.extend(c.to_lowercase());
                i += 1;
            }
        }
    }
    Some(out)
}

fn is_kana(c: char) -> bool {
    ('ぁ'..='ゖ').contains(&c) || c == 'ー'
}

/// Fold katakana (ァ..ヶ) onto the hiragana block; ー survives for vowel extension.
fn katakana_to_hiragana(c: char) -> char {
    if ('ァ'..='ヶ').contains(&c) {
        char::from_u32(c as u32 - 0x60).unwrap_or(c)
    } else {
        c
    }
}

fn is_small(c: char) -> bool {
    matches!(c, 'ゃ' | 'ゅ' | 'ょ' | 'ぁ' | 'ぃ' | 'ぅ' | 'ぇ' | 'ぉ')
}

#[rustfmt::skip]
fn mono(c: char) -> Option<&'static str> {
    Some(match c {
        'あ' | 'ぁ' => "a", 'い' | 'ぃ' => "i", 'う' | 'ぅ' => "u",
        'え' | 'ぇ' => "e", 'お' | 'ぉ' => "o",
        'か' => "ka", 'き' => "ki", 'く' => "ku", 'け' => "ke", 'こ' => "ko",
        'が' => "ga", 'ぎ' => "gi", 'ぐ' => "gu", 'げ' => "ge", 'ご' => "go",
        'さ' => "sa", 'し' => "shi", 'す' => "su", 'せ' => "se", 'そ' => "so",
        'ざ' => "za", 'じ' => "ji", 'ず' => "zu", 'ぜ' => "ze", 'ぞ' => "zo",
        'た' => "ta", 'ち' => "chi", 'つ' => "tsu", 'て' => "te", 'と' => "to",
        'だ' => "da", 'ぢ' => "ji", 'づ' => "zu", 'で' => "de", 'ど' => "do",
        'な' => "na", 'に' => "ni", 'ぬ' => "nu", 'ね' => "ne", 'の' => "no",
        'は' => "ha", 'ひ' => "hi", 'ふ' => "fu", 'へ' => "he", 'ほ' => "ho",
        'ば' => "ba", 'び' => "bi", 'ぶ' => "bu", 'べ' => "be", 'ぼ' => "bo",
        'ぱ' => "pa", 'ぴ' => "pi", 'ぷ' => "pu", 'ぺ' => "pe", 'ぽ' => "po",
        'ま' => "ma", 'み' => "mi", 'む' => "mu", 'め' => "me", 'も' => "mo",
        'や' | 'ゃ' => "ya", 'ゆ' | 'ゅ' => "yu", 'よ' | 'ょ' => "yo",
        'ら' => "ra", 'り' => "ri", 'る' => "ru", 'れ' => "re", 'ろ' => "ro",
        'わ' | 'ゎ' => "wa", 'ゐ' => "i", 'ゑ' => "e", 'を' => "o",
        'ん' => "n", 'ゔ' => "vu", 'ゕ' => "ka", 'ゖ' => "ke",
        _ => return None,
    })
}

/// Contracted syllables: きゃ → kya, しゅ → shu, ふぁ → fa, てぃ → ti, …
fn digraph(base: char, small: char) -> Option<String> {
    if matches!((base, small), ('て', 'ぃ')) {
        return Some("ti".to_string());
    }
    if matches!((base, small), ('で', 'ぃ')) {
        return Some("di".to_string());
    }
    let lead = match base {
        'き' => "ky", 'ぎ' => "gy", 'に' => "ny", 'ひ' => "hy",
        'び' => "by", 'ぴ' => "py", 'み' => "my", 'り' => "ry",
        'し' => "sh", 'じ' => "j", 'ち' => "ch", 'ぢ' => "j",
        'ふ' => "f", 'ゔ' => "v", 'う' => "w",
        _ => return None,
    };
    let tail = match small {
        'ゃ' => "ya", 'ゅ' => "yu", 'ょ' => "yo",
        'ぁ' => "a", 'ぃ' => "i", 'ぅ' => "u", 'ぇ' => "e", 'ぉ' => "o",
        _ => return None,
    };
    // Every lead already encodes the glide (ky, sh, j …): きゃ → kya, しゃ → sha.
    Some(format!("{lead}{}", tail.trim_start_matches('y')))
}

#[cfg(test)]
mod tests {
    use super::kana_to_romaji;

    #[test]
    fn hiragana_and_katakana_basic() {
        assert_eq!(kana_to_romaji("ののか").as_deref(), Some("nonoka"));
        assert_eq!(kana_to_romaji("ノノカ").as_deref(), Some("nonoka"));
        assert_eq!(kana_to_romaji("すもも").as_deref(), Some("sumomo"));
    }

    #[test]
    fn digraphs_sokuon_and_long_vowel() {
        assert_eq!(kana_to_romaji("しゅか").as_deref(), Some("shuka"));
        assert_eq!(kana_to_romaji("きょうこ").as_deref(), Some("kyouko"));
        assert_eq!(kana_to_romaji("ゆづき").as_deref(), Some("yuzuki"));
        assert_eq!(kana_to_romaji("はっとり").as_deref(), Some("hattori"));
        assert_eq!(kana_to_romaji("ソー").as_deref(), Some("soo"));
    }

    #[test]
    fn no_kana_yields_none_and_mixed_passes_through() {
        assert_eq!(kana_to_romaji("桐島朱夏"), None);
        assert_eq!(kana_to_romaji("Nonoka"), None);
        assert_eq!(
            kana_to_romaji("日比野すもも").as_deref(),
            Some("日比野sumomo")
        );
    }
}
