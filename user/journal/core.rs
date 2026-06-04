// yolo CLI — journal/core.rs
//
// The Journal: segments + markers + precomputed liveness.
// Borrowing filter methods — no moves, no collects, no intermediate allocations.

use super::marker::MarkerIndex;
use super::parse;
use super::tree::DirTree;
use super::types::*;
use anyhow::Result;
use std::path::Path;

/// All segments + P/T skeleton + precomputed alive mask.
pub struct Journal {
    pub segments: Vec<Segment>,
    pub markers: MarkerIndex,
    alive: Vec<bool>,
}

impl Journal {
    /// Build from parsed journal records.
    pub fn new(records: Vec<Record>) -> Self {
        let mut segments = Vec::new();
        let mut markers_vec: Vec<Marker> = vec![Marker::Snapshot {
            gen_id: 0,
            name: "(initial)".into(),
        }];
        let mut current_records: Vec<Record> = Vec::new();
        let mut current_from: u64 = 0;

        for record in records.into_iter() {
            match record {
                Record::Marker(marker @ Marker::Snapshot { gen_id, .. }) => {
                    segments.push(Segment {
                        from: current_from,
                        records: std::mem::take(&mut current_records),
                    });
                    current_from = gen_id;
                    markers_vec.push(marker);
                }
                Record::Marker(marker @ Marker::Travel { target_gen, .. }) => {
                    segments.push(Segment {
                        from: current_from,
                        records: std::mem::take(&mut current_records),
                    });
                    markers_vec.push(marker);
                    current_from = target_gen;
                }
                Record::Action(_) | Record::Note(_) => {
                    current_records.push(record);
                }
            }
        }

        // Trailing segment.
        segments.push(Segment {
            from: current_from,
            records: current_records,
        });

        let markers = MarkerIndex::new(markers_vec);
        let alive = markers.alive_segments(segments.len());

        Journal {
            segments,
            markers,
            alive,
        }
    }

    /// Read and parse the journal file, then build a Journal.
    pub fn read(yolo_dir: &Path) -> Result<Self> {
        Ok(Self::new(parse::read(yolo_dir)?))
    }

    /// Whether the segment with this gen_id is alive (for audit/timeline display).
    pub fn is_alive(&self, gen_id: usize) -> bool {
        self.alive.get(gen_id).copied().unwrap_or(false)
    }

    /// Segment range [start, end) for the default scoped ("latest") view — the
    /// most recent batch of changes. Each `yolo exec` auto-snapshots, so the
    /// usual tip is an empty trailing segment; in that case we show the segment
    /// the most recent snapshot captured (the last command's work). If there is
    /// uncommitted work *after* the last snapshot we show that instead, and with
    /// no snapshots at all we fall back to the full range. The bool is true when
    /// older history is hidden, so callers can hint about the full range (`..`).
    pub fn latest_range(&self) -> (usize, usize, bool) {
        let num = self.segments.len();
        match self.markers.last_snapshot_idx() {
            None => (0, num, false),
            Some(s) => {
                // Live, non-empty work after the last snapshot (e.g. with
                // auto-snapshot off) — show that. A post-travel dead zone is not
                // live, so it doesn't count.
                let live_tail =
                    (s..num).any(|i| self.is_alive(i) && !self.segments[i].records.is_empty());
                if live_tail {
                    (s, num, s > 0)
                } else {
                    let start = self.markers.prev_snapshot_idx(s);
                    (start, s, start > 0)
                }
            }
        }
    }

    /// Consume the journal and build a DirTree from all live segments.
    pub fn into_tree(self) -> DirTree {
        DirTree::build(self.into_live_segments())
    }

    /// Consume the journal and build a DirTree from live segments up to a snapshot.
    pub fn into_tree_at(self, gen_id: u64) -> DirTree {
        DirTree::build(self.into_live_segments_at(gen_id))
    }

    /// Whether any live segment carries a staging action (S/D/R) — i.e. there
    /// are staged changes to commit or discard. Cheaper than
    /// `into_tree().is_empty()` (builds no tree, early-exits) and correctly
    /// ignores what isn't a staged change: audit notes, snapshot/travel markers,
    /// and changes a `travel` left behind in a dead segment.
    pub fn has_staged_changes(&self) -> bool {
        self.segments
            .iter()
            .enumerate()
            .filter(|(i, _)| self.is_alive(*i))
            .any(|(_, seg)| seg.records.iter().any(|r| matches!(r, Record::Action(_))))
    }

