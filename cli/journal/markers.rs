// agfs CLI — journal/markers.rs
//
// The CKP/RST skeleton: checkpoint and restore records extracted from the journal.
// Provides lookup by gen_id or name, and segment range computation.

use super::types::*;
use anyhow::Result;

/// The CKP/RST skeleton of the journal.
pub struct Markers(pub(super) Vec<Marker>);

impl Markers {
    pub(super) fn new(markers: Vec<Marker>) -> Self {
        Markers(markers)
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
    /// via `atomic64_inc_return()` on every K and T record, so gen_id values
    /// are strictly sequential — marker\[i\] has gen_id = i.
    fn find_marker_by_gen_id(&self, gen_id: u64) -> Result<u64> {
        if gen_id == 0 {
            anyhow::bail!("marker not found: {gen_id}");
        }
        let idx = gen_id as usize;
        match self.0.get(idx) {
            Some(Marker::Checkpoint { gen_id: g, .. } | Marker::Restore { gen_id: g, .. })
                if *g == gen_id =>
            {
                Ok(*g)
            }
            _ => anyhow::bail!("marker not found: {gen_id}"),
        }
    }

    /// Find a checkpoint by name. Returns the last match (names may repeat).
    fn find_checkpoint_by_name(&self, name: &str) -> Result<u64> {
        let mut last = None;
        for marker in self.0.iter() {
            if let Marker::Checkpoint { gen_id, name: n } = marker
                && n == name
            {
                last = Some(*gen_id);
            }
        }
        last.ok_or_else(|| anyhow::anyhow!("marker not found: {name}"))
    }

    /// Find a marker (checkpoint or restore) by name or numeric ID.
    /// Names only match checkpoints (restore markers have no names).
    pub fn find_marker(&self, name_or_id: &str) -> Result<u64> {
        if let Ok(id) = name_or_id.parse::<u64>() {
            return self.find_marker_by_gen_id(id);
        }
        self.find_checkpoint_by_name(name_or_id)
    }

    /// Get the marker at this index (returns `None` for the phantom
    /// marker at index 0).
    pub fn marker_at(&self, marker_idx: usize) -> Option<&Marker> {
        let m = self.0.get(marker_idx)?;
        match m {
            Marker::Checkpoint { gen_id, .. } | Marker::Restore { gen_id, .. }
                if *gen_id > 0 =>
            {
                Some(m)
            }
            _ => None,
        }
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
            let gen_id = self.find_marker(name)?;
            let m_idx = gen_id as usize;
            // Find the previous checkpoint marker (skip phantom at index 0).
            let prev_k = (1..m_idx)
                .rev()
                .find(|&i| matches!(&self.0[i], Marker::Checkpoint { .. }));
            let start = prev_k.unwrap_or(0);
            return Ok((start, m_idx));
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
            anyhow::bail!("invalid range: --from checkpoint comes after --to checkpoint");
        }

        Ok((start, end))
    }

    // ── Liveness computation ─────────────────────────────────────────

