// yolo CLI — journal/markers.rs
//
// The P/T skeleton: snapshot and travel records extracted from the journal.
// Provides lookup by gen_id or name, and segment range computation.

use super::types::*;
use anyhow::Result;

/// The P/T skeleton of the journal.
pub struct MarkerIndex(pub(super) Vec<Marker>);

impl MarkerIndex {
    pub(super) fn new(markers: Vec<Marker>) -> Self {
        MarkerIndex(markers)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, idx: usize) -> Option<&Marker> {
        self.0.get(idx)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Marker> {
        self.0.iter()
    }

    /// Find any marker by numeric gen ID. O(1) via direct indexing.
    ///
    /// A marker's gen *is* its index: the phantom "(initial)" marker sits at
    /// index 0, and `Journal::new` assigns each P/T record the next index. So
    /// resolving a gen is a bounds check — gen 0 is the base marker, the rest
    /// address snapshots/travels in order.
    fn find_marker_by_gen_id(&self, gen_id: u64) -> Result<u64> {
        if (gen_id as usize) < self.0.len() {
            Ok(gen_id)
        } else {
            anyhow::bail!("marker not found: {gen_id}")
        }
    }

    /// Find a snapshot by name. Returns the last match (names may repeat).
    fn find_snapshot_by_name(&self, name: &str) -> Result<u64> {
        let mut last = None;
        for (idx, marker) in self.0.iter().enumerate() {
            if let Marker::Snapshot { name: n } = marker
                && n == name
            {
                last = Some(idx as u64);
            }
        }
        last.ok_or_else(|| anyhow::anyhow!("marker not found: {name}"))
    }

    /// Find a marker (snapshot or travel) by name or numeric ID.
    /// Names only match snapshots (travel markers have no names).
    pub fn find_marker(&self, name_or_id: &str) -> Result<u64> {
        if let Ok(id) = name_or_id.parse::<u64>() {
            return self.find_marker_by_gen_id(id);
        }
        self.find_snapshot_by_name(name_or_id)
    }

    /// Get the marker at this index (returns `None` for the phantom
    /// marker at index 0).
    pub fn marker_at(&self, marker_idx: usize) -> Option<&Marker> {
        if marker_idx == 0 {
            return None;
        }
        self.0.get(marker_idx)
    }

    /// Index of the most recent snapshot marker (highest index > 0),
    /// or `None` when only the phantom initial marker exists.
    pub fn last_snapshot_idx(&self) -> Option<usize> {
        self.0.iter().enumerate().rev().find_map(|(i, m)| {
            (i > 0 && matches!(m, Marker::Snapshot { .. })).then_some(i)
        })
    }

    /// Index of the nearest snapshot marker before `idx` (skipping the phantom
    /// at index 0), or 0 when there is none.
    pub fn prev_snapshot_idx(&self, idx: usize) -> usize {
        (1..idx)
            .rev()
            .find(|&i| matches!(&self.0[i], Marker::Snapshot { .. }))
            .unwrap_or(0)
    }

    /// Compute the segment index range `[start, end)` for a status/diff query.
    /// `at` → that snapshot's own segment (`prev(at)..at`); otherwise `from`/`to`
    /// bound a range, defaulting to the base (0) and the tip.
    pub fn segment_range(
        &self,
        at: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        num_segments: usize,
    ) -> Result<(usize, usize)> {
        if let Some(name) = at {
            let m_idx = self.find_marker(name)? as usize;
            return Ok((self.prev_snapshot_idx(m_idx), m_idx));
        }

        let start = if let Some(from_name) = from {
            self.find_marker(from_name)? as usize
        } else {
            0
        };

        let end = if let Some(to_name) = to {
            self.find_marker(to_name)? as usize
        } else {
            num_segments
        };

        if start > end {
            anyhow::bail!("invalid range: start snapshot comes after end snapshot");
        }

        Ok((start, end))
    }

    // ── Liveness computation ─────────────────────────────────────────

    /// Compute alive flags for all segments.
    ///
    /// Walks travel (T) markers right-to-left. Each T(target_gen) kills
    /// segments between the target snapshot and the T marker.
    pub fn alive_segments(&self, num_segments: usize) -> Vec<bool> {
        self.alive_segments_range(0..self.len(), num_segments)
    }

    /// Compute alive flags for segments using only markers in `range`.
    pub fn alive_segments_range(
        &self,
        range: std::ops::Range<usize>,
        num_segments: usize,
    ) -> Vec<bool> {
        if num_segments == 0 {
            return vec![];
        }

        // A marker's gen is its index, so a travel's `target_gen` is the target
        // marker's index directly — no gen→index lookup needed.
        let (rstart, rend) = (range.start, range.end);
        let mut alive = vec![true; num_segments];
        let mut alive_end = rend;

        for m in (rstart..rend).rev() {
            let Marker::Travel { target_gen } = &self.0[m] else {
                continue;
            };
            if m > alive_end {
                continue;
            }
            let k_idx = *target_gen as usize;
            // Ignore targets outside this window or not pointing strictly
            // backward (a corrupt/forward `target_gen`, e.g. one past the tip).
            if !(rstart..m).contains(&k_idx) {
                continue;
            }
            for flag in &mut alive[k_idx..m] {
                *flag = false;
            }
            alive_end = k_idx;
        }

        alive
    }
}

impl IntoIterator for MarkerIndex {
    type Item = Marker;
    type IntoIter = std::vec::IntoIter<Marker>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::core::Journal;