    /// Consume the journal and return owned live segments in `[start, end)`.
    /// When `start == 0` and `end == segments.len()`, returns all live segments.
    pub fn into_live_segments_range(
        self,
        start: usize,
        end: usize,
    ) -> impl Iterator<Item = Segment> {
        let alive = self.alive;
        self.segments
            .into_iter()
            .enumerate()
            .filter(move |(i, _)| *i >= start && *i < end && alive[*i])
            .map(|(_, seg)| seg)
    }

    /// Borrow the live segments in `[start, end)` — the read-only counterpart to
    /// [`into_live_segments_range`](Self::into_live_segments_range), for callers
    /// that build a tree without consuming the journal (e.g. `Changeset::collect`
    /// run once per segment for `--each`).
    pub fn live_segments_range(
        &self,
        start: usize,
        end: usize,
    ) -> impl Iterator<Item = &Segment> {
        let alive = &self.alive;
        self.segments
            .iter()
            .enumerate()
            .filter(move |(i, _)| *i >= start && *i < end && alive[*i])
            .map(|(_, seg)| seg)
    }

    // ── Private helpers ──────────────────────────────────────────────

    fn into_live_segments(self) -> impl Iterator<Item = Segment> {
        let len = self.segments.len();
        self.into_live_segments_range(0, len)
    }

    fn into_live_segments_at(self, gen_id: u64) -> impl Iterator<Item = Segment> {
        let num_prefix = (gen_id as usize).min(self.segments.len());
        // Include one extra marker so that a travel marker at gen_id
        // participates in the dead-zone scan.
        let marker_end = (gen_id as usize + 1).min(self.markers.len());
        let alive = self.markers.alive_segments_range(0..marker_end, num_prefix);
        self.segments
            .into_iter()
            .enumerate()
            .take(num_prefix)
            .filter(move |(i, _)| alive[*i])
            .map(|(_, s)| s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_staged_changes_ignores_notes_markers_and_dead() {
        // Empty journal → nothing staged.
        assert!(!Journal::new(vec![]).has_staged_changes());

        // A bare note (e.g. a blocked access) is not a staged change.
        let notes_only = Journal::new(vec![Record::Note(Note::Block {
            path: "/etc/x".into(),
            op: Op::Write,
        })]);
        assert!(!notes_only.has_staged_changes());

        // A live stage counts.
        let staged = Journal::new(vec![Record::Action(Action::Stage {
            path: "/a".into(),
            ino: 1,
            preimage: None,
        })]);
        assert!(staged.has_staged_changes());

        // A change a `travel` left in a dead segment does not count.
        let traveled = Journal::new(vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Travel {
                gen_id: 2,
                target_gen: 1,
            }),
        ]);
        assert!(!traveled.has_staged_changes());
    }

    // ── Segmentation tests (migrated from segment.rs) ────────────────

