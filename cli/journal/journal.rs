// agfs CLI — journal/journal.rs
//
// The Journal: segments + markers + precomputed liveness.
// Borrowing filter methods — no moves, no collects, no intermediate allocations.

use super::markers::Markers;
use super::parse;
use super::types::*;
use anyhow::Result;
use std::path::Path;

/// All segments + K/T skeleton + precomputed alive mask.
pub struct Journal {
    pub segments: Vec<Segment>,
    pub markers: Markers,
    alive: Vec<bool>,
}

impl Journal {
    /// Build from parsed journal records.
    pub fn new(records: Vec<Record>) -> Self {
        let mut segments = Vec::new();
        let mut markers_vec: Vec<Marker> = Vec::new();
        let mut current_records: Vec<Action> = Vec::new();
        let mut current_from: u64 = 0;

        for record in records.into_iter() {
            match record {
                Record::Marker(marker @ Marker::Checkpoint { gen_id, .. }) => {
                    segments.push(Segment {
                        from: current_from,
                        records: std::mem::take(&mut current_records),
                    });
                    current_from = gen_id;
                    markers_vec.push(marker);
                }
                Record::Marker(marker @ Marker::Restore { target_gen, .. }) => {
                    segments.push(Segment {
                        from: current_from,
                        records: std::mem::take(&mut current_records),
                    });
                    markers_vec.push(marker);
                    current_from = target_gen;
                }
                Record::Action(action) => {
                    current_records.push(action);
                }
            }
        }

        // Trailing segment.
        segments.push(Segment {
            from: current_from,
            records: current_records,
        });

        let markers = Markers::new(markers_vec);
        let alive = markers.alive_segments(segments.len());

        Journal {
            segments,
            markers,
            alive,
        }
    }

    /// Read and parse the journal file, then build a Journal.
    pub fn read(agfs_dir: &Path) -> Result<Self> {
        Ok(Self::new(parse::read(agfs_dir)?))
    }

    /// All live segments (filtered by precomputed alive mask).
    pub fn live_segments(&self) -> impl Iterator<Item = &Segment> {
        self.segments
            .iter()
            .enumerate()
            .filter(|(i, _)| self.alive[*i])
            .map(|(_, s)| s)
    }

    /// Live segments up to a checkpoint (by gen_id).
    /// Computes its own alive mask scoped to the prefix, because
    /// restore records after the prefix boundary do not affect it.
    ///
    /// Callers must ensure gen_id ≤ markers.len() (use `live_segments_at_name`
    /// for a safe wrapper that validates by name).
    pub fn live_segments_at(&self, gen_id: u64) -> impl Iterator<Item = &Segment> {
        let num_prefix = (gen_id as usize).min(self.markers.len());
        let alive = self.markers.alive_segments_range(0..num_prefix, num_prefix);
        self.segments
            .iter()
            .enumerate()
            .take(num_prefix)
            .filter(move |(i, _)| alive[*i])
            .map(|(_, s)| s)
    }

    /// Whether segment at this index is alive (for audit/timeline display).
    pub fn is_alive(&self, segment_index: usize) -> bool {
        self.alive.get(segment_index).copied().unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Segmentation tests (migrated from segment.rs) ────────────────

    #[test]
    fn segmentation_basic() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
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
    fn segmentation_splits_at_s_boundary() {
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
        assert_eq!(j.segments.len(), 6);
        assert_eq!(j.segments[4].from, 2);
        assert_eq!(j.segments[4].records.len(), 1);
        assert!(matches!(&j.segments[4].records[0], Action::Add { path, .. } if path == "/d"));
    }

    #[test]
    fn records_before_first_checkpoint_in_segment_zero() {
        let records = vec![
            Record::Action(Action::Add { path: "/orphan".into(), dtype: Some(DType::File), ino: 999 }),
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
        ];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 3);
        assert_eq!(j.segments[0].from, 0);
        assert_eq!(j.segments[0].records.len(), 1);
        assert!(matches!(&j.segments[0].records[0], Action::Add { path, .. } if path == "/orphan"));
        assert_eq!(j.segments[1].records.len(), 1);
        assert!(matches!(&j.segments[1].records[0], Action::Add { path, .. } if path == "/a"));
    }

    #[test]
    fn segmentation_empty_journal() {
        let j = Journal::new(vec![]);
        assert_eq!(j.segments.len(), 1);
        assert_eq!(j.segments[0].from, 0);
        assert!(j.segments[0].records.is_empty());
        assert!(j.markers.is_empty());
    }

