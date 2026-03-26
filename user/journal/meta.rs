// agfs CLI — journal/metas.rs
//
// The M/J skeleton: mark and jump records extracted from the journal.
// Provides lookup by gen_id or name, and segment range computation.

use super::types::*;
use anyhow::Result;

/// The M/J skeleton of the journal.
pub struct MetaIndex(pub(super) Vec<Meta>);

impl MetaIndex {
    pub(super) fn new(metas: Vec<Meta>) -> Self {
        MetaIndex(metas)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, idx: usize) -> Option<&Meta> {
        self.0.get(idx)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Meta> {
        self.0.iter()
    }

    /// Find any meta by numeric gen ID. O(1) via direct indexing.
    ///
    /// This relies on the gen_id invariant: the kernel increments `sbi->gen`
    /// via `atomic64_inc_return()` on every M and J record, so gen_id values
    /// are strictly sequential — meta[i] has gen_id = i.
    fn find_meta_by_gen_id(&self, gen_id: u64) -> Result<u64> {
        if gen_id == 0 {
            anyhow::bail!("meta not found: {gen_id}");
        }
        let idx = gen_id as usize;
        match self.0.get(idx) {
            Some(Meta::Mark { gen_id: g, .. } | Meta::Jump { gen_id: g, .. }) if *g == gen_id => {
                Ok(*g)
            }
            _ => anyhow::bail!("meta not found: {gen_id}"),
        }
    }

    /// Find a mark by name. Returns the last match (names may repeat).
    fn find_mark_by_name(&self, name: &str) -> Result<u64> {
        let mut last = None;
        for meta in self.0.iter() {
            if let Meta::Mark { gen_id, name: n } = meta
                && n == name
            {
                last = Some(*gen_id);
            }
        }
        last.ok_or_else(|| anyhow::anyhow!("meta not found: {name}"))
    }

    /// Find a meta (mark or jump) by name or numeric ID.
    /// Names only match marks (jump metas have no names).
    pub fn find_meta(&self, name_or_id: &str) -> Result<u64> {
        if let Ok(id) = name_or_id.parse::<u64>() {
            return self.find_meta_by_gen_id(id);
        }
        self.find_mark_by_name(name_or_id)
    }

    /// Get the meta at this index (returns `None` for the phantom
    /// meta at index 0).
    pub fn meta_at(&self, meta_idx: usize) -> Option<&Meta> {
        let m = self.0.get(meta_idx)?;
        match m {
            Meta::Mark { gen_id, .. } | Meta::Jump { gen_id, .. } if *gen_id > 0 => Some(m),
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
            let gen_id = self.find_meta(name)?;
            let m_idx = gen_id as usize;
            // Find the previous mark meta (skip phantom at index 0).
            let prev_k = (1..m_idx)
                .rev()
                .find(|&i| matches!(&self.0[i], Meta::Mark { .. }));
            let start = prev_k.unwrap_or(0);
            return Ok((start, m_idx));
        }

        let start = if let Some(from_name) = from {
            self.find_meta(from_name)? as usize
        } else {
            0
        };

        let end = if let Some(to_name) = to {
            self.find_meta(to_name)? as usize
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
    /// Walks jump (J) metas right-to-left. Each J(target_gen) kills
    /// segments between the target mark and the J meta.
    pub fn alive_segments(&self, num_segments: usize) -> Vec<bool> {
        self.alive_segments_range(0..self.len(), num_segments)
    }

    /// Compute alive flags for segments using only metas in `range`.
    pub fn alive_segments_range(
        &self,
        range: std::ops::Range<usize>,
        num_segments: usize,
    ) -> Vec<bool> {
        if num_segments == 0 {
            return vec![];
        }

        // Build gen_id → meta index lookup for O(1) resolution.
        let mut gen_to_idx: std::collections::HashMap<u64, usize> =
            std::collections::HashMap::new();
        for i in range.clone() {
            let id = match &self.0[i] {
                Meta::Mark { gen_id, .. } | Meta::Jump { gen_id, .. } => *gen_id,
            };
            gen_to_idx.insert(id, i);
        }

        let mut alive = vec![true; num_segments];
        let mut alive_end = range.end;

        for m in range.rev() {
            let Meta::Jump { target_gen, .. } = &self.0[m] else {
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

impl IntoIterator for MetaIndex {
    type Item = Meta;
    type IntoIter = std::vec::IntoIter<Meta>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::core::Journal;

    // ── Meta lookup tests (migrated from segment.rs) ───────────────

    #[test]
    fn find_meta_by_gen_id() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "first".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "second".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.metas.find_meta_by_gen_id(1).unwrap();
        assert_eq!(gen_id, 1);
    }

    #[test]
    fn find_mark_by_name() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "first".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "second".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.metas.find_mark_by_name("second").unwrap();
        assert_eq!(gen_id, 2);
    }

    #[test]
    fn find_mark_not_found() {
        let records = vec![Record::Meta(Meta::Mark {
            gen_id: 1,
            name: "first".into(),
        })];
        let j = Journal::new(records);
        assert!(j.metas.find_mark_by_name("nonexistent").is_err());
    }

    #[test]
    fn find_mark_duplicate_names_returns_last() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "dup".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "dup".into(),
            }),
        ];
        let j = Journal::new(records);
        let gen_id = j.metas.find_mark_by_name("dup").unwrap();
        assert_eq!(gen_id, 2, "should return the last matching mark");
    }

    #[test]
    fn meta_at_on_jump_meta() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
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
        let j = Journal::new(records);
        assert!(
            j.metas.meta_at(0).is_none(),
            "Phantom meta should return None"
        );
        assert!(j.metas.meta_at(1).is_some());
        assert!(j.metas.meta_at(2).is_some());
        assert!(
            j.metas.meta_at(3).is_some(),
            "Jump meta should be returned by meta_at"
        );
    }

