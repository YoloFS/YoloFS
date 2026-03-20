// agfs CLI — journal/segment.rs
//
// Segmentation: split a flat record stream into segments at checkpoint (K)
// and restore (S) boundaries. Each segment contains only data records
// (A/M/D/R); checkpoint and restore records are stored in Markers.

use super::types::*;
use anyhow::Result;

/// The K/S skeleton of the journal.
pub struct Markers(pub(super) Vec<Record>);

impl Markers {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, idx: usize) -> Option<&Record> {
        self.0.get(idx)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Record> {
        self.0.iter()
    }

    /// Find a checkpoint by numeric gen ID. O(1) via direct indexing.
    pub fn find_checkpoint_by_gen_id(&self, gen_id: u64) -> Result<(u64, &str)> {
        let idx = gen_id.checked_sub(1).and_then(|i| usize::try_from(i).ok());
        if let Some(idx) = idx {
            if let Some(Record::Checkpoint { gen_id: g, name }) = self.0.get(idx) {
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
            if let Record::Checkpoint { gen_id, name: n } = marker
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
    pub fn closing_checkpoint(&self, marker_idx: usize) -> Option<(u64, &str)> {
        match self.0.get(marker_idx)? {
            Record::Checkpoint { gen_id, name } => Some((*gen_id, name)),
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
                .find(|&i| matches!(&self.0[i], Record::Checkpoint { .. }));
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
}

// ── SegmentedJournal ─────────────────────────────────────────────────

/// All segments + K/S skeleton. Level 1 of the pipeline.
pub struct SegmentedJournal {
    pub segments: Vec<Segment>,
    pub markers: Markers,
}

impl SegmentedJournal {
    /// Build from a parsed journal.
    /// Splits at both checkpoint (K) and restore (S) boundaries.
    pub fn new(journal: RawJournal) -> Self {
        let mut segments = Vec::new();
        let mut markers_vec: Vec<Record> = Vec::new();
        let mut current_records = Vec::new();
        let mut current_from: u64 = 0;

        for record in journal.0.into_iter() {
            match record {
                Record::Checkpoint { gen_id, .. } => {
                    segments.push(Segment {
                        from: current_from,
                        records: std::mem::take(&mut current_records),
                    });
                    current_from = gen_id;
                    markers_vec.push(record);
                }
                Record::Restore { target_gen, .. } => {
                    segments.push(Segment {
                        from: current_from,
                        records: std::mem::take(&mut current_records),
                    });
                    markers_vec.push(record);
                    current_from = target_gen;
                }
                _ => {
                    current_records.push(record);
                }
            }
        }

        // Trailing segment.
        segments.push(Segment {
            from: current_from,
            records: current_records,
        });

        SegmentedJournal {
            segments,
            markers: Markers(markers_vec),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmentation_basic() {
        // K1 [A] K2 [B] K3
        let records = vec![
            Record::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            },
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        // seg0(None,[]) seg1(K1,[A]) seg2(K2,[B]) seg3(K3,[])
        assert_eq!(sj.segments.len(), 4);
        assert_eq!(sj.segments[0].from, 0);
        assert!(sj.segments[0].records.is_empty());
        assert_eq!(sj.segments[1].from, 1);
        assert_eq!(sj.segments[1].records.len(), 1);
        assert_eq!(sj.segments[2].from, 2);
        assert_eq!(sj.segments[2].records.len(), 1);
        assert_eq!(sj.segments[3].from, 3);
        assert!(sj.segments[3].records.is_empty());
    }

    #[test]
    fn segmentation_splits_at_s_boundary() {
        // K1 [A] K2 [B] K3 S4(K2) [D] K5
        let records = vec![
            Record::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            },
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            },
            Record::Restore {
                gen_id: 4,
                target_gen: 2,
            },
            Record::Added {
                path: "/d".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
            Record::Checkpoint {
                gen_id: 5,
                name: "c5".into(),
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        // seg0(None,[]) seg1(K1,[A]) seg2(K2,[B]) seg3(K3,[]) seg4(K2,[D]) seg5(K5,[])
        assert_eq!(sj.segments.len(), 6);
        // seg4 inherits from=K2 (restore target)
        assert_eq!(sj.segments[4].from, 2);
        assert_eq!(sj.segments[4].records.len(), 1);
        assert!(matches!(&sj.segments[4].records[0], Record::Added { path, .. } if path == "/d"));
    }

    #[test]
    fn records_before_first_checkpoint_in_segment_zero() {
        let records = vec![
            Record::Added {
                path: "/orphan".into(),
                dtype: Some(DType::File),
                ino: 999,
            },
            Record::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        // seg0(None,[/orphan]) seg1(K1,[/a]) seg2(K2,[])
        assert_eq!(sj.segments.len(), 3);
        assert_eq!(sj.segments[0].from, 0);
        assert_eq!(sj.segments[0].records.len(), 1);
        assert!(
            matches!(&sj.segments[0].records[0], Record::Added { path, .. } if path == "/orphan")
        );
        assert_eq!(sj.segments[1].records.len(), 1);
        assert!(matches!(&sj.segments[1].records[0], Record::Added { path, .. } if path == "/a"));
    }

    // ── Markers tests ────────────────────────────────────────────────

    #[test]
    fn find_checkpoint_by_gen_id() {
        let records = vec![
            Record::Checkpoint {
                gen_id: 1,
                name: "first".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "second".into(),
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        let (gen_id, _) = sj.markers.find_checkpoint_by_gen_id(1).unwrap();
        assert_eq!(gen_id, 1);
    }

    #[test]
    fn find_checkpoint_by_name() {
        let records = vec![
            Record::Checkpoint {
                gen_id: 1,
                name: "first".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "second".into(),
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        let (gen_id, _) = sj.markers.find_checkpoint_by_name("second").unwrap();
        assert_eq!(gen_id, 2);
    }

    #[test]
    fn find_checkpoint_not_found() {
        let records = vec![Record::Checkpoint {
            gen_id: 1,
            name: "first".into(),
        }];
        let sj = SegmentedJournal::new(RawJournal(records));
        assert!(sj.markers.find_checkpoint_by_name("nonexistent").is_err());
    }

    #[test]
    fn find_checkpoint_duplicate_names_returns_last() {
        let records = vec![
            Record::Checkpoint {
                gen_id: 1,
                name: "dup".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "dup".into(),
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        let (gen_id, _) = sj.markers.find_checkpoint_by_name("dup").unwrap();
        assert_eq!(gen_id, 2, "should return the last matching checkpoint");
    }

    #[test]
    fn closing_checkpoint_on_restore_marker() {
        // K1 [A] K2 S3(K1)
        let records = vec![
            Record::Checkpoint {
                gen_id: 1,
                name: "init".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            },
            Record::Restore {
                gen_id: 3,
                target_gen: 1,
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        // Marker 0 is K1, Marker 1 is K2, Marker 2 is S3
        assert!(sj.markers.closing_checkpoint(0).is_some());
        assert!(sj.markers.closing_checkpoint(1).is_some());
        assert!(
            sj.markers.closing_checkpoint(2).is_none(),
            "Restore marker should return None"
        );
    }

    #[test]
    fn segment_range_from_after_to_is_error() {
        // K1 [A] K2 [B] K3
        let records = vec![
            Record::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            },
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        let result = sj
            .markers
            .segment_range(None, Some("c3"), Some("c1"), sj.segments.len());
        assert!(result.is_err(), "from > to should be an error");
    }

    #[test]
    fn segment_range_at_first_checkpoint() {
        // K1 [A] K2 [B] K3
        let records = vec![
            Record::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            },
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        // --at c1: no previous K → start=0, end=1
        let (start, end) = sj
            .markers
            .segment_range(Some("c1"), None, None, sj.segments.len())
            .unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn segment_range_at_middle_checkpoint() {
        // K1 [A] K2 [B] K3
        let records = vec![
            Record::Checkpoint {
                gen_id: 1,
                name: "c1".into(),
            },
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "c2".into(),
            },
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint {
                gen_id: 3,
                name: "c3".into(),
            },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        // --at c2: prev K is marker 0 (K1), so start=1, end=2
        let (start, end) = sj
            .markers
            .segment_range(Some("c2"), None, None, sj.segments.len())
            .unwrap();
        assert_eq!(start, 1);
        assert_eq!(end, 2);
    }

    #[test]
    fn segment_range_at_checkpoint_after_restore() {
        // K1 [A] K2 [B] K3 S4(K2) [D] K5
        let records = vec![
            Record::Checkpoint { gen_id: 1, name: "c1".into() },
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint { gen_id: 2, name: "c2".into() },
            Record::Added { path: "/b".into(), dtype: Some(DType::File), ino: 2 },
            Record::Checkpoint { gen_id: 3, name: "c3".into() },
            Record::Restore { gen_id: 4, target_gen: 2 },
            Record::Added { path: "/d".into(), dtype: Some(DType::File), ino: 3 },
            Record::Checkpoint { gen_id: 5, name: "c5".into() },
        ];
        let sj = SegmentedJournal::new(RawJournal(records));
        // --at c5: prev K is marker[2] (K3), so start=3, end=5
        let (start, end) = sj
            .markers
            .segment_range(Some("c5"), None, None, sj.segments.len())
            .unwrap();
        assert_eq!(start, 3);
        assert_eq!(end, 5);
    }

    #[test]
    fn segmentation_empty_journal() {
        let sj = SegmentedJournal::new(RawJournal(vec![]));
        // Even empty journal produces one trailing segment.
        assert_eq!(sj.segments.len(), 1);
        assert_eq!(sj.segments[0].from, 0);
        assert!(sj.segments[0].records.is_empty());
        assert!(sj.markers.is_empty());
    }

    #[test]
    fn segmentation_only_s_records() {
        // S record with no preceding K — target not found, treated as orphan boundary.
        let records = vec![Record::Restore {
            gen_id: 1,
            target_gen: 99,
        }];
        let sj = SegmentedJournal::new(RawJournal(records));
        // seg0(None,[]) + seg1(None,[]) (split at S, target not in k_map)
        assert_eq!(sj.segments.len(), 2);
        assert_eq!(sj.segments[0].from, 0);
        assert_eq!(sj.segments[1].from, 99);
        assert_eq!(sj.markers.len(), 1);
    }
}
