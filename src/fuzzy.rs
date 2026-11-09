//! Fuzzy matching for the quick switcher and command palette.

/// Score `cand` against `query` (case-insensitive subsequence). Higher is
/// better. Returns matched char indices for highlighting.
pub fn score(query: &str, cand: &str) -> Option<(i64, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, vec![]));
    }
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).filter(|c| !c.is_whitespace()).collect();
    let c: Vec<char> = cand.chars().collect();
    let cl: Vec<char> = c.iter().map(|ch| ch.to_lowercase().next().unwrap_or(*ch)).collect();
    if q.is_empty() {
        return Some((0, vec![]));
    }
    // DP over (query idx, cand idx) keeping the best score ending at each match
    let n = c.len();
    let m = q.len();
    if m > n {
        return None;
    }
    const NEG: i64 = i64::MIN / 4;
    let mut dp = vec![vec![NEG; n]; m];
    let mut from = vec![vec![usize::MAX; n]; m];
    let bonus = |j: usize| -> i64 {
        if j == 0 {
            return 12;
        }
        let p = c[j - 1];
        if p == '/' || p == '\\' {
            10
        } else if p == ' ' || p == '-' || p == '_' || p == '.' {
            8
        } else if c[j].is_uppercase() && p.is_lowercase() {
            7
        } else {
            0
        }
    };
    for i in 0..m {
        let mut best_prev = NEG;
        let mut best_prev_j = usize::MAX;
        for j in 0..n {
            if i > 0 && j > 0 {
                let cand_prev = dp[i - 1][j - 1];
                if cand_prev > NEG {
                    let gap_best = best_prev - 1;
                    if cand_prev > gap_best {
                        best_prev = cand_prev;
                        best_prev_j = j - 1;
                    } else {
                        best_prev = gap_best;
                    }
                } else if best_prev > NEG {
                    best_prev -= 1;
                }
            }
            if cl[j] != q[i] {
                continue;
            }
            let base = 16 + bonus(j);
            if i == 0 {
                dp[i][j] = base - (j as i64).min(20) / 4;
            } else {
                // consecutive match bonus
                let cons = if j > 0 && dp[i - 1][j - 1] > NEG { dp[i - 1][j - 1] + base + 15 } else { NEG };
                let gapped = if best_prev > NEG { best_prev + base - 3 } else { NEG };
                if cons >= gapped && cons > NEG {
                    dp[i][j] = cons;
                    from[i][j] = j - 1;
                } else if gapped > NEG {
                    dp[i][j] = gapped;
                    from[i][j] = best_prev_j;
                }
            }
        }
    }
    let (mut bj, mut bs) = (usize::MAX, NEG);
    for j in 0..n {
        if dp[m - 1][j] > bs {
            bs = dp[m - 1][j];
            bj = j;
        }
    }
    if bs <= NEG {
        return None;
    }
    // reconstruct (best effort: follow pointers, else greedy)
    let mut idx = vec![0; m];
    let mut j = bj;
    for i in (0..m).rev() {
        idx[i] = j;
        if i > 0 {
            let f = from[i][j];
            j = if f != usize::MAX && f < j { f } else { (0..j).rev().find(|&k| cl[k] == q[i - 1]).unwrap_or(0) };
        }
    }
    let len_pen = (n as i64) / 3;
    Some((bs - len_pen, idx))
}

/// Rank candidates; returns (index, score, matched positions), best first.
pub fn rank<S: AsRef<str>>(query: &str, cands: &[S]) -> Vec<(usize, i64, Vec<usize>)> {
    if query.trim().is_empty() {
        return (0..cands.len()).map(|i| (i, 0, vec![])).collect();
    }
    let mut out: Vec<(usize, i64, Vec<usize>)> = cands.iter().enumerate().filter_map(|(i, c)| score(query, c.as_ref()).map(|(s, m)| (i, s, m))).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(cands[a.0].as_ref().len().cmp(&cands[b.0].as_ref().len())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranking() {
        let c = ["notes/daily/2024-01-01.md", "Project Ideas.md", "projects/glyph/readme.md", "pi.md"];
        let r = rank("pi", &c);
        assert_eq!(c[r[0].0], "pi.md");
        let r = rank("projid", &c);
        assert_eq!(c[r[0].0], "Project Ideas.md");
        assert!(score("xyz", "abc").is_none());
        let (_, m) = score("gr", "projects/glyph/readme.md").unwrap();
        assert_eq!(m.len(), 2);
        let r = rank("glyph read", &c);
        assert_eq!(c[r[0].0], "projects/glyph/readme.md");
    }
}
