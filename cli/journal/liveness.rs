// agfs CLI — journal/liveness.rs
//
// Reachability (liveness) filtering: determine which segments survive
// restore pruning, and provide live(), live_prefix(), live_slice()
// convenience methods on SegmentedJournal.

use super::segment::*;
use super::types::*;
use anyhow::Result;

// ── Liveness computation ─────────────────────────────────────────────

impl Markers {
    /// Compute alive flags for all segments.
    ///
    /// Walks restore (S) markers right-to-left. Each S(target_gen) kills
    /// segments between the target checkpoint and the S marker.
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
            if let Marker::Checkpoint { checkpoint, .. } = &self.0[i] {
                gen_to_idx.insert(checkpoint.gen_id, i);
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

// ── SegmentedJournal → live filtering ────────────────────────────────

impl SegmentedJournal {
    /// Filter to live segments only (Level 1 → Level 2).
    pub fn live(self) -> LiveSegments {
        let alive = self.markers.alive_segments(self.segments.len());
        LiveSegments(
            self.segments
                .into_iter()
                .enumerate()
                .filter(|(i, _)| alive[*i])
                .map(|(_, s)| s)
                .collect(),
        )
    }

    /// Filter to live segments and flatten into a single record list.
    pub fn live_records(self) -> Vec<Record> {
        self.live().0.into_iter().flat_map(|s| s.records).collect()
    }

    /// Take prefix up to a marker index (inclusive), then filter to live.
    /// Used by restore to get live records up to a target checkpoint.
    /// Reachability is computed only within the prefix (S records after the
    /// prefix boundary do not affect it).
    pub fn live_prefix(self, up_to_marker: usize) -> LiveSegments {
        let num_prefix = up_to_marker + 1;
        let alive = self.markers.alive_segments_range(0..num_prefix, num_prefix);
        LiveSegments(
            self.segments
                .into_iter()
                .enumerate()
                .take(num_prefix)
                .filter(|(i, _)| alive[*i])
                .map(|(_, s)| s)
                .collect(),
        )
    }

    /// Like `live_prefix`, but flattens into a single record list.
    pub fn live_prefix_records(self, up_to_marker: usize) -> Vec<Record> {
        self.live_prefix(up_to_marker)
            .0
            .into_iter()
            .flat_map(|s| s.records)
            .collect()
    }

    /// Slice to a checkpoint range and filter to live segments.
    /// Returns segments paired with their closing checkpoint (for display).
    pub fn live_slice(
        self,
        at: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<(Segment, Option<Checkpoint>)>> {
        let SegmentedJournal { segments, markers } = self;
        let num = segments.len();
        let alive = markers.alive_segments(num);
        let (start, end) = markers.segment_range(at, from, to, num)?;

        let result: Vec<_> = segments
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i >= start && *i < end && alive[*i])
            .map(|(i, seg)| {
                let closing = markers.closing_checkpoint(i).cloned();
                (seg, closing)
            })
            .collect();

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper: collect live records (segments + from checkpoints) ────

    fn live_records(records: Vec<Record>) -> Vec<Record> {
        let sj = SegmentedJournal::new(RawJournal(records));
        let live = sj.live();
        let mut result = Vec::new();
        for seg in live.0 {
            if let Some(chk) = seg.from {
                result.push(Record::Checkpoint(chk));
            }
            result.extend(seg.records);
        }
        result
    }

    // ── Alive computation tests ──────────────────────────────────────

    #[test]
    fn segment_alive_with_restore() {
        // K1 [A] K2 [B] K3 S4(K2) [D] K5
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Restore {
                gen_id: 4,
                target_gen: 2,
            },
            Record::Added {
                path: "/d".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        let alive = sj.markers.alive_segments(sj.segments.len());
        // seg0(None)=alive seg1(K1)=alive seg2(K2)=dead seg3(K3)=dead seg4(K2*)=alive seg5(K5)=alive
        assert!(alive[0]);
        assert!(alive[1]);
        assert!(!alive[2]);
        assert!(!alive[3]);
        assert!(alive[4]);
        assert!(alive[5]);
    }

    // ── Reachability tests (via live_records helper) ─────────────────

    #[test]
    fn reachable_no_restores() {
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
        ];
        assert_eq!(live_records(records).len(), 3);
    }

    #[test]
    fn reachable_single_restore() {
        // K1 [A] K2 [B] K3 S4(K2) [D] K5
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Restore {
                gen_id: 4,
                target_gen: 2,
            },
            Record::Added {
                path: "/d".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let result = live_records(records);
        assert_eq!(result.len(), 5);
        assert!(matches!(&result[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&result[1], Record::Added { path, .. } if path == "/a"));
        assert!(matches!(&result[2], Record::Checkpoint(c) if c.gen_id == 2));
        assert!(matches!(&result[3], Record::Added { path, .. } if path == "/d"));
        assert!(matches!(&result[4], Record::Checkpoint(c) if c.gen_id == 5));
    }

    #[test]
    fn reachable_multiple_restores_last_wins() {
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Restore {
                gen_id: 4,
                target_gen: 2,
            },
            Record::Added {
                path: "/d".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Restore {
                gen_id: 6,
                target_gen: 1,
            },
        ];
        let result = live_records(records);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Record::Checkpoint(c) if c.gen_id == 1));
    }

    #[test]
    fn reachable_nested_s_in_dead_zone() {
        // K1 [A] K2 [B] K3 S4(K1) [D] K5 [E] K6 S7(K5)
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Restore {
                gen_id: 4,
                target_gen: 1,
            },
            Record::Added {
                path: "/d".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Added {
                path: "/e".into(),
                dtype: Some(DType::File),
                ino: 4,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 6,
                name: "c6".into(),
            }),
            Record::Restore {
                gen_id: 7,
                target_gen: 5,
            },
        ];
        let result = live_records(records);
        assert_eq!(result.len(), 3);
        assert!(matches!(&result[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&result[1], Record::Added { path, .. } if path == "/d"));
        assert!(matches!(&result[2], Record::Checkpoint(c) if c.gen_id == 5));
    }

    #[test]
    fn reachable_undo_restore() {
        // K1 [A] K2 [B] K3 S4(K1) [D] K5 S6(K3)
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Restore {
                gen_id: 4,
                target_gen: 1,
            },
            Record::Added {
                path: "/d".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Restore {
                gen_id: 6,
                target_gen: 3,
            },
        ];
        let result = live_records(records);
        assert_eq!(result.len(), 5);
        assert!(matches!(&result[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&result[1], Record::Added { path, .. } if path == "/a"));
        assert!(matches!(&result[2], Record::Checkpoint(c) if c.gen_id == 2));
        assert!(matches!(&result[3], Record::Added { path, .. } if path == "/b"));
        assert!(matches!(&result[4], Record::Checkpoint(c) if c.gen_id == 3));
    }

    /// Restore to the first checkpoint discards everything after it.
    #[test]
    fn reachable_restore_to_first_checkpoint() {
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Restore {
                gen_id: 3,
                target_gen: 1,
            },
        ];
        let result = live_records(records);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Record::Checkpoint(c) if c.gen_id == 1));
    }

    #[test]
    fn reachable_corrupt_s_record_skipped() {
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Restore {
                gen_id: 3,
                target_gen: 99,
            },
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
        ];
        let result = live_records(records);
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn reachable_empty_journal() {
        assert!(live_records(vec![]).is_empty());
    }

    #[test]
    fn reachable_consecutive_s_records() {
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Restore {
                gen_id: 4,
                target_gen: 2,
            },
            Record::Restore {
                gen_id: 5,
                target_gen: 1,
            },
        ];
        let result = live_records(records);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Record::Checkpoint(c) if c.gen_id == 1));
    }

    // ── Slice tests ──────────────────────────────────────────────────

    #[test]
    fn live_slice_at() {
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        let pairs = sj.live_slice(Some("c3"), None, None).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.from.as_ref().unwrap().gen_id, 2);
        assert_eq!(pairs[0].0.records.len(), 1);
        assert!(matches!(&pairs[0].0.records[0], Record::Added { path, .. } if path == "/b"));
        assert_eq!(pairs[0].1.as_ref().unwrap().gen_id, 3);
    }

    #[test]
    fn live_slice_from_to() {
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Added {
                path: "/c".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 4,
                name: "c4".into(),
            }),
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        let pairs = sj.live_slice(None, Some("c2"), Some("c3")).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.from.as_ref().unwrap().gen_id, 2);
        assert!(matches!(&pairs[0].0.records[0], Record::Added { path, .. } if path == "/b"));
    }

    #[test]
    fn live_slice_not_found() {
        let records = vec![Record::Checkpoint(Checkpoint {
            gen_id: 1,
            name: "init".into(),
        })];
        let sj = SegmentedJournal::new(RawJournal(records));
        assert!(sj.live_slice(Some("nonexistent"), None, None).is_err());
    }

    // ── Live prefix test (for restore) ───────────────────────────────

    #[test]
    fn live_prefix_basic() {
        // K1 [A] K2 [B] K3 S4(K1) [D] K5
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Restore {
                gen_id: 4,
                target_gen: 1,
            },
            Record::Added {
                path: "/d".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        let (m_idx, _) = sj.markers.find_checkpoint_by_name("c2").unwrap();
        let prefix = sj.live_prefix(m_idx);
        let records: Vec<Record> = prefix
            .0
            .into_iter()
            .flat_map(|s| {
                let mut r = Vec::new();
                if let Some(c) = s.from {
                    r.push(Record::Checkpoint(c));
                }
                r.extend(s.records);
                r
            })
            .collect();
        // K1, [A] — the live prefix up to K2
        assert_eq!(records.len(), 2);
        assert!(matches!(&records[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&records[1], Record::Added { path, .. } if path == "/a"));
    }

    #[test]
    fn alive_segments_range_ignores_restore_outside_range() {
        // K1 [A] K2 [B] K3 S4(K1)
        // alive_segments_range(0..2) should only see K1 and K2, ignore S4.
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Restore {
                gen_id: 4,
                target_gen: 1,
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        // Markers: K1(0), K2(1), K3(2), S4(3). Segments: 5 total.
        // Range 0..2 means only K1 and K2 markers considered.
        let alive = sj.markers.alive_segments_range(0..2, 2);
        assert!(alive[0], "seg0 should be alive (S4 outside range)");
        assert!(alive[1], "seg1 should be alive (S4 outside range)");
    }

    #[test]
    fn live_prefix_with_nested_restores() {
        // K1 [A] K2 [B] K3 S4(K1) [C] K5 [D] K6
        // live_prefix up to K5 marker:
        //   prefix markers: K1, K2, K3, S4. S4(K1) kills K2,K3,S4 segments.
        //   live: seg(K1,[A]) + seg(K1*,[C])
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            }),
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            }),
            Record::Restore {
                gen_id: 4,
                target_gen: 1,
            },
            Record::Added {
                path: "/c".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            }),
            Record::Added {
                path: "/d".into(),
                dtype: Some(DType::File),
                ino: 4,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 6,
                name: "c6".into(),
            }),
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        // Markers: K1(0), K2(1), K3(2), S4(3), K5(4), K6(5)
        let (m_idx, _) = sj.markers.find_checkpoint_by_name("c5").unwrap();
        let prefix = sj.live_prefix(m_idx);
        let result: Vec<Record> = prefix.0.into_iter().flat_map(|s| s.records).collect();
        // Live prefix up to K5: [C] from seg4.
        // [A] is dead (killed by S4, which kills segs after K1 marker). [D] is beyond K5.
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Record::Added { path, .. } if path == "/c"));
    }

    #[test]
    fn corrupt_s_record_skipped_in_alive() {
        // K1 [A] S2(K99) [B]
        // S targets nonexistent K99 — should be skipped, all segments alive.
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "init".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Restore {
                gen_id: 2,
                target_gen: 99,
            },
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        let alive = sj.markers.alive_segments(sj.segments.len());
        assert!(
            alive.iter().all(|&a| a),
            "all segments alive when S target is missing"
        );
    }
}
