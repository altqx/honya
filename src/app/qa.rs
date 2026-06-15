//! Translation QA: aggregate the active volume's quality issues into one reviewable
//! report for the QA overlay. Sourced from durable on-disk signals — review-needed
//! chunk markers (`translated/ch_NNN.md`) and continuity notes (`VOLUME.md`) — plus
//! the live `ChapterStatus::Failed` a run leaves in memory. Always scoped to the
//! active volume (the one `workspace` resolves), matching the rest of the app.

use crate::model::ChapterStatus;

use super::ActiveProject;

/// Severity of a continuity note that rises to a QA issue (info notes are skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Conflict,
}

/// What kind of finding a [`QaIssue`] is — decides its glyph, color, and tag.
#[derive(Debug, Clone)]
pub enum QaKind {
    /// A chunk committed without passing review (durable `REVIEW_NEEDED` flag).
    ReviewChunk { chunk: u32 },
    /// A chapter that hit max retries / a hard error during this session.
    ChapterFailed,
    /// A continuity note the Orchestrator flagged (warning or conflict).
    Continuity { severity: Severity },
    /// Project-wide roster rendering drift, not chapter-anchored.
    Consistency,
}

/// One QA finding, navigable to `chapter` (None = a note not anchored to a chapter).
#[derive(Debug, Clone)]
pub struct QaIssue {
    pub chapter: Option<u32>,
    /// Owning chapter's display title (empty for unanchored notes), for grouping.
    pub title: String,
    pub kind: QaKind,
    /// One-line reviewer reason / continuity note (may be empty → a default is shown).
    pub detail: String,
}

/// The active volume's aggregated QA report: a flat, chapter-sorted issue list plus
/// chapter-level counts that drive the overlay's summary header.
#[derive(Debug, Clone, Default)]
pub struct QaReport {
    pub issues: Vec<QaIssue>,
    /// Chapters fully done with a clean (passed) review.
    pub done: u32,
    /// Chapters carrying ≥1 review-needed chunk.
    pub review: u32,
    /// Chapters that failed this session.
    pub failed: u32,
}

impl QaReport {
    /// Issues belonging to one chapter (drives the per-chapter count badge).
    pub fn count_for(&self, chapter: Option<u32>) -> usize {
        self.issues.iter().filter(|i| i.chapter == chapter).count()
    }

    /// Percent of *attempted* chapters that are clean-done — `None` when none have
    /// been attempted yet (so the header shows no misleading 0%).
    pub fn clean_pct(&self) -> Option<u16> {
        let attempted = self.done + self.review + self.failed;
        if attempted == 0 {
            None
        } else {
            Some(((self.done as f64 / attempted as f64) * 100.0).round() as u16)
        }
    }
}

/// Gather QA issues for the active project's active volume. Reads each
/// review-needed chapter's translated file and the volume's continuity notes from
/// disk (cheap: only `NeedsReview` chapters are read, and they are few).
pub fn collect(active: &ActiveProject) -> QaReport {
    let vol = active.vol;
    let mut report = QaReport::default();

    let volume = active.project.volumes.iter().find(|v| v.number == vol);

    if let Some(volume) = volume {
        for ch in &volume.chapters {
            match ch.status {
                ChapterStatus::Done | ChapterStatus::Appended => report.done += 1,
                ChapterStatus::Failed => {
                    report.failed += 1;
                    report.issues.push(QaIssue {
                        chapter: Some(ch.number),
                        title: ch.title.clone(),
                        kind: QaKind::ChapterFailed,
                        detail: String::new(),
                    });
                }
                ChapterStatus::NeedsReview => {
                    report.review += 1;
                    let text = std::fs::read_to_string(active.workspace.translated(ch.number))
                        .unwrap_or_default();
                    for (chunk, reason) in
                        crate::workspace::translation::review_needed_details_in(&text)
                    {
                        report.issues.push(QaIssue {
                            chapter: Some(ch.number),
                            title: ch.title.clone(),
                            kind: QaKind::ReviewChunk { chunk },
                            detail: reason,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    // Continuity notes (warning / conflict only) from VOLUME.md.
    let data = crate::workspace::volume::load(&active.workspace);
    for note in &data.notes {
        let severity = match note.severity.trim().to_ascii_lowercase().as_str() {
            "conflict" => Severity::Conflict,
            "warning" | "warn" => Severity::Warning,
            _ => continue, // info notes are not QA issues
        };
        let title = note
            .chapter
            .and_then(|n| volume.and_then(|v| v.chapters.iter().find(|c| c.number == n)))
            .map(|c| c.title.clone())
            .unwrap_or_default();
        report.issues.push(QaIssue {
            chapter: note.chapter,
            title,
            kind: QaKind::Continuity { severity },
            detail: note.note.clone(),
        });
    }

    // Shared rosters can drift across volumes; surface that in QA.
    let chars = crate::workspace::characters::load(&active.workspace);
    let terms = crate::workspace::glossary::load(&active.workspace);
    for issue in crate::workspace::consistency::roster_consistency(&chars, &terms) {
        report.issues.push(QaIssue {
            chapter: None,
            title: String::new(),
            kind: QaKind::Consistency,
            detail: issue.detail,
        });
    }

    // Group by chapter for the overlay's per-chapter rendering. A stable sort keeps
    // each chapter's failed→review→continuity insertion order; unanchored notes last.
    report.issues.sort_by_key(|i| i.chapter.unwrap_or(u32::MAX));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(chapter: Option<u32>) -> QaIssue {
        QaIssue {
            chapter,
            title: String::new(),
            kind: QaKind::ChapterFailed,
            detail: String::new(),
        }
    }

    #[test]
    fn count_for_and_clean_pct() {
        let report = QaReport {
            issues: vec![issue(Some(3)), issue(Some(3)), issue(Some(7)), issue(None)],
            done: 6,
            review: 1,
            failed: 1,
        };
        assert_eq!(report.count_for(Some(3)), 2);
        assert_eq!(report.count_for(Some(7)), 1);
        assert_eq!(report.count_for(None), 1);
        // 6 clean of 8 attempted → 75%.
        assert_eq!(report.clean_pct(), Some(75));
    }

    #[test]
    fn clean_pct_is_none_when_nothing_attempted() {
        assert_eq!(QaReport::default().clean_pct(), None);
    }
}