    // ── Marker lookup tests (migrated from segment.rs) ───────────────

    #[test]
    fn find_marker_by_gen_id() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "first".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "second".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.markers.find_marker_by_gen_id(1).unwrap();
        assert_eq!(gen_id, 1);
    }

    #[test]
    fn find_snapshot_by_name() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "first".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "second".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.markers.find_snapshot_by_name("second").unwrap();
        assert_eq!(gen_id, 2);
    }

    #[test]
    fn find_snapshot_not_found() {
        let records = vec![Record::Marker(Marker::Snapshot {
            name: "first".into(),
        })];
        let j = Journal::new(records);
        assert!(j.markers.find_snapshot_by_name("nonexistent").is_err());
    }

    #[test]
    fn find_snapshot_duplicate_names_returns_last() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "dup".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "dup".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.markers.find_snapshot_by_name("dup").unwrap();
        assert_eq!(gen_id, 2, "should return the last matching snapshot");
    }

    #[test]
    fn marker_at_on_travel_marker() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        assert!(
            j.markers.marker_at(0).is_none(),
            "Phantom marker should return None"
        );
        assert!(j.markers.marker_at(1).is_some());
        assert!(j.markers.marker_at(2).is_some());
        assert!(
            j.markers.marker_at(3).is_some(),
            "Travel marker should be returned by marker_at"
        );
    }

    #[test]
    fn find_marker_by_gen_id_resolves_phantom() {
        let records = vec![Record::Marker(Marker::Snapshot {
            name: "c1".into(),
        })];
        let j = Journal::new(records);
        // gen 0 is the base/"(initial)" marker — addressable like its name.
        assert_eq!(j.markers.find_marker_by_gen_id(0).unwrap(), 0);
        assert!(j.markers.find_marker_by_gen_id(1).is_ok());
        // Out-of-range ids still error.
        assert!(j.markers.find_marker_by_gen_id(99).is_err());
    }

    #[test]
    fn travel_targeting_phantom() {
        // Travel to gen_id=0 (initial state) should kill all segments
        // between the phantom and the travel marker.
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Travel {
                target_gen: 0,
            }),
        ];
        let j = Journal::new(records);
        // markers: [phantom(0), P(1), P(2), T(3→0)]
        // segments: [seg0, seg1, seg2, seg3]
        // Travel to 0 kills segments 0..3 → seg0, seg1, seg2 dead.
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(!alive[0], "seg0 killed by travel to initial");
        assert!(!alive[1], "seg1 killed by travel to initial");
        assert!(!alive[2], "seg2 killed by travel to initial");
        assert!(alive[3], "seg3 (trailing after travel) alive");
    }

    #[test]
    fn segment_range_at_phantom_is_empty() {
        let records = vec![Record::Marker(Marker::Snapshot {
            name: "c1".into(),
        })];
        let j = Journal::new(records);
        // Snapshot 0 (the base) resolves to an empty range — the base introduced
        // no changes, so `status 0` shows nothing (no longer an error).
        let r = j
            .markers
            .segment_range(Some("0"), None, None, j.segments.len())
            .unwrap();
        assert_eq!(r, (0, 0));
    }

    #[test]
    fn segment_range_from_phantom_is_full() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records);
        // `from 0` spans everything since the base — i.e. the full range `..`.
        let r = j
            .markers
            .segment_range(None, Some("0"), None, j.segments.len())
            .unwrap();
        assert_eq!(r, (0, j.segments.len()));
    }

    #[test]
    fn segment_range_from_after_to_is_error() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c3".into(),
            }),
        ];
        let j = Journal::new(records);
        let result = j
            .markers
            .segment_range(None, Some("c3"), Some("c1"), j.segments.len());
        assert!(result.is_err(), "from > to should be an error");
    }

    #[test]
    fn segment_range_at_first_snapshot() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records);
        let (start, end) = j
            .markers
            .segment_range(Some("c1"), None, None, j.segments.len())
            .unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn segment_range_at_middle_snapshot() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c3".into(),
            }),
        ];
        let j = Journal::new(records);
        // at=c2: the previous snapshot is P1 (marker 1), so start=1, end=2
        let (start, end) = j
            .markers
            .segment_range(Some("c2"), None, None, j.segments.len())
            .unwrap();
        assert_eq!(start, 1);
        assert_eq!(end, 2);
    }

    #[test]
    fn segment_range_at_snapshot_after_travel() {
        // P1 [A] P2 [B] P3 T4(P2) [D] P5
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        // at=c5: the previous snapshot is P3 (marker 2), so start=3, end=5
        let (start, end) = j
            .markers
            .segment_range(Some("c5"), None, None, j.segments.len())
            .unwrap();
        assert_eq!(start, 3);
        assert_eq!(end, 5);
    }

    // ── Alive computation tests (migrated from liveness.rs) ──────────

    #[test]
    fn segment_alive_with_travel() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0]);
        assert!(alive[1]);
        assert!(!alive[2]);
        assert!(!alive[3]);
        assert!(alive[4]);
        assert!(alive[5]);
    }

    #[test]
    fn alive_segments_range_ignores_travel_outside_range() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments_range(0..2, 2);
        assert!(alive[0], "seg0 should be alive (T4 outside range)");
        assert!(alive[1], "seg1 should be alive (T4 outside range)");
    }

    #[test]
    fn alive_segments_range_ignores_target_before_window() {
        // P1 [A] P2 [B] P3 T4(→P1): restrict to the window 2..5. T4's target
        // (gen 1) precedes the window start (2), so the bounds guard skips it
        // and both windowed segments stay alive. Exercises the `k_idx < rstart`
        // branch — the old `gen_to_idx`-miss path now has no map to miss.
        let records = vec![
            Record::Marker(Marker::Snapshot { name: "c1".into() }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot { name: "c2".into() }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot { name: "c3".into() }),
            Record::Marker(Marker::Travel { target_gen: 1 }),
        ];
        let j = Journal::new(records);
        // markers [phantom, P1, P2, P3, T4]; segments 0..=4. Window covers segs 2,3.
        let alive = j.markers.alive_segments_range(2..5, j.segments.len());
        assert!(alive[2], "seg2 alive: T4 target precedes the window");
        assert!(alive[3], "seg3 alive: T4 target precedes the window");
    }

    #[test]
    fn corrupt_t_record_skipped_in_alive() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Travel {
                target_gen: 99,
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(
            alive.iter().all(|&a| a),
            "all segments alive when S target is missing"
        );
    }

    // ── Additional alive edge cases (migrated from liveness.rs) ──────

    #[test]
    fn alive_no_travels() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive.iter().all(|&a| a), "no travels means all alive");
    }

    #[test]
    fn alive_empty_journal() {
        let j = Journal::new(vec![]);
        let alive = j.markers.alive_segments(j.segments.len());
        assert_eq!(alive.len(), 1);
        assert!(alive[0]);
    }

    #[test]
    fn alive_multiple_travels_last_wins() {
        // P1 [A] P2 [B] P3 T4(P2) [D] P5 T6(P1)
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c5".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive (before P1)");
        assert!(!alive[1], "seg1 dead");
        assert!(!alive[2], "seg2 dead");
        assert!(!alive[3], "seg3 dead");
        assert!(!alive[4], "seg4 dead");
        assert!(!alive[5], "seg5 dead");
        assert!(alive[6], "seg6 alive (trailing, empty)");
    }

    #[test]
    fn alive_consecutive_t_records() {
        // P1 [A] P2 [B] P3 T4(P2) T5(P1)
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 2,
            }),
            Record::Marker(Marker::Travel {
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(!alive[1], "seg1 dead (after P1, killed by T5)");
    }

    #[test]
    fn alive_travel_to_first_snapshot() {
        // P1 [A] P2 T3(P1)
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 before P1 alive");
        assert!(!alive[1], "seg1 dead (after P1, before P2)");
        assert!(!alive[2], "seg2 dead");
        assert!(alive[3], "seg3 alive (trailing, empty)");
    }

    #[test]
    fn alive_nested_t_in_dead_zone() {
        // P1 [A] P2 [B] P3 T4(P1) [D] P5 [E] P6 T7(P5)
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c5".into(),
            }),
            Record::Action(Action::Stage {
                path: "/e".into(),
                ino: 4,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c6".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 5,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(!alive[1], "seg1 dead (killed by T4)");
        assert!(!alive[2], "seg2 dead (killed by T4)");
        assert!(!alive[3], "seg3 dead (killed by T4)");
        assert!(alive[4], "seg4 alive (after T4, before P5)");
        assert!(!alive[5], "seg5 dead (killed by T7)");
        assert!(!alive[6], "seg6 dead (killed by T7)");
        assert!(alive[7], "seg7 alive (trailing, empty)");
    }

    #[test]
    fn alive_undo_travel() {
        // P1 [A] P2 [B] P3 T4(P1) [D] P5 T6(P3)
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c5".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 3,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(alive[1], "seg1 alive (traveled by T6)");
        assert!(alive[2], "seg2 alive (traveled by T6)");
        assert!(!alive[3], "seg3 dead (T4 segment)");
        assert!(!alive[4], "seg4 dead (killed by T6)");
        assert!(!alive[5], "seg5 dead (killed by T6)");
        assert!(alive[6], "seg6 alive (trailing, empty)");
    }

    // ── find_marker tests for travel markers ───────────────────────

    #[test]
    fn find_marker_accepts_travel_gen_id() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        assert_eq!(j.markers.find_marker("3").unwrap(), 3);
        assert_eq!(j.markers.find_marker("1").unwrap(), 1);
        assert_eq!(j.markers.find_marker("c1").unwrap(), 1);
        // gen 0 is the base "(initial)" marker, resolvable like its name.
        assert_eq!(j.markers.find_marker("0").unwrap(), 0);
        // Out-of-range ids still error.
        assert!(j.markers.find_marker("99").is_err());
    }

    #[test]
    fn alive_travel_to_travel_marker() {
        // P1 [A] P2 T3(→P1) [D] P4 T5(→T3)
        // T5 targets T3, so dead zone is 3..5 (seg_3, seg_4).
        let records = vec![
            Record::Marker(Marker::Snapshot {
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c2".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
                pre: Target::Absence,
            }),
            Record::Marker(Marker::Snapshot {
                name: "c4".into(),
            }),
            Record::Marker(Marker::Travel {
                target_gen: 3,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive (before P1)");
        assert!(!alive[1], "seg1 dead (killed by T3)");
        assert!(!alive[2], "seg2 dead (killed by T3)");
        assert!(!alive[3], "seg3 dead (killed by T5)");
        assert!(!alive[4], "seg4 dead (killed by T5)");
        assert!(alive[5], "seg5 alive (trailing)");
    }
}
