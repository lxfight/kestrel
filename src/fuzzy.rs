//! Fuzzy matching for the command palette and quick open.

/// A scored fuzzy match with the matched character positions (for highlighting).
pub struct FuzzyMatch {
    pub score: i32,
    pub positions: Vec<usize>,
}

/// Cheap subsequence test used to filter large candidate lists (e.g. 100k files).
/// Returns a rough score (0 = weak), or `None` when `query` is not a subsequence.
pub fn quick_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let mut score = 0i32;
    let text_chars = text.char_indices().peekable();
    let query_chars: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut qi = 0;
    let mut last_match: Option<usize> = None;
    let mut prev_char: Option<char> = None;
    for (idx, ch) in text_chars {
        if qi >= query_chars.len() {
            break;
        }
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        if lower == query_chars[qi] {
            score += 10;
            if last_match.is_some_and(|last| last + ch.len_utf8() == idx) {
                score += 8;
            }
            let boundary = prev_char.is_none_or(|p| {
                p == '/' || p == '\\' || p == '_' || p == '-' || p == '.' || p == ' ' || p == ':'
            }) || prev_char.is_some_and(|p| p.is_lowercase() && ch.is_uppercase());
            if boundary {
                score += 12;
            }
            if qi == 0 && idx == 0 {
                score += 8;
            }
            last_match = Some(idx);
            qi += 1;
        }
        prev_char = Some(ch);
    }
    if qi == query_chars.len() {
        // Prefer shorter texts.
        score += (60i32 - text.len().min(60) as i32).max(0) / 6;
        Some(score)
    } else {
        None
    }
}

/// Full dynamic-programming match used for the final ranking of the top candidates.
pub fn fuzzy_match(query: &str, text: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            positions: Vec::new(),
        });
    }
    let text_chars: Vec<char> = text.chars().collect();
    let query_chars: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let n = query_chars.len();
    let m = text_chars.len();
    if n > m {
        return None;
    }

    const NEG: i32 = i32::MIN / 4;
    // best[i][j]: best score matching query[..=i] with query[i] at text[j].
    let mut best = vec![vec![NEG; m]; n];
    let mut parent = vec![vec![usize::MAX; m]; n];

    let boundary = |idx: usize| -> bool {
        if idx == 0 {
            return true;
        }
        let prev = text_chars[idx - 1];
        matches!(prev, '/' | '\\' | '_' | '-' | '.' | ' ' | ':')
            || (prev.is_lowercase() && text_chars[idx].is_uppercase())
    };

    for j in 0..m {
        let lower = text_chars[j].to_lowercase().next().unwrap_or(text_chars[j]);
        if lower == query_chars[0] {
            best[0][j] = 10 + if boundary(j) { 14 } else { 0 } + if j == 0 { 8 } else { 0 };
        }
    }
    for i in 1..n {
        for j in 0..m {
            let lower = text_chars[j].to_lowercase().next().unwrap_or(text_chars[j]);
            if lower != query_chars[i] {
                continue;
            }
            let mut best_prev = NEG;
            let mut best_k = usize::MAX;
            for (k, prev) in best[i - 1].iter().take(j).enumerate() {
                if *prev <= NEG {
                    continue;
                }
                let mut score = *prev + 10;
                if k + 1 == j {
                    score += 12;
                }
                if boundary(j) {
                    score += 10;
                }
                if score > best_prev {
                    best_prev = score;
                    best_k = k;
                }
            }
            if best_prev > NEG {
                best[i][j] = best_prev;
                parent[i][j] = best_k;
            }
        }
    }

    let mut end = usize::MAX;
    let mut score = NEG;
    for (j, value) in best[n - 1].iter().enumerate().take(m) {
        if *value > score {
            score = *value;
            end = j;
        }
    }
    if end == usize::MAX {
        return None;
    }

    let mut positions = vec![0usize; n];
    let mut j = end;
    for i in (0..n).rev() {
        positions[i] = j;
        if i > 0 {
            j = parent[i][j];
            if j == usize::MAX {
                return None;
            }
        }
    }
    score += (60i32 - m.min(60) as i32).max(0) / 5;
    Some(FuzzyMatch { score, positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_filter() {
        assert!(quick_score("main", "src/main.rs").is_some());
        assert!(quick_score("mrs", "src/main.rs").is_some());
        assert!(quick_score("zzz", "src/main.rs").is_none());
        assert!(quick_score("", "anything").is_some());
    }

    #[test]
    fn ranking_prefers_boundaries() {
        let a = fuzzy_match("app", "src/app.rs").unwrap();
        let b = fuzzy_match("app", "src/happening.rs").unwrap();
        assert!(a.score > b.score, "{} vs {}", a.score, b.score);
        assert_eq!(a.positions, vec![4, 5, 6]);
    }
}