    #[test]
    fn find_meta_by_gen_id_rejects_phantom() {
        let records = vec![Record::Meta(Meta::Mark {
            gen_id: 1,
            name: "c1".into(),
        })];
        let j = Journal::new(records);
        assert!(
            j.metas.find_meta_by_gen_id(0).is_err(),
            "phantom gen_id=0 should not be a valid meta"
        );
        assert!(j.metas.find_meta_by_gen_id(1).is_ok());
    }

    #[test]
    fn jump_targeting_phantom() {
        // Jump to gen_id=0 (initial state) should kill all segments
        // between the phantom and the jump meta.
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
            }),
            Record::Meta(Meta::Jump {
                gen_id: 3,
                target_gen: 0,
            }),
        ];
        let j = Journal::new(records);
        // metas: [phantom(0), M(1), M(2), J(3→0)]
        // segments: [seg0, seg1, seg2, seg3]
        // Jump to 0 kills segments 0..3 → seg0, seg1, seg2 dead.
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(!alive[0], "seg0 killed by jump to initial");
        assert!(!alive[1], "seg1 killed by jump to initial");
        assert!(!alive[2], "seg2 killed by jump to initial");
        assert!(alive[3], "seg3 (trailing after jump) alive");
    }

    #[test]
    fn segment_range_at_rejects_phantom_id() {
        let records = vec![Record::Meta(Meta::Mark {
            gen_id: 1,
            name: "c1".into(),
        })];
        let j = Journal::new(records);
        assert!(
            j.metas
                .segment_range(Some("0"), None, None, j.segments.len())
                .is_err(),
            "--at 0 should be rejected (phantom)"
        );
    }

    #[test]
    fn segment_range_from_after_to_is_error() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
        ];
        let j = Journal::new(records);
        let result = j
            .metas
            .segment_range(None, Some("c3"), Some("c1"), j.segments.len());
        assert!(result.is_err(), "from > to should be an error");
    }

    #[test]
    fn segment_range_at_first_mark() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records);
        let (start, end) = j
            .metas
            .segment_range(Some("c1"), None, None, j.segments.len())
            .unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn segment_range_at_middle_mark() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 3,
                name: "c3".into(),
            }),
        ];
        let j = Journal::new(records);
        // --at c2: prev M is meta 0 (M1), so start=1, end=2
        let (start, end) = j
            .metas
            .segment_range(Some("c2"), None, None, j.segments.len())
            .unwrap();
        assert_eq!(start, 1);
        assert_eq!(end, 2);
    }

    #[test]
    fn segment_range_at_mark_after_jump() {
        // M1 [A] M2 [B] M3 J4(M2) [D] M5
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
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
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        // --at c5: prev M is meta[2] (M3), so start=3, end=5
        let (start, end) = j
            .metas
            .segment_range(Some("c5"), None, None, j.segments.len())
            .unwrap();
        assert_eq!(start, 3);
        assert_eq!(end, 5);
    }

    // ── Alive computation tests (migrated from liveness.rs) ──────────

    #[test]
    fn segment_alive_with_jump() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
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
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let j = Journal::new(records);
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(alive[0]);
        assert!(alive[1]);
        assert!(!alive[2]);
        assert!(!alive[3]);
        assert!(alive[4]);
        assert!(alive[5]);
    }

    #[test]
    fn alive_segments_range_ignores_jump_outside_range() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
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
        ];
        let j = Journal::new(records);
        let alive = j.metas.alive_segments_range(0..2, 2);
        assert!(alive[0], "seg0 should be alive (J4 outside range)");
        assert!(alive[1], "seg1 should be alive (J4 outside range)");
    }

    #[test]
    fn corrupt_j_record_skipped_in_alive() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Jump {
                gen_id: 2,
                target_gen: 99,
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
                ino: 2,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(
            alive.iter().all(|&a| a),
            "all segments alive when S target is missing"
        );
    }

    // ── Additional alive edge cases (migrated from liveness.rs) ──────

    #[test]
    fn alive_no_jumps() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        let j = Journal::new(records);
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(alive.iter().all(|&a| a), "no jumps means all alive");
    }

    #[test]
    fn alive_empty_journal() {
        let j = Journal::new(vec![]);
        let alive = j.metas.alive_segments(j.segments.len());
        assert_eq!(alive.len(), 1);
        assert!(alive[0]);
    }

    #[test]
    fn alive_multiple_jumps_last_wins() {
        // M1 [A] M2 [B] M3 J4(M2) [D] M5 J6(M1)
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
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
        let j = Journal::new(records);
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive (before M1)");
        assert!(!alive[1], "seg1 dead");
        assert!(!alive[2], "seg2 dead");
        assert!(!alive[3], "seg3 dead");
        assert!(!alive[4], "seg4 dead");
        assert!(!alive[5], "seg5 dead");
        assert!(alive[6], "seg6 alive (trailing, empty)");
    }

    #[test]
    fn alive_consecutive_j_records() {
        // M1 [A] M2 [B] M3 J4(M2) J5(M1)
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
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
        let j = Journal::new(records);
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(!alive[1], "seg1 dead (after M1, killed by J5)");
    }

    #[test]
    fn alive_jump_to_first_mark() {
        // M1 [A] M2 J3(M1)
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
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
        let j = Journal::new(records);
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 before M1 alive");
        assert!(!alive[1], "seg1 dead (after M1, before M2)");
        assert!(!alive[2], "seg2 dead");
        assert!(alive[3], "seg3 alive (trailing, empty)");
    }

    #[test]
    fn alive_nested_j_in_dead_zone() {
        // M1 [A] M2 [B] M3 J4(M1) [D] M5 [E] M6 J7(M5)
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
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
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Action(Action::Stage {
                path: "/e".into(),
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
        let j = Journal::new(records);
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(!alive[1], "seg1 dead (killed by J4)");
        assert!(!alive[2], "seg2 dead (killed by J4)");
        assert!(!alive[3], "seg3 dead (killed by J4)");
        assert!(alive[4], "seg4 alive (after J4, before M5)");
        assert!(!alive[5], "seg5 dead (killed by J7)");
        assert!(!alive[6], "seg6 dead (killed by J7)");
        assert!(alive[7], "seg7 alive (trailing, empty)");
    }

    #[test]
    fn alive_undo_jump() {
        // M1 [A] M2 [B] M3 J4(M1) [D] M5 J6(M3)
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
                ino: 1,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Action(Action::Stage {
                path: "/b".into(),
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
        let j = Journal::new(records);
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive");
        assert!(alive[1], "seg1 alive (jumped by J6)");
        assert!(alive[2], "seg2 alive (jumped by J6)");
        assert!(!alive[3], "seg3 dead (J4 segment)");
        assert!(!alive[4], "seg4 dead (killed by J6)");
        assert!(!alive[5], "seg5 dead (killed by J6)");
        assert!(alive[6], "seg6 alive (trailing, empty)");
    }

    // ── find_meta tests for jump metas ───────────────────────

    #[test]
    fn find_meta_accepts_jump_gen_id() {
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
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
        let j = Journal::new(records);
        assert_eq!(j.metas.find_meta("3").unwrap(), 3);
        assert_eq!(j.metas.find_meta("1").unwrap(), 1);
        assert_eq!(j.metas.find_meta("c1").unwrap(), 1);
        assert!(j.metas.find_meta("0").is_err());
    }

    #[test]
    fn alive_jump_to_jump_meta() {
        // M1 [A] M2 J3(→M1) [D] M4 J5(→J3)
        // J5 targets J3, so dead zone is 3..5 (seg_3, seg_4).
        let records = vec![
            Record::Meta(Meta::Mark {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Action(Action::Stage {
                path: "/a".into(),
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
            Record::Action(Action::Stage {
                path: "/d".into(),
                ino: 3,
            }),
            Record::Meta(Meta::Mark {
                gen_id: 4,
                name: "c4".into(),
            }),
            Record::Meta(Meta::Jump {
                gen_id: 5,
                target_gen: 3,
            }),
        ];
        let j = Journal::new(records);
        let alive = j.metas.alive_segments(j.segments.len());
        assert!(alive[0], "seg0 alive (before M1)");
        assert!(!alive[1], "seg1 dead (killed by J3)");
        assert!(!alive[2], "seg2 dead (killed by J3)");
        assert!(!alive[3], "seg3 dead (killed by J5)");
        assert!(!alive[4], "seg4 dead (killed by J5)");
        assert!(alive[5], "seg5 alive (trailing)");
    }
}