    #[test]
    fn segmentation_basic() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
        ];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 4);
        assert_eq!(j.segments[0].from, 0);
        assert!(j.segments[0].records.is_empty());
        assert_eq!(j.segments[1].from, 1);
        assert_eq!(j.segments[1].records.len(), 1);
        assert_eq!(j.segments[2].from, 2);
        assert_eq!(j.segments[2].records.len(), 1);
        assert_eq!(j.segments[3].from, 3);
        assert!(j.segments[3].records.is_empty());
    }

    #[test]
    fn segmentation_splits_at_t_boundary() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 6);
        assert_eq!(j.segments[4].from, 2);
        assert_eq!(j.segments[4].records.len(), 1);
        assert!(
            matches!(&j.segments[4].records[0], Record::Action(Action::Stage { path, .. }) if path == "/d")
        );
    }

    #[test]
    fn records_before_first_snapshot_in_segment_zero() {
        let records = vec![
            Record::Action(Action::Stage {
                path: "/orphan".into(),
                ino: 999,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 3);
        assert_eq!(j.segments[0].from, 0);
        assert_eq!(j.segments[0].records.len(), 1);
        assert!(
            matches!(&j.segments[0].records[0], Record::Action(Action::Stage { path, .. }) if path == "/orphan")
        );
        assert_eq!(j.segments[1].records.len(), 1);
        assert!(
            matches!(&j.segments[1].records[0], Record::Action(Action::Stage { path, .. }) if path == "/a")
        );
    }

    #[test]
    fn segmentation_empty_journal() {
        let j = Journal::new(vec![]);
        assert_eq!(j.segments.len(), 1);
        assert_eq!(j.segments[0].from, 0);
        assert!(j.segments[0].records.is_empty());
        assert_eq!(j.markers.len(), 1, "phantom marker only");
    }

    #[test]
    fn segmentation_only_t_records() {
        let records = vec![Record::Marker(Marker::Travel {
            gen_id: 1,
            target_gen: 99,
        })];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 2);
        assert_eq!(j.segments[0].from, 0);
        assert_eq!(j.segments[1].from, 99);
        assert_eq!(j.markers.len(), 2);
    }

    #[test]
    fn segmentation_notes_ride_in_segments() {
        // Notes are observational; they live in the same segment as the
        // surrounding actions and do not split it.
        let records = vec![
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Note(Note::Block {
                path: "/etc/x".into(),
                op: Op::Write,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Note(Note::Block {
                path: "/etc/y".into(),
                op: Op::Write,
            }),
        ];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 2);
        // seg0: action + note before the first snapshot.
        assert_eq!(j.segments[0].records.len(), 2);
        assert!(matches!(
            &j.segments[0].records[0],
            Record::Action(Action::Stage { path, .. }) if path == "/a"
        ));
        assert!(matches!(
            &j.segments[0].records[1],
            Record::Note(Note::Block { path, .. }) if path == "/etc/x"
        ));
        // seg1: trailing note.
        assert_eq!(j.segments[1].records.len(), 1);
        assert!(matches!(
            &j.segments[1].records[0],
            Record::Note(Note::Block { path, .. }) if path == "/etc/y"
        ));
    }

    // ── Live segments tests (migrated from liveness.rs) ──────────────

    #[test]
    fn live_segments_basic() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        let live: Vec<_> = j.into_live_segments_range(0, usize::MAX).collect();
        // seg0, seg1 alive; seg2,seg3 dead; seg4,seg5 alive → 4 live
        assert_eq!(live.len(), 4);
    }

    #[test]
    fn live_segments_at_basic() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        // Live prefix up to P2: seg0 (empty) + seg1 ([A])
        let live: Vec<_> = j.into_live_segments_at(2).collect();
        let actions: Vec<_> = live.iter().flat_map(|s| &s.records).collect();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Record::Action(Action::Stage { path, .. }) if path == "/a"));
    }

    // ── Reachability tests (via live_segments, migrated from liveness.rs) ──

    fn live_actions(records: Vec<Record>) -> Vec<String> {
        let j = Journal::new(records);
        j.into_live_segments_range(0, usize::MAX)
            .flat_map(|s| s.records)
            .filter_map(|r| match r {
                Record::Action(Action::Stage { path, .. }) => Some(path),
                Record::Action(Action::Delete { path, .. }) => Some(path),
                Record::Action(Action::Rename { dst, .. }) => Some(dst),
                Record::Note(_) | Record::Marker(_) => None,
            })
            .collect()
    }

    #[test]
    fn reachable_no_travels() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        // 3 segments (seg0 empty, seg1 [/a], seg2 empty), all alive, 1 action
        let j = Journal::new(records);
        assert_eq!(j.into_live_segments_range(0, usize::MAX).count(), 3);
    }

    #[test]
    fn reachable_multiple_travels_last_wins() {
        // P1 [A] P2 [B] P3 T4(P2) [D] P5 T6(P1)
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 6,
                target_gen: 1,
            }),
        ];
        let actions = live_actions(records);
        assert!(
            actions.is_empty(),
            "last travel to P1 kills everything after P1"
        );
    }

    #[test]
    fn reachable_nested_t_in_dead_zone() {
        // P1 [A] P2 [B] P3 T4(P1) [D] P5 [E] P6 T7(P5)
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Action(Action::Stage {
                path: "/e".into(),
                ino: 4,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 6,
                name: "c6".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 7,
                target_gen: 5,
            }),
        ];
        let actions = live_actions(records);
        assert_eq!(actions, vec!["/d"]);
    }

    #[test]
    fn reachable_undo_travel() {
        // P1 [A] P2 [B] P3 T4(P1) [D] P5 T6(P3)
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 6,
                target_gen: 3,
            }),
        ];
        let actions = live_actions(records);
        assert_eq!(actions, vec!["/a", "/b"]);
    }

    #[test]
    fn reachable_travel_to_first_snapshot() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 3,
                target_gen: 1,
            }),
        ];
        let actions = live_actions(records);
        assert!(
            actions.is_empty(),
            "travel to first snapshot discards all actions"
        );
    }

    #[test]
    fn reachable_consecutive_t_records() {
        // P1 [A] P2 [B] P3 T4(P2) T5(P1)
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Marker(Marker::Travel {
                gen_id: 5,
                target_gen: 1,
            }),
        ];
        let actions = live_actions(records);
        assert!(
            actions.is_empty(),
            "consecutive travels: last one (P1) wins"
        );
    }

    #[test]
    fn reachable_single_travel() {
        // P1 [A] P2 [B] P3 T4(P2) [D] P5
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let actions = live_actions(records);
        assert_eq!(actions, vec!["/a", "/d"]);
    }

    #[test]
    fn reachable_empty_journal() {
        let actions = live_actions(vec![]);
        assert!(actions.is_empty());
    }

    #[test]
    fn reachable_corrupt_t_record_skipped() {
        // P1 [A] T2(P99) [B]
        // T targets nonexistent P99 — skipped, all segments alive.
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Travel {
                gen_id: 2,
                target_gen: 99,
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
        ];
        let actions = live_actions(records);
        assert_eq!(actions, vec!["/a", "/b"]);
    }

    // ── Slice / prefix tests (migrated from liveness.rs) ─────────────

    #[test]
    fn live_segments_slice_from_to() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Action(Action::Stage {
                path: "/c".into(),
                ino: 3,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 4,
                name: "c4".into(),
            }),
        ];
        let j = Journal::new(records);
        let num = j.segments.len();
        let (start, end) = j
            .markers
            .segment_range(None, Some("c2"), Some("c3"), num)
            .unwrap();
        let live: Vec<_> = j.segments[start..end]
            .iter()
            .enumerate()
            .filter(|(i, _)| j.is_alive(start + i))
            .map(|(_, s)| s)
            .collect();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].from, 2);
        assert!(
            matches!(&live[0].records[0], Record::Action(Action::Stage { path, .. }) if path == "/b")
        );
    }

    #[test]
    fn live_segments_slice_not_found() {
        let records = vec![Record::Marker(Marker::Snapshot {
            gen_id: 1,
            name: "init".into(),
        })];
        let j = Journal::new(records);
        assert!(
            j.markers
                .segment_range(Some("nonexistent"), None, None, j.segments.len())
                .is_err()
        );
    }

    #[test]
    fn live_segments_at_with_nested_travels() {
        // P1 [A] P2 [B] P3 T4(P1) [C] P5 [D] P6
        // live_segments_at(P5): prefix markers P1,P2,P3,T4.
        // T4(P1) kills seg1,seg2,seg3 → live: seg0 (empty) + seg4 ([C])
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/c".into(),
                ino: 3,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 4,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 6,
                name: "c6".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.markers.find_marker("c5").unwrap();
        let live: Vec<_> = j.into_live_segments_at(gen_id).collect();
        let actions: Vec<_> = live.iter().flat_map(|s| &s.records).collect();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Record::Action(Action::Stage { path, .. }) if path == "/c"));
    }

    #[test]
    fn live_segments_at_clamps_invalid_gen_id() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records.clone());
        let all_live = j.into_live_segments_range(0, usize::MAX).count();
        let j2 = Journal::new(records);
        // gen_id 999 is way beyond segments.len(); should clamp, not panic.
        let live: Vec<_> = j2.into_live_segments_at(999).collect();
        assert!(live.len() <= all_live);
    }

    #[test]
    fn into_tree_at_travel_marker() {
        // [A:/a] P1 [B:/b] P2 T3(→P1)
        // into_tree_at(3) should give the journal state at position 3.
        let records = vec![
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 3,
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let tree = j.into_tree_at(3);
        assert_eq!(tree.len(), 1, "only /a should be in the tree");
        assert!(tree.get("/a").is_some());
        assert!(tree.get("/b").is_none());
    }

    // ── Original tests ───────────────────────────────────────────────

    #[test]
    fn is_alive_basic() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                preimage: None,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 2,
            }),
        ];
        let j = Journal::new(records);
        assert!(j.is_alive(0));
        assert!(j.is_alive(1));
        assert!(!j.is_alive(2));
        assert!(!j.is_alive(3));
        assert!(j.is_alive(4));
    }
}