    #[test]
    fn segmentation_only_s_records() {
        let records = vec![Record::Marker(Marker::Restore { gen_id: 1, target_gen: 99 })];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 2);
        assert_eq!(j.segments[0].from, 0);
        assert_eq!(j.segments[1].from, 99);
        assert_eq!(j.markers.len(), 1);
    }

    // ── Live segments tests (migrated from liveness.rs) ──────────────

    #[test]
    fn live_segments_basic() {
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
        let live: Vec<_> = j.live_segments().collect();
        // seg0, seg1 alive; seg2,seg3 dead; seg4,seg5 alive → 4 live
        assert_eq!(live.len(), 4);
    }

    #[test]
    fn live_segments_at_basic() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Marker(Marker::Restore { gen_id: 4, target_gen: 1 }),
            Record::Action(Action::Add { path: "/d".into(), dtype: Some(DType::File), ino: 3 }),
            Record::Marker(Marker::Checkpoint { gen_id: 5, name: "c5".into() }),
        ];
        let j = Journal::new(records);
        // Live prefix up to K2: seg0 (empty) + seg1 ([A])
        let live: Vec<_> = j.live_segments_at(2).collect();
        let actions: Vec<_> = live.iter().flat_map(|s| &s.records).collect();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Add { path, .. } if path == "/a"));
    }

    // ── Reachability tests (via live_segments, migrated from liveness.rs) ──

    fn live_actions(records: Vec<Record>) -> Vec<&'static str> {
        let j = Journal::new(records);
        j.live_segments()
            .flat_map(|s| &s.records)
            .map(|a| match a {
                Action::Add { path, .. } => Box::leak(path.clone().into_boxed_str()) as &str,
                Action::Modify { path, .. } => Box::leak(path.clone().into_boxed_str()) as &str,
                Action::Delete { path, .. } => Box::leak(path.clone().into_boxed_str()) as &str,
                Action::Rename { dst, .. } => Box::leak(dst.clone().into_boxed_str()) as &str,
                Action::Replace { dst, .. } => Box::leak(dst.clone().into_boxed_str()) as &str,
            })
            .collect()
    }

    fn live_record_count(records: Vec<Record>) -> usize {
        let j = Journal::new(records);
        let actions: usize = j.live_segments().map(|s| s.records.len()).sum();
        let markers = j.live_segments().count().saturating_sub(1); // one marker between each pair
        actions + markers
    }

    #[test]
    fn reachable_no_restores() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
        ];
        // 3 segments (seg0 empty, seg1 [/a], seg2 empty), all alive, 1 action
        let j = Journal::new(records);
        assert_eq!(j.live_segments().count(), 3);
    }

    #[test]
    fn reachable_multiple_restores_last_wins() {
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
        let actions = live_actions(records);
        assert!(actions.is_empty(), "last restore to K1 kills everything after K1");
    }

    #[test]
    fn reachable_nested_s_in_dead_zone() {
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
        let actions = live_actions(records);
        assert_eq!(actions, vec!["/d"]);
    }

    #[test]
    fn reachable_undo_restore() {
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
        let actions = live_actions(records);
        assert_eq!(actions, vec!["/a", "/b"]);
    }

    #[test]
    fn reachable_restore_to_first_checkpoint() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "c1".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Marker(Marker::Restore { gen_id: 3, target_gen: 1 }),
        ];
        let actions = live_actions(records);
        assert!(actions.is_empty(), "restore to first checkpoint discards all actions");
    }

    #[test]
    fn reachable_consecutive_s_records() {
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
        let actions = live_actions(records);
        assert!(actions.is_empty(), "consecutive restores: last one (K1) wins");
    }

    // ── Slice / prefix tests (migrated from liveness.rs) ─────────────

    #[test]
    fn live_segments_slice_from_to() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Action(Action::Add { path: "/c".into(), dtype: Some(DType::File), ino: 3 }),
            Record::Marker(Marker::Checkpoint { gen_id: 4, name: "c4".into() }),
        ];
        let j = Journal::new(records);
        let num = j.segments.len();
        let (start, end) = j.markers.segment_range(None, Some("c2"), Some("c3"), num).unwrap();
        let live: Vec<_> = j.segments[start..end]
            .iter()
            .enumerate()
            .filter(|(i, _)| j.is_alive(start + i))
            .map(|(_, s)| s)
            .collect();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].from, 2);
        assert!(matches!(&live[0].records[0], Action::Add { path, .. } if path == "/b"));
    }

    #[test]
    fn live_segments_slice_not_found() {
        let records = vec![Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() })];
        let j = Journal::new(records);
        assert!(j.markers.segment_range(Some("nonexistent"), None, None, j.segments.len()).is_err());
    }

    #[test]
    fn live_segments_at_with_nested_restores() {
        // K1 [A] K2 [B] K3 S4(K1) [C] K5 [D] K6
        // live_segments_at(K5): prefix markers K1,K2,K3,S4.
        // S4(K1) kills seg1,seg2,seg3 → live: seg0 (empty) + seg4 ([C])
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Marker(Marker::Restore { gen_id: 4, target_gen: 1 }),
            Record::Action(Action::Add { path: "/c".into(), dtype: Some(DType::File), ino: 3 }),
            Record::Marker(Marker::Checkpoint { gen_id: 5, name: "c5".into() }),
            Record::Action(Action::Add { path: "/d".into(), dtype: Some(DType::File), ino: 4 }),
            Record::Marker(Marker::Checkpoint { gen_id: 6, name: "c6".into() }),
        ];
        let j = Journal::new(records);
        let (gen_id, _) = j.markers.find_checkpoint("c5").unwrap();
        let live: Vec<_> = j.live_segments_at(gen_id).collect();
        let actions: Vec<_> = live.iter().flat_map(|s| &s.records).collect();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Add { path, .. } if path == "/c"));
    }

    #[test]
    fn live_segments_at_clamps_invalid_gen_id() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
        ];
        let j = Journal::new(records);
        // gen_id 999 is way beyond markers.len()=2; should clamp, not panic
        let live: Vec<_> = j.live_segments_at(999).collect();
        assert_eq!(live.len(), j.live_segments().count());
    }

    // ── Original tests ───────────────────────────────────────────────

    #[test]
    fn is_alive_basic() {
        let records = vec![
            Record::Marker(Marker::Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Action(Action::Add { path: "/a".into(), dtype: Some(DType::File), ino: 1 }),
            Record::Marker(Marker::Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Action(Action::Add { path: "/b".into(), dtype: Some(DType::File), ino: 2 }),
            Record::Marker(Marker::Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Marker(Marker::Restore { gen_id: 4, target_gen: 2 }),
        ];
        let j = Journal::new(records);
        assert!(j.is_alive(0));
        assert!(j.is_alive(1));
        assert!(!j.is_alive(2));
        assert!(!j.is_alive(3));
        assert!(j.is_alive(4));
    }
}