    /// Compute alive flags for all segments.
    ///
    /// Walks restore (RST) markers right-to-left. Each RST(target_gen) kills
    /// segments between the target checkpoint and the RST marker.
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
                Marker::Checkpoint { gen_id, .. } | Marker::Restore { gen_id, .. } => *gen_id,
            };
            gen_to_idx.insert(id, i);
        }

        let mut alive = vec![true; num_segments];
        let mut alive_end = range.end;

        for m in range.rev() {
            let Marker::Restore { target_gen, .. } = &self.0[m] else {
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

impl IntoIterator for Markers {
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
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "first".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "second".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.markers.find_marker_by_gen_id(1).unwrap();
        assert_eq!(gen_id, 1);
    }

    #[test]
    fn find_checkpoint_by_name() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "first".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "second".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.markers.find_checkpoint_by_name("second").unwrap();
        assert_eq!(gen_id, 2);
    }

    #[test]
    fn find_checkpoint_not_found() {
        let records = vec![Record::Marker(Marker::Checkpoint {
            gen_id: 1,
            name: "first".into(),
        })];
        let j = Journal::new(records);
        assert!(j.markers.find_checkpoint_by_name("nonexistent").is_err());
    }

    #[test]
    fn find_checkpoint_duplicate_names_returns_last() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "dup".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "dup".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.markers.find_checkpoint_by_name("dup").unwrap();
        assert_eq!(gen_id, 2, "should return the last matching checkpoint");
    }

    #[test]
    fn marker_at_on_restore_marker() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Marker(Marker::Restore {
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
            "Restore marker should be returned by marker_at"
        );
    }

    #[test]
    fn find_marker_by_gen_id_rejects_phantom() {
        let records = vec![Record::Marker(Marker::Checkpoint {
            gen_id: 1,
            name: "c1".into(),
        })];
        let j = Journal::new(records);
        assert!(
            j.markers.find_marker_by_gen_id(0).is_err(),
            "phantom gen_id=0 should not be a valid marker"
        );
        assert!(j.markers.find_marker_by_gen_id(1).is_ok());
    }

    #[test]
    fn restore_targeting_phantom() {
        // Restore to gen_id=0 (initial state) should kill all segments
        // between the phantom and the restore marker.
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Restore {
                gen_id: 3,
                target_gen: 0,
            }),
        ];
        let j = Journal::new(records);
        // markers: [phantom(0), K(1), K(2), R(3→0)]
        // segments: [seg0, seg1, seg2, seg3]
        // Restore to 0 kills segments 0..3 → seg0, seg1, seg2 dead.
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(!alive[0], "seg0 killed by restore to initial");
        assert!(!alive[1], "seg1 killed by restore to initial");
        assert!(!alive[2], "seg2 killed by restore to initial");
        assert!(alive[3], "seg3 (trailing after restore) alive");
    }

    #[test]
    fn segment_range_at_rejects_phantom_id() {
        let records = vec![Record::Marker(Marker::Checkpoint {
            gen_id: 1,
            name: "c1".into(),
        })];
        let j = Journal::new(records);
        assert!(
            j.markers
                .segment_range(Some("0"), None, None, j.segments.len())
                .is_err(),
            "--at 0 should be rejected (phantom)"
        );
    }

    #[test]
    fn segment_range_from_after_to_is_error() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Checkpoint {
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
    fn segment_range_at_first_checkpoint() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
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
    fn segment_range_at_middle_checkpoint() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
        ];
        let j = Journal::new(records);
        // --at c2: prev K is marker 0 (K1), so start=1, end=2
        let (start, end) = j
            .markers
            .segment_range(Some("c2"), None, None, j.segments.len())
            .unwrap();
        assert_eq!(start, 1);
        assert_eq!(end, 2);
    }

    #[test]
    fn segment_range_at_checkpoint_after_restore() {
        // K1 [A] K2 [B] K3 S4(K2) [D] K5
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Add {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        // --at c5: prev K is marker[2] (K3), so start=3, end=5
        let (start, end) = j
            .markers
            .segment_range(Some("c5"), None, None, j.segments.len())
            .unwrap();
        assert_eq!(start, 3);
        assert_eq!(end, 5);
    }

    // ── Alive computation tests (migrated from liveness.rs) ──────────

    #[test]
    fn segment_alive_with_restore() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Add {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Marker(Marker::Checkpoint {
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
    fn alive_segments_range_ignores_restore_outside_range() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 4,
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments_range(0..2, 2);
        assert!(alive[0], "seg0 should be alive (S4 outside range)");
        assert!(alive[1], "seg1 should be alive (S4 outside range)");
    }

    #[test]
    fn corrupt_s_record_skipped_in_alive() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Restore {
                gen_id: 2,
                target_gen: 99,
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
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
    fn alive_no_restores() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive.iter().all(|&a| a), "no restores means all alive");
    }

    #[test]
    fn alive_empty_journal() {
        let j = Journal::new(vec![]);
        let alive = j.markers.alive_segments(j.segments.len());
        assert_eq!(alive.len(), 1);
        assert!(alive[0]);
    }

    #[test]
    fn alive_multiple_restores_last_wins() {
        // K1 [A] K2 [B] K3 S4(K2) [D] K5 S6(K1)
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Add {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 6,
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive (before K1)");
        assert!(!alive[1], "seg1 dead");
        assert!(!alive[2], "seg2 dead");
        assert!(!alive[3], "seg3 dead");
        assert!(!alive[4], "seg4 dead");
        assert!(!alive[5], "seg5 dead");
        assert!(alive[6], "seg6 alive (trailing, empty)");
    }

    #[test]
    fn alive_consecutive_s_records() {
        // K1 [A] K2 [B] K3 S4(K2) S5(K1)
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Marker(Marker::Restore {
                gen_id: 5,
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(!alive[1], "seg1 dead (after K1, killed by S5)");
    }

    #[test]
    fn alive_restore_to_first_checkpoint() {
        // K1 [A] K2 S3(K1)
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 3,
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 before K1 alive");
        assert!(!alive[1], "seg1 dead (after K1, before K2)");
        assert!(!alive[2], "seg2 dead");
        assert!(alive[3], "seg3 alive (trailing, empty)");
    }

    #[test]
    fn alive_nested_s_in_dead_zone() {
        // K1 [A] K2 [B] K3 S4(K1) [D] K5 [E] K6 S7(K5)
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Add {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Action(Action::Add {
                path: "/e".into(),
                dtype: Some(libc::DT_REG),
                ino: 4,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 6,
                name: "c6".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 7,
                target_gen: 5,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(!alive[1], "seg1 dead (killed by S4)");
        assert!(!alive[2], "seg2 dead (killed by S4)");
        assert!(!alive[3], "seg3 dead (killed by S4)");
        assert!(alive[4], "seg4 alive (after S4, before K5)");
        assert!(!alive[5], "seg5 dead (killed by S7)");
        assert!(!alive[6], "seg6 dead (killed by S7)");
        assert!(alive[7], "seg7 alive (trailing, empty)");
    }

    #[test]
    fn alive_undo_restore() {
        // K1 [A] K2 [B] K3 S4(K1) [D] K5 S6(K3)
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Add {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Add {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 6,
                target_gen: 3,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(alive[1], "seg1 alive (restored by S6)");
        assert!(alive[2], "seg2 alive (restored by S6)");
        assert!(!alive[3], "seg3 dead (S4 segment)");
        assert!(!alive[4], "seg4 dead (killed by S6)");
        assert!(!alive[5], "seg5 dead (killed by S6)");
        assert!(alive[6], "seg6 alive (trailing, empty)");
    }

    // ── find_marker tests for restore markers ───────────────────────

    #[test]
    fn find_marker_accepts_restore_gen_id() {
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 3,
                target_gen: 1,
            }),
        ];
        let j = Journal::new(records);
        assert_eq!(j.markers.find_marker("3").unwrap(), 3);
        assert_eq!(j.markers.find_marker("1").unwrap(), 1);
        assert_eq!(j.markers.find_marker("c1").unwrap(), 1);
        assert!(j.markers.find_marker("0").is_err());
    }

    #[test]
    fn alive_restore_to_restore_marker() {
        // K1 [A] K2 R3(→K1) [D] K4 R5(→R3)
        // R5 targets R3, so dead zone is 3..5 (seg_3, seg_4).
        let records = vec![
            Record::Marker(Marker::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Add {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 3,
                target_gen: 1,
            }),
            Record::Action(Action::Add {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Marker(Marker::Checkpoint {
                gen_id: 4,
                name: "c4".into(),
            }),
            Record::Marker(Marker::Restore {
                gen_id: 5,
                target_gen: 3,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive (before K1)");
        assert!(!alive[1], "seg1 dead (killed by R3)");
        assert!(!alive[2], "seg2 dead (killed by R3)");
        assert!(!alive[3], "seg3 dead (killed by R5)");
        assert!(!alive[4], "seg4 dead (killed by R5)");
        assert!(alive[5], "seg5 alive (trailing)");
    }
}
