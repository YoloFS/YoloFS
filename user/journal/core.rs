// agfs CLI — journal/core.rs
//
// The Journal: segments + metas + precomputed liveness.
// Borrowing filter methods — no moves, no collects, no intermediate allocations.

use super::meta::MetaIndex;
use super::parse;
use super::tree::DirTree;
use super::types::*;
use anyhow::Result;
use std::path::Path;

/// All segments + M/J skeleton + precomputed alive mask.
pub struct Journal {
    pub segments: Vec<Segment>,
    pub metas: MetaIndex,
    alive: Vec<bool>,
}

impl Journal {
    /// Build from parsed journal records.
    pub fn new(records: Vec<Record>) -> Self {
        let mut segments = Vec::new();
        let mut metas_vec: Vec<Meta> = vec![Meta::Mark {
            gen_id: 0,
            name: "(initial)".into(),
        }];
        let mut current_records: Vec<Action> = Vec::new();
        let mut current_from: u64 = 0;

        for record in records.into_iter() {
            match record {
                Record::Meta(meta @ Meta::Mark { gen_id, .. }) => {
                    segments.push(Segment {
                        from: current_from,
                        records: std::mem::take(&mut current_records),
                    });
                    current_from = gen_id;
                    metas_vec.push(meta);
                }
                Record::Meta(meta @ Meta::Jump { target_gen, .. }) => {
                    segments.push(Segment {
                        from: current_from,
                        records: std::mem::take(&mut current_records),
                    });
                    metas_vec.push(meta);
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

        let metas = MetaIndex::new(metas_vec);
        let alive = metas.alive_segments(segments.len());

        Journal {
            segments,
            metas,
            alive,
        }
    }

    /// Read and parse the journal file, then build a Journal.
    pub fn read(agfs_dir: &Path) -> Result<Self> {
        Ok(Self::new(parse::read(agfs_dir)?))
    }

    /// Whether the segment with this gen_id is alive (for audit/timeline display).
    pub fn is_alive(&self, gen_id: usize) -> bool {
        self.alive.get(gen_id).copied().unwrap_or(false)
    }

    /// Consume the journal and build a DirTree from all live segments.
    pub fn into_tree(self) -> DirTree {
        DirTree::build(self.into_live_segments())
    }

    /// Consume the journal and build a DirTree from live segments up to a mark.
    pub fn into_tree_at(self, gen_id: u64) -> DirTree {
        DirTree::build(self.into_live_segments_at(gen_id))
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

    // ── Private helpers ──────────────────────────────────────────────

    fn into_live_segments(self) -> impl Iterator<Item = Segment> {
        let len = self.segments.len();
        self.into_live_segments_range(0, len)
    }

    fn into_live_segments_at(self, gen_id: u64) -> impl Iterator<Item = Segment> {
        let num_prefix = (gen_id as usize).min(self.segments.len());
        // Include one extra meta so that a jump meta at gen_id
        // participates in the dead-zone scan.
        let meta_end = (gen_id as usize + 1).min(self.metas.len());
        let alive = self.metas.alive_segments_range(0..meta_end, num_prefix);
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

    // ── Segmentation tests (migrated from segment.rs) ────────────────

    #[test]
    fn segmentation_basic() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
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
    fn segmentation_splits_at_j_boundary() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 6);
        assert_eq!(j.segments[4].from, 2);
        assert_eq!(j.segments[4].records.len(), 1);
        assert!(matches!(&j.segments[4].records[0], Action::Stage { path, .. } if path == "/d"));
    }

    #[test]
    fn records_before_first_mark_in_segment_zero() {
        let records = vec![
            Record::Action(Action::Stage {
                path: "/orphan".into(),
                dtype: Some(libc::DT_REG),
                ino: 999,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 3);
        assert_eq!(j.segments[0].from, 0);
        assert_eq!(j.segments[0].records.len(), 1);
        assert!(
            matches!(&j.segments[0].records[0], Action::Stage { path, .. } if path == "/orphan")
        );
        assert_eq!(j.segments[1].records.len(), 1);
        assert!(matches!(&j.segments[1].records[0], Action::Stage { path, .. } if path == "/a"));
    }

    #[test]
    fn segmentation_empty_journal() {
        let j = Journal::new(vec![]);
        assert_eq!(j.segments.len(), 1);
        assert_eq!(j.segments[0].from, 0);
        assert!(j.segments[0].records.is_empty());
        assert_eq!(j.metas.len(), 1, "phantom meta only");
    }

    #[test]
    fn segmentation_only_j_records() {
        let records = vec![Record::Meta(Meta::Jump {
            gen_id: 1,
            target_gen: 99,
        })];
        let j = Journal::new(records);
        assert_eq!(j.segments.len(), 2);
        assert_eq!(j.segments[0].from, 0);
        assert_eq!(j.segments[1].from, 99);
        assert_eq!(j.metas.len(), 2);
    }

    // ── Live segments tests (migrated from liveness.rs) ──────────────

    #[test]
    fn live_segments_basic() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
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
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        // Live prefix up to M2: seg0 (empty) + seg1 ([A])
        let live: Vec<_> = j.into_live_segments_at(2).collect();
        let actions: Vec<_> = live.iter().flat_map(|s| &s.records).collect();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Stage { path, .. } if path == "/a"));
    }

    // ── Reachability tests (via live_segments, migrated from liveness.rs) ──

    fn live_actions(records: Vec<Record>) -> Vec<String> {
        let j = Journal::new(records);
        j.into_live_segments_range(0, usize::MAX)
            .flat_map(|s| s.records)
            .map(|a| match a {
                Action::Stage { path, .. } => path,
                Action::Delete { path, .. } => path,
                Action::Rename { dst, .. } => dst,
            })
            .collect()
    }

    #[test]
    fn reachable_no_jumps() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        // 3 segments (seg0 empty, seg1 [/a], seg2 empty), all alive, 1 action
        let j = Journal::new(records);
        assert_eq!(j.into_live_segments_range(0, usize::MAX).count(), 3);
    }

    #[test]
    fn reachable_multiple_jumps_last_wins() {
        // M1 [A] M2 [B] M3 J4(M2) [D] M5 J6(M1)
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 6,
                target_gen: 1,
            }),
        ];
        let actions = live_actions(records);
        assert!(
            actions.is_empty(),
            "last jump to M1 kills everything after M1"
        );
    }

    #[test]
    fn reachable_nested_j_in_dead_zone() {
        // M1 [A] M2 [B] M3 J4(M1) [D] M5 [E] M6 J7(M5)
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Action(Action::Stage {
                path: "/e".into(),
                dtype: Some(libc::DT_REG),
                ino: 4,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 6,
                name: "c6".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 7,
                target_gen: 5,
            }),
        ];
        let actions = live_actions(records);
        assert_eq!(actions, vec!["/d"]);
    }

    #[test]
    fn reachable_undo_jump() {
        // M1 [A] M2 [B] M3 J4(M1) [D] M5 J6(M3)
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 6,
                target_gen: 3,
            }),
        ];
        let actions = live_actions(records);
        assert_eq!(actions, vec!["/a", "/b"]);
    }

    #[test]
    fn reachable_jump_to_first_mark() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 3,
                target_gen: 1,
            }),
        ];
        let actions = live_actions(records);
        assert!(
            actions.is_empty(),
            "jump to first mark discards all actions"
        );
    }

    #[test]
    fn reachable_consecutive_j_records() {
        // M1 [A] M2 [B] M3 J4(M2) J5(M1)
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Meta(Meta::Jump {
                gen_id: 5,
                target_gen: 1,
            }),
        ];
        let actions = live_actions(records);
        assert!(actions.is_empty(), "consecutive jumps: last one (M1) wins");
    }

    #[test]
    fn reachable_single_jump() {
        // M1 [A] M2 [B] M3 J4(M2) [D] M5
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 4,
                target_gen: 2,
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
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
    fn reachable_corrupt_j_record_skipped() {
        // M1 [A] J2(M99) [B]
        // J targets nonexistent M99 — skipped, all segments alive.
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Jump {
                gen_id: 2,
                target_gen: 99,
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
        ];
        let actions = live_actions(records);
        assert_eq!(actions, vec!["/a", "/b"]);
    }

    // ── Slice / prefix tests (migrated from liveness.rs) ─────────────

    #[test]
    fn live_segments_slice_from_to() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Action(Action::Stage {
                path: "/c".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 4,
                name: "c4".into(),
            }),
        ];
        let j = Journal::new(records);
        let num = j.segments.len();
        let (start, end) = j
            .metas
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
        assert!(matches!(&live[0].records[0], Action::Stage { path, .. } if path == "/b"));
    }

    #[test]
    fn live_segments_slice_not_found() {
        let records = vec![Record::Meta(Meta::Mark {
            gen_id: 1,
            name: "init".into(),
        })];
        let j = Journal::new(records);
        assert!(
            j.metas
                .segment_range(Some("nonexistent"), None, None, j.segments.len())
                .is_err()
        );
    }

    #[test]
    fn live_segments_at_with_nested_jumps() {
        // M1 [A] M2 [B] M3 J4(M1) [C] M5 [D] M6
        // live_segments_at(M5): prefix metas M1,M2,M3,J4.
        // J4(M1) kills seg1,seg2,seg3 → live: seg0 (empty) + seg4 ([C])
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 4,
                target_gen: 1,
            }),
            Record::Action(Action::Stage {
                path: "/c".into(),
                dtype: Some(libc::DT_REG),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Action(Action::Stage {
                path: "/d".into(),
                dtype: Some(libc::DT_REG),
                ino: 4,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 6,
                name: "c6".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.metas.find_meta("c5").unwrap();
        let live: Vec<_> = j.into_live_segments_at(gen_id).collect();
        let actions: Vec<_> = live.iter().flat_map(|s| &s.records).collect();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Stage { path, .. } if path == "/c"));
    }

    #[test]
    fn live_segments_at_clamps_invalid_gen_id() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
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
    fn into_tree_at_jump_meta() {
        // [A:/a] M1 [B:/b] M2 J3(→M1)
        // into_tree_at(3) should give the journal state at position 3.
        let records = vec![
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Meta(Meta::Jump {
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
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                dtype: Some(libc::DT_REG),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Meta(Meta::Jump {
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
