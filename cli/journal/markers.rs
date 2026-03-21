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

    /// Find a checkpoint by numeric gen ID. O(1) via direct indexing.
    ///
    /// This relies on the gen_id invariant: the kernel increments `sbi->gen`
    /// via `atomic64_inc_return()` on every K and T record, so gen_id values
    /// are strictly sequential — marker\[i\] has gen_id = i + 1.
    pub fn find_checkpoint_by_gen_id(&self, gen_id: u64) -> Result<(u64, &str)> {
        let idx = gen_id.checked_sub(1).and_then(|i| usize::try_from(i).ok());
        if let Some(idx) = idx {
            if let Some(Marker::Checkpoint { gen_id: g, name }) = self.0.get(idx) {
                if *g == gen_id {
                    return Ok((*g, name));
                }
            }
        }
        anyhow::bail!("checkpoint not found: {gen_id}");
    }

    /// Find a checkpoint by name. Returns the last match (names may repeat).
    pub fn find_checkpoint_by_name(&self, name: &str) -> Result<(u64, &str)> {
        let mut last = None;
        for marker in self.0.iter() {
            if let Marker::Checkpoint { gen_id, name: n } = marker
                && n == name
            {
                last = Some((*gen_id, n.as_str()));
            }
        }
        last.ok_or_else(|| anyhow::anyhow!("checkpoint not found: {name}"))
    }

    /// Find a checkpoint by name or numeric ID (user input).
    pub fn find_checkpoint(&self, name_or_id: &str) -> Result<(u64, &str)> {
        if let Ok(id) = name_or_id.parse::<u64>() {
            return self.find_checkpoint_by_gen_id(id);
        }
        self.find_checkpoint_by_name(name_or_id)
    }

    /// Get the checkpoint at this marker index (returns `None` for restore markers).
    pub fn checkpoint_at(&self, marker_idx: usize) -> Option<(u64, &str)> {
        match self.0.get(marker_idx)? {
            Marker::Checkpoint { gen_id, name } => Some((*gen_id, name)),
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
            let (gen_id, _) = self.find_checkpoint(name)?;
            let m_idx = (gen_id - 1) as usize;
            // Find the previous checkpoint marker to bound the "at" range.
            let prev_k = (0..m_idx)
                .rev()
                .find(|&i| matches!(&self.0[i], Marker::Checkpoint { .. }));
            let start = prev_k.map(|k| k + 1).unwrap_or(0);
            return Ok((start, m_idx + 1));
        }

        let start = if let Some(from_name) = from {
            let (gen_id, _) = self.find_checkpoint(from_name)?;
            gen_id as usize
        } else {
            0
        };

        let end = if let Some(to_name) = to {
            let (gen_id, _) = self.find_checkpoint(to_name)?;
            gen_id as usize
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

        // Build gen_id → marker index lookup for O(1) checkpoint resolution.
        let mut gen_to_idx: std::collections::HashMap<u64, usize> =
            std::collections::HashMap::new();
        for i in range.clone() {
            if let Marker::Checkpoint { gen_id, .. } = &self.0[i] {
                gen_to_idx.insert(*gen_id, i);
            }
        }

        let mut alive = vec![true; num_segments];
        let mut alive_end = num_segments;

        for m in range.rev() {
            let Marker::Restore { target_gen, .. } = &self.0[m] else {
                continue;
            };
            if m + 1 >= alive_end {
                continue;
            }
            let Some(&k_idx) = gen_to_idx.get(target_gen) else {
                continue;
            };
            for flag in &mut alive[(k_idx + 1)..=m] {
                *flag = false;
            }
            alive_end = k_idx + 1;
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
    use crate::journal::journal::Journal;

    // ── Marker lookup tests (migrated from segment.rs) ───────────────

    #[test]
    fn find_checkpoint_by_gen_id() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "first".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "second".into() }),
        ];
        let j = Journal::new(records);
        let (gen_id, _) = j.markers.find_checkpoint_by_gen_id(1).unwrap();
        assert_eq!(gen_id, 1);
    }

    #[test]
    fn find_checkpoint_by_name() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "first".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "second".into() }),
        ];
        let j = Journal::new(records);
        let (gen_id, _) = j.markers.find_checkpoint_by_name("second").unwrap();
        assert_eq!(gen_id, 2);
    }

    #[test]
    fn find_checkpoint_not_found() {
        let records = vec![Record::Marker(Marker::Checkpoint { gen_id: 1, name: "first".into() })];
        let j = Journal::new(records);
        assert!(j.markers.find_checkpoint_by_name("nonexistent").is_err());
    }

    #[test]
    fn find_checkpoint_duplicate_names_returns_last() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "dup".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "dup".into() }),
        ];
        let j = Journal::new(records);
        let (gen_id, _) = j.markers.find_checkpoint_by_name("dup").unwrap();
        assert_eq!(gen_id, 2, "should return the last matching checkpoint");
    }

    #[test]
    fn checkpoint_at_on_restore_marker() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Marker(Marker::Restore { gen_id: 3, target_gen: 1 }),
        ];
        let j = Journal::new(records);
        assert!(j.markers.checkpoint_at(0).is_some());
        assert!(j.markers.checkpoint_at(1).is_some());
        assert!(j.markers.checkpoint_at(2).is_none(), "Restore marker should return None");
    }

    #[test]
    fn segment_range_from_after_to_is_error() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "c1".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
        ];
        let j = Journal::new(records);
        let result = j.markers.segment_range(None, Some("c3"), Some("c1"), j.segments.len());
        assert!(result.is_err(), "from > to should be an error");
    }

    #[test]
    fn segment_range_at_first_checkpoint() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "c1".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
        ];
        let j = Journal::new(records);
        let (start, end) = j.markers.segment_range(Some("c1"), None, None, j.segments.len()).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 1);
    }

    // ── Alive computation tests (migrated from liveness.rs) ──────────

    #[test]
    fn segment_alive_with_restore() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Marker(Marker::Restore { gen_id: 4, target_gen: 2 }),
            Record::Action(Action::Add { path: "/d".into(), dtype: Some(DType::File), ino: 3 }),
            Record::Marker(Marker::Checkpoint { gen_id: 5, name: "c5".into() }),
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
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Marker(Marker::Restore { gen_id: 4, target_gen: 1 }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments_range(0..2, 2);
        assert!(alive[0], "seg0 should be alive (S4 outside range)");
        assert!(alive[1], "seg1 should be alive (S4 outside range)");
    }

    #[test]
    fn corrupt_s_record_skipped_in_alive() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Restore { gen_id: 2, target_gen: 99 }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive.iter().all(|&a| a), "all segments alive when S target is missing");
    }

    // ── Additional alive edge cases (migrated from liveness.rs) ──────

    #[test]
    fn alive_no_restores() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
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
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Marker(Marker::Restore { gen_id: 4, target_gen: 2 }),
            Record::Action(Action::Add { path: "/d".into(), dtype: Some(DType::File), ino: 3 }),
            Record::Marker(Marker::Checkpoint { gen_id: 5, name: "c5".into() }),
            Record::Marker(Marker::Restore { gen_id: 6, target_gen: 1 }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive (before K1)");
        assert!(!alive[1], "seg1 dead");
        assert!(!alive[2], "seg2 dead");
        assert!(!alive[3], "seg3 dead");
        assert!(!alive[4], "seg4 dead");
        assert!(!alive[5], "seg5 dead");
        assert!(!alive[6], "seg6 dead (trailing)");
    }

    #[test]
    fn alive_consecutive_s_records() {
        // K1 [A] K2 [B] K3 S4(K2) S5(K1)
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Marker(Marker::Restore { gen_id: 4, target_gen: 2 }),
            Record::Marker(Marker::Restore { gen_id: 5, target_gen: 1 }),
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
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "c1".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Marker(Marker::Restore { gen_id: 3, target_gen: 1 }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 before K1 alive");
        assert!(!alive[1], "seg1 dead (after K1, before K2)");
        assert!(!alive[2], "seg2 dead");
        assert!(!alive[3], "seg3 dead (after S3)");
    }

    #[test]
    fn alive_nested_s_in_dead_zone() {
        // K1 [A] K2 [B] K3 S4(K1) [D] K5 [E] K6 S7(K5)
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Marker(Marker::Restore { gen_id: 4, target_gen: 1 }),
            Record::Action(Action::Add { path: "/d".into(), dtype: Some(DType::File), ino: 3 }),
            Record::Marker(Marker::Checkpoint { gen_id: 5, name: "c5".into() }),
            Record::Action(Action::Add { path: "/e".into(), dtype: Some(DType::File), ino: 4 }),
            Record::Marker(Marker::Checkpoint { gen_id: 6, name: "c6".into() }),
            Record::Marker(Marker::Restore { gen_id: 7, target_gen: 5 }),
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
        assert!(!alive[7], "seg7 dead (trailing after S7)");
    }

    #[test]
    fn alive_undo_restore() {
        // K1 [A] K2 [B] K3 S4(K1) [D] K5 S6(K3)
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Marker(Marker::Restore { gen_id: 4, target_gen: 1 }),
            Record::Action(Action::Add { path: "/d".into(), dtype: Some(DType::File), ino: 3 }),
            Record::Marker(Marker::Checkpoint { gen_id: 5, name: "c5".into() }),
            Record::Marker(Marker::Restore { gen_id: 6, target_gen: 3 }),
        ];
        let j = Journal::new(records);
        let alive = j.markers.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(alive[1], "seg1 alive (restored by S6)");
        assert!(alive[2], "seg2 alive (restored by S6)");
        assert!(!alive[3], "seg3 dead (S4 segment)");
        assert!(!alive[4], "seg4 dead (killed by S6)");
        assert!(!alive[5], "seg5 dead (killed by S6)");
        assert!(!alive[6], "seg6 dead (trailing after S6)");
    }
}
