//! A small, dependency-free line-level diff for the Reader's rerun-compare view.
//!
//! Computes, for two texts, which OLD lines were removed and which NEW lines were
//! added — the complement of their Longest Common Subsequence (by whole line). The
//! Reader uses these flags to tint changed lines red (old pane) / green (new pane)
//! without relying on strict row alignment (which terminal wrapping would break).
//!
//! Common prefix/suffix are stripped first (a rerun usually changes only a few
//! paragraphs, so the LCS DP runs over a tiny middle); a size cap keeps a
//! pathological chapter from quadratic blowup by leaving the middle marked changed.

/// Per-line change flags for a side-by-side diff at line granularity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineDiff {
    /// One flag per OLD line: `true` when the line is absent from the LCS (removed).
    pub old_changed: Vec<bool>,
    /// One flag per NEW line: `true` when the line is absent from the LCS (added).
    pub new_changed: Vec<bool>,
    /// Count of removed (old) lines.
    pub removed: usize,
    /// Count of added (new) lines.
    pub added: usize,
}

/// Diff `old` against `new` by line.
pub fn diff_lines(old: &str, new: &str) -> LineDiff {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let (a_in, b_in) = lcs_membership(&a, &b);
    let old_changed: Vec<bool> = a_in.iter().map(|x| !x).collect();
    let new_changed: Vec<bool> = b_in.iter().map(|x| !x).collect();
    let removed = old_changed.iter().filter(|c| **c).count();
    let added = new_changed.iter().filter(|c| **c).count();
    LineDiff {
        old_changed,
        new_changed,
        removed,
        added,
    }
}

/// Beyond this `old_middle * new_middle` product we skip the LCS DP and report the
/// whole (post-prefix/suffix) middle as changed. 4M cells ≈ a few ms / 16 MB worst
/// case; real light-novel chapters are a few hundred lines, far under this.
const LCS_CELL_CAP: usize = 4_000_000;

/// Returns `(a_in_lcs, b_in_lcs)`: one bool per line of each side marking whether
/// that line participates in the longest common subsequence (i.e. is unchanged).
fn lcs_membership(a: &[&str], b: &[&str]) -> (Vec<bool>, Vec<bool>) {
    let mut a_in = vec![false; a.len()];
    let mut b_in = vec![false; b.len()];

    // Common prefix — trivially part of the LCS.
    let mut lo = 0usize;
    while lo < a.len() && lo < b.len() && a[lo] == b[lo] {
        a_in[lo] = true;
        b_in[lo] = true;
        lo += 1;
    }
    // Common suffix — likewise, stopping before the shared prefix.
    let mut hi_a = a.len();
    let mut hi_b = b.len();
    while hi_a > lo && hi_b > lo && a[hi_a - 1] == b[hi_b - 1] {
        a_in[hi_a - 1] = true;
        b_in[hi_b - 1] = true;
        hi_a -= 1;
        hi_b -= 1;
    }

    let am = &a[lo..hi_a];
    let bm = &b[lo..hi_b];
    let (n, m) = (am.len(), bm.len());
    if n == 0 || m == 0 {
        return (a_in, b_in); // one side wholly inserted/deleted in the middle
    }
    if n.saturating_mul(m) > LCS_CELL_CAP {
        return (a_in, b_in); // too large: leave the middle marked changed
    }

    // Suffix-DP LCS lengths: dp[i][j] = LCS(am[i..], bm[j..]).
    let stride = m + 1;
    let mut dp = vec![0u32; (n + 1) * stride];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * stride + j] = if am[i] == bm[j] {
                dp[(i + 1) * stride + (j + 1)] + 1
            } else {
                dp[(i + 1) * stride + j].max(dp[i * stride + (j + 1)])
            };
        }
    }

    // Backtrack the matched pairs, marking them unchanged on both sides.
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if am[i] == bm[j] {
            a_in[lo + i] = true;
            b_in[lo + j] = true;
            i += 1;
            j += 1;
        } else if dp[(i + 1) * stride + j] >= dp[i * stride + (j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }

    (a_in, b_in)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_has_no_changes() {
        let d = diff_lines("a\nb\nc", "a\nb\nc");
        assert_eq!((d.removed, d.added), (0, 0));
        assert_eq!(d.old_changed, vec![false, false, false]);
        assert_eq!(d.new_changed, vec![false, false, false]);
    }

    #[test]
    fn single_middle_line_edit() {
        // Middle line changes; first/last are common prefix/suffix.
        let d = diff_lines("a\nOLD\nc", "a\nNEW\nc");
        assert_eq!(d.old_changed, vec![false, true, false]);
        assert_eq!(d.new_changed, vec![false, true, false]);
        assert_eq!((d.removed, d.added), (1, 1));
    }

    #[test]
    fn pure_insertion() {
        let d = diff_lines("a\nc", "a\nb\nc");
        assert_eq!(d.old_changed, vec![false, false]);
        assert_eq!(d.new_changed, vec![false, true, false]);
        assert_eq!((d.removed, d.added), (0, 1));
    }

    #[test]
    fn pure_deletion() {
        let d = diff_lines("a\nb\nc", "a\nc");
        assert_eq!(d.old_changed, vec![false, true, false]);
        assert_eq!(d.new_changed, vec![false, false]);
        assert_eq!((d.removed, d.added), (1, 0));
    }

    #[test]
    fn reordered_block_uses_lcs_not_set_membership() {
        // "b" moved: LCS keeps one alignment, the moved copy reads as add+remove.
        let d = diff_lines("a\nb\nc\nd", "a\nc\nd\nb");
        // a,c,d form the LCS; old "b" (idx 1) removed, new "b" (idx 3) added.
        assert_eq!(d.old_changed, vec![false, true, false, false]);
        assert_eq!(d.new_changed, vec![false, false, false, true]);
    }

    #[test]
    fn empty_old_marks_all_new_added() {
        let d = diff_lines("", "x\ny");
        assert_eq!(d.removed, 0);
        assert_eq!(d.added, 2);
        assert_eq!(d.new_changed, vec![true, true]);
    }

    #[test]
    fn thai_prose_paragraph_change() {
        let old = "เขาเดินเข้าไปในเงา\n「ใครอยู่ตรงนั้น?」\nเสียงสะท้อน";
        let new = "เขาก้าวเข้าสู่เงามืด\n「ใครอยู่ตรงนั้น?」\nเสียงสะท้อนก้องดังขึ้น";
        let d = diff_lines(old, new);
        // Line 0 and line 2 changed; the quoted line is identical → unchanged.
        assert_eq!(d.old_changed, vec![true, false, true]);
        assert_eq!(d.new_changed, vec![true, false, true]);
    }
}
