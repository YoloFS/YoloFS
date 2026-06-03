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
    /// This relies on the gen_id invariant: the kernel increments `sbi->gen`
    /// via `atomic64_inc_return()` on every P and T record, so gen_id values
    /// are strictly sequential — marker[i] has gen_id = i. gen 0 is the base
    /// "(initial)" marker, addressable like any other (and like its name).
    fn find_marker_by_gen_id(&self, gen_id: u64) -> Result<u64> {
        let idx = gen_id as usize;
        match self.0.get(idx) {
            Some(Marker::Snapshot { gen_id: g, .. } | Marker::Travel { gen_id: g, .. })
                if *g == gen_id =>
            {
                Ok(*g)
            }
            _ => anyhow::bail!("marker not found: {gen_id}"),
        }
    }

    /// Find a snapshot by name. Returns the last match (names may repeat).
    fn find_snapshot_by_name(&self, name: &str) -> Result<u64> {
        let mut last = None;
        for marker in self.0.iter() {
            if let Marker::Snapshot { gen_id, name: n } = marker
                && n == name
            {
                last = Some(*gen_id);
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
        let m = self.0.get(marker_idx)?;
        match m {
            Marker::Snapshot { gen_id, .. } | Marker::Travel { gen_id, .. } if *gen_id > 0 => {
                Some(m)
            }
            _ => None,
        }
    }

    /// Index of the most recent snapshot marker (highest index, gen_id > 0),
    /// or `None` when only the phantom initial marker exists.
    pub fn last_snapshot_idx(&self) -> Option<usize> {
        self.0.iter().enumerate().rev().find_map(|(i, m)| {
            matches!(m, Marker::Snapshot { gen_id, .. } if *gen_id > 0).then_some(i)
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

    /// Compute the segment index range for --at/--from/--to queries.
    /// Returns a half-open range [start, end).
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
            anyhow::bail!("invalid range: --from snapshot comes after --to snapshot");
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

        // Build gen_id → marker index lookup for O(1) resolution.
        let mut gen_to_idx: std::collections::HashMap<u64, usize> =
            std::collections::HashMap::new();
        for i in range.clone() {
            let id = match &self.0[i] {
                Marker::Snapshot { gen_id, .. } | Marker::Travel { gen_id, .. } => *gen_id,
            };
            gen_to_idx.insert(id, i);
        }

        let mut alive = vec![true; num_segments];
        let mut alive_end = range.end;

        for m in range.rev() {
            let Marker::Travel { target_gen, .. } = &self.0[m] else {
                continue;
            };
            if m > alive_end {
                continue;
            }
            let Some(&k_idx) = gen_to_idx.get(target_gen) else {
                continue;
            };
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
                gen_id: 1,
                name: "first".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
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
                gen_id: 1,
                name: "first".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
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
            gen_id: 1,
            name: "first".into(),
        })];
        let j = Journal::new(records);
        assert!(j.markers.find_snapshot_by_name("nonexistent").is_err());
    }

    #[test]
    fn find_snapshot_duplicate_names_returns_last() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "dup".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
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
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
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
            gen_id: 1,
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
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
            }),
            Record::Marker(Marker::Travel {
                gen_id: 3,
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
            gen_id: 1,
            name: "c1".into(),
        })];
        let j = Journal::new(records);
        // `--at 0` (the base) resolves to an empty range — the base introduced
        // no changes, so `status --at 0` shows nothing (no longer an error).
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
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records);
        // `--from 0` spans everything since the base — i.e. the same as `--full`.
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
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
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
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
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
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
        ];
        let j = Journal::new(records);
        // --at c2: prev P is marker 0 (P1), so start=1, end=2
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
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
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
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        // --at c5: prev P is marker[2] (P3), so start=3, end=5
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
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
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
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
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
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 4,
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments_range(0..2, 2);
        assert!(alive[0], "seg0 should be alive (T4 outside range)");
        assert!(alive[1], "seg1 should be alive (T4 outside range)");
    }

    #[test]
    fn corrupt_t_record_skipped_in_alive() {
        let records = vec![
            Record::Marker(Marker::Snapshot {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Travel {
                gen_id: 2,
                target_gen: 99,
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
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
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
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
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
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
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
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
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
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
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
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
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Action(Action::Stage {
                path: "/e".into(),
                ino: 4,
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
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
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
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
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
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 3,
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
            }),
            Record::Marker(Marker::Snapshot {
                gen_id: 4,
                name: "c4".into(),
            }),
            Record::Marker(Marker::Travel {
                gen_id: 5,
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
