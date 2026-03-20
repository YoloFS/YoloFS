// agfs CLI — journal/timeline.rs
//
// Structured view of journal records with reachability analysis.
//
// A Timeline splits records into Segments at checkpoint (K) and restore (S)
// boundaries. Segments are marked reachable or unreachable based on restore
// records.

use super::types::*;
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// A group of records between checkpoint/restore boundaries.
#[derive(Debug)]
pub struct Segment {
    /// The checkpoint at the start of this segment (or a synthetic one for the
    /// initial segment before the first checkpoint).
    pub from: Checkpoint,
    /// The checkpoint at the end, or None for the trailing (unsaved) segment.
    pub to: Option<Checkpoint>,
    /// Whether this segment is reachable (not killed by restores).
    pub reachable: bool,
    /// The A/M/D/R records in this segment (no K/S records).
    pub records: Vec<Record>,
}

/// Structured view of the journal with reachability information.
pub struct Timeline {
    /// All original records (flat, for audit display).
    all_records: Vec<Record>,
    /// Structured segments with reachability.
    pub segments: Vec<Segment>,
    /// Per-record reachability mask (parallel to all_records).
    reachable_mask: Vec<bool>,
}

impl Timeline {
    /// Build a Timeline from raw journal records.
    ///
    /// Splits records into segments at K and S boundaries, then computes
    /// reachability using the same algorithm as the flat `reachable()` function.
    pub fn new(records: Vec<Record>) -> Self {
        let reachable_mask = compute_reachable_mask(&records);

        // Build segments by walking through records.
        // Track the index of the current `from` checkpoint so we can read
        // its reachability directly from the mask.
        let mut segments = Vec::new();
        let mut current_records = Vec::new();
        let mut current_from: Option<(Checkpoint, usize)> = None;

        for (i, record) in records.iter().enumerate() {
            match record {
                Record::Checkpoint(c) => {
                    // Close the previous segment if there is one.
                    if let Some((from, from_idx)) = current_from.take() {
                        segments.push(Segment {
                            from,
                            to: Some(c.clone()),
                            reachable: reachable_mask[from_idx],
                            records: std::mem::take(&mut current_records),
                        });
                    }
                    current_from = Some((c.clone(), i));
                }
                Record::Restore { .. } => {
                    // Restore records are boundaries but don't create named segments.
                    // They just close the current open segment.
                    // The records before the restore in the current segment are
                    // part of the current segment.
                }
                _ => {
                    if current_from.is_some() {
                        current_records.push(record.clone());
                    }
                    // Records before the first checkpoint are not in any segment.
                }
            }
        }

        // Trailing segment after the last checkpoint.
        if let Some((from, from_idx)) = current_from
            && !current_records.is_empty()
        {
            segments.push(Segment {
                from,
                to: None,
                reachable: reachable_mask[from_idx],
                records: std::mem::take(&mut current_records),
            });
        }

        Timeline {
            all_records: records,
            segments,
            reachable_mask,
        }
    }

    /// Iterate over reachable segments.
    pub fn reachable(&self) -> impl Iterator<Item = &Segment> {
        self.segments.iter().filter(|s| s.reachable)
    }

    /// Collect all records from reachable segments (cloned), suitable for resolve.
    ///
    /// This produces the same output as the flat `reachable()` function:
    /// all records that survive restore pruning (including K records but not S).
    pub fn reachable_records(&self) -> Vec<Record> {
        self.all_records
            .iter()
            .enumerate()
            .filter(|(i, _)| self.reachable_mask[*i])
            .map(|(_, r)| r.clone())
            .collect()
    }

    /// Find a checkpoint by name or numeric ID.
    ///
    /// Searches ALL records (including unreachable) so undo-restore works.
    /// Tries numeric parse first, then name match (latest occurrence).
    /// Returns (record index in all_records, checkpoint ref).
    pub fn find_checkpoint(&self, name_or_id: &str) -> Result<(usize, &Checkpoint)> {
        // Try numeric ID first
        if let Ok(target_id) = name_or_id.parse::<u64>() {
            for (i, record) in self.all_records.iter().enumerate() {
                if let Record::Checkpoint(c) = record
                    && c.gen_id == target_id
                {
                    return Ok((i, c));
                }
            }
        }

        // Fall back to name match (latest occurrence)
        let mut last = None;
        for (i, record) in self.all_records.iter().enumerate() {
            if let Record::Checkpoint(c) = record
                && c.name == name_or_id
            {
                last = Some((i, c));
            }
        }
        last.ok_or_else(|| anyhow::anyhow!("checkpoint not found: {name_or_id}"))
    }

    /// Slice reachable records by checkpoint range.
    ///
    /// Same semantics as the old `slice_records()`:
    /// - `at`   → single segment between previous checkpoint and named one
    /// - `from` → records from that checkpoint to end
    /// - `to`   → records from start up to (and including) that checkpoint
    /// - both   → records between the two checkpoints (inclusive)
    /// - none   → all reachable records (unchanged)
    pub fn slice(
        &self,
        at: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<Record>> {
        let mut records = self.reachable_records();
        slice_records_inner(&mut records, at, from, to)
    }

    /// Check if a record at index i in the original flat list is reachable.
    pub fn is_reachable(&self, idx: usize) -> bool {
        self.reachable_mask.get(idx).copied().unwrap_or(false)
    }

    /// Access the original flat record list (for audit display).
    pub fn all_records(&self) -> &[Record] {
        &self.all_records
    }
}

/// Compute reachable ranges from journal records.
///
/// Returns `None` when there are no restore records (everything is reachable).
/// Otherwise returns half-open `(start, end)` ranges of reachable indices,
/// in reverse order (latest first). Callers that need forward order must
/// reverse the result.
fn compute_reachable_ranges(records: &[Record]) -> Option<Vec<(usize, usize)>> {
    let n = records.len();
    if n == 0 {
        return None;
    }

    // Collect S and K positions in one pass.
    let mut s_list: Vec<(usize, u64)> = Vec::new();
    let mut k_map: HashMap<u64, usize> = HashMap::new();

    for (i, record) in records.iter().enumerate() {
        match record {
            Record::Restore { target_gen, .. } => {
                s_list.push((i, *target_gen));
            }
            Record::Checkpoint(c) => {
                k_map.insert(c.gen_id, i);
            }
            _ => {}
        }
    }

    if s_list.is_empty() {
        return None;
    }

    // Walk S records right-to-left, building reachable ranges.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut end = n;

    for &(s_pos, target_gen) in s_list.iter().rev() {
        if s_pos >= end {
            continue; // S is in a dead zone, skip
        }
        let Some(&k_pos) = k_map.get(&target_gen) else {
            continue; // Target checkpoint not found — skip corrupt S record
        };
        if s_pos + 1 < end {
            ranges.push((s_pos + 1, end));
        }
        end = k_pos + 1;
    }
    ranges.push((0, end));
    Some(ranges)
}

/// Compute per-record reachability mask (parallel to the record slice).
fn compute_reachable_mask(records: &[Record]) -> Vec<bool> {
    let n = records.len();
    let Some(ranges) = compute_reachable_ranges(records) else {
        return vec![true; n];
    };

    let mut mask = vec![false; n];
    for (start, range_end) in ranges {
        for m in &mut mask[start..range_end] {
            *m = true;
        }
    }
    mask
}

/// Build the set of record indices that are reachable (not killed by restores).
pub fn reachable_indices(records: &[Record]) -> HashSet<usize> {
    let n = records.len();
    let Some(ranges) = compute_reachable_ranges(records) else {
        return (0..n).collect();
    };

    let mut set = HashSet::new();
    for (start, range_end) in ranges {
        for i in start..range_end {
            set.insert(i);
        }
    }
    set
}

/// Remove unreachable records created by S (restore) records.
///
/// An S record with `target_gen=G` means "state was reset to checkpoint G."
/// Records between that checkpoint and the S record are unreachable. This
/// function returns only the reachable records by walking S records
/// right-to-left and building non-overlapping reachable ranges.
///
/// If an S record references a non-existent checkpoint (corrupted journal),
/// that S record is skipped.
///
/// O(N) to collect positions + O(R) to build ranges where R = number of
/// restore records.
pub fn reachable(records: Vec<Record>) -> Vec<Record> {
    let Some(mut ranges) = compute_reachable_ranges(&records) else {
        return records;
    };
    ranges.reverse();

    // Extract reachable records directly from sorted, non-overlapping ranges.
    let capacity: usize = ranges.iter().map(|&(s, e)| e - s).sum();
    let mut reachable = Vec::with_capacity(capacity);
    let mut iter = records.into_iter();
    let mut pos = 0;
    for &(start, end) in &ranges {
        if start > pos {
            iter.by_ref().take(start - pos).for_each(drop);
        }
        reachable.extend(iter.by_ref().take(end - start));
        pos = end;
    }
    reachable
}

/// Find the record index of a checkpoint by name or numeric ID.
/// Tries parsing as a numeric ID first, then falls back to name match
/// (using the latest occurrence if names are duplicated).
pub fn find_checkpoint_index(records: &[Record], name_or_id: &str) -> Result<usize> {
    // Try numeric ID first
    if let Ok(target_id) = name_or_id.parse::<u64>() {
        for (i, record) in records.iter().enumerate() {
            if let Record::Checkpoint(c) = record
                && c.gen_id == target_id
            {
                return Ok(i);
            }
        }
    }

    // Fall back to name match (latest occurrence)
    let mut last = None;
    for (i, record) in records.iter().enumerate() {
        if let Record::Checkpoint(c) = record
            && c.name == name_or_id
        {
            last = Some(i);
        }
    }
    last.ok_or_else(|| anyhow::anyhow!("checkpoint not found: {name_or_id}"))
}

/// Slice journal records to the range specified by --at, --from, --to.
///
/// The returned slice always includes boundary checkpoint records so that
/// `resolve_segments` can determine `from` and `to` for each segment.
///
/// - `at`   → single segment between previous checkpoint and named one
/// - `from` → records from that checkpoint to end
/// - `to`   → records from start up to (and including) that checkpoint
/// - both   → records between the two checkpoints (inclusive)
/// - none   → all records (unchanged)
pub fn slice_records(
    mut records: Vec<Record>,
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<Record>> {
    slice_records_inner(&mut records, at, from, to)
}

/// Internal implementation of slice_records that works on a mutable Vec.
fn slice_records_inner(
    records: &mut Vec<Record>,
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<Record>> {
    if let Some(name) = at {
        let chk_idx = find_checkpoint_index(records, name)?;
        let prev = records[..chk_idx]
            .iter()
            .rposition(|r| matches!(r, Record::Checkpoint(_)));
        let start = prev.unwrap_or(0);
        records.truncate(chk_idx + 1);
        return Ok(records.split_off(start));
    }
    // Truncate end first so `from` indices stay valid.
    if let Some(to_name) = to {
        let to_idx = find_checkpoint_index(records, to_name)?;
        records.truncate(to_idx + 1);
    }
    if let Some(from_name) = from {
        let from_idx = find_checkpoint_index(records, from_name)?;
        *records = records.split_off(from_idx); // include from checkpoint
    }
    Ok(std::mem::take(records))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let tl = Timeline::new(records);
        let reachable = tl.reachable_records();
        assert_eq!(reachable.len(), 3);
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
        let tl = Timeline::new(records);
        let reachable = tl.reachable_records();
        // Reachable: K1, A(/a), K2, A(/d), K5
        assert_eq!(reachable.len(), 5);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&reachable[1], Record::Added { path, .. } if path == "/a"));
        assert!(matches!(&reachable[2], Record::Checkpoint(c) if c.gen_id == 2));
        assert!(matches!(&reachable[3], Record::Added { path, .. } if path == "/d"));
        assert!(matches!(&reachable[4], Record::Checkpoint(c) if c.gen_id == 5));
    }

    #[test]
    fn reachable_multiple_restores_last_wins() {
        // K1 [A] K2 [B] K3 S4(K2) [D] K5 S6(K1)
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
        let tl = Timeline::new(records);
        let reachable = tl.reachable_records();
        // Reachable: K1 only
        assert_eq!(reachable.len(), 1);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
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
        let tl = Timeline::new(records);
        let reachable = tl.reachable_records();
        // S7(K5): reachable prefix up to K5, nothing after S7
        // But prefix K1..K5 contains S4(K1), so recurse:
        //   S4(K1): reachable prefix up to K1, then K5 suffix = [D] K5
        // Final: K1, [D], K5
        assert_eq!(reachable.len(), 3);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&reachable[1], Record::Added { path, .. } if path == "/d"));
        assert!(matches!(&reachable[2], Record::Checkpoint(c) if c.gen_id == 5));
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
        let tl = Timeline::new(records);
        let reachable = tl.reachable_records();
        // S6(K3) is last S. Reachable = records[0..=K3] (K3 is at idx 4)
        // No S in that prefix. Reachable: K1 [A] K2 [B] K3
        assert_eq!(reachable.len(), 5);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&reachable[1], Record::Added { path, .. } if path == "/a"));
        assert!(matches!(&reachable[2], Record::Checkpoint(c) if c.gen_id == 2));
        assert!(matches!(&reachable[3], Record::Added { path, .. } if path == "/b"));
        assert!(matches!(&reachable[4], Record::Checkpoint(c) if c.gen_id == 3));
    }

    #[test]
    fn reachable_restore_to_initial() {
        // K1 [A] K2 S3(K1)
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
                target_gen: 1,
            },
        ];
        let tl = Timeline::new(records);
        let reachable = tl.reachable_records();
        // Reachable: K1 only
        assert_eq!(reachable.len(), 1);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
    }

    #[test]
    fn reachable_corrupt_s_record_skipped() {
        // S record references non-existent checkpoint gen 99 — should be skipped.
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
        let tl = Timeline::new(records);
        let reachable = tl.reachable_records();
        // Corrupt S is skipped, all records pass through.
        assert_eq!(reachable.len(), 5);
    }

    #[test]
    fn reachable_empty_journal() {
        let tl = Timeline::new(vec![]);
        let reachable = tl.reachable_records();
        assert!(reachable.is_empty());
    }

    #[test]
    fn reachable_consecutive_s_records() {
        // K1 [A] K2 [B] K3 S4(K2) S5(K1)
        // Two consecutive restores: second one "wins" and goes further back.
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
        let tl = Timeline::new(records);
        let reachable = tl.reachable_records();
        // S5(K1) kills everything after K1. S4(K2) is in that dead zone.
        assert_eq!(reachable.len(), 1);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
    }

    #[test]
    fn find_checkpoint_by_id() {
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "first".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "second".into(),
            }),
        ];
        let tl = Timeline::new(records);
        let (idx, c) = tl.find_checkpoint("1").unwrap();
        assert_eq!(idx, 0);
        assert_eq!(c.gen_id, 1);
    }

    #[test]
    fn find_checkpoint_by_name() {
        let records = vec![
            Record::Checkpoint(Checkpoint {
                gen_id: 1,
                name: "first".into(),
            }),
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint(Checkpoint {
                gen_id: 2,
                name: "second".into(),
            }),
        ];
        let tl = Timeline::new(records);
        let (idx, c) = tl.find_checkpoint("second").unwrap();
        assert_eq!(idx, 2);
        assert_eq!(c.gen_id, 2);
    }

    #[test]
    fn find_checkpoint_not_found() {
        let records = vec![Record::Checkpoint(Checkpoint {
            gen_id: 1,
            name: "first".into(),
        })];
        let tl = Timeline::new(records);
        assert!(tl.find_checkpoint("nonexistent").is_err());
    }

    #[test]
    fn segment_reachability() {
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
        let tl = Timeline::new(records);

        // K1 and K2 are reachable, K3 is not (killed by S4)
        assert!(tl.is_reachable(0)); // K1
        assert!(tl.is_reachable(1)); // A(/a)
        assert!(tl.is_reachable(2)); // K2
        assert!(!tl.is_reachable(3)); // B — dead
        assert!(!tl.is_reachable(4)); // K3 — dead
        assert!(!tl.is_reachable(5)); // S4 — dead
        assert!(tl.is_reachable(6)); // D
        assert!(tl.is_reachable(7)); // K5
    }

    #[test]
    fn records_before_first_checkpoint_skipped() {
        let records = vec![
            Record::Added {
                path: "/orphan".into(),
                dtype: Some(DType::File),
                ino: 999,
            },
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
        let tl = Timeline::new(records);
        assert_eq!(tl.segments.len(), 1);
        assert_eq!(tl.segments[0].records.len(), 1); // Only /a, not /orphan
        assert!(matches!(&tl.segments[0].records[0], Record::Added { path, .. } if path == "/a"));
    }

    #[test]
    fn timeline_slice_at() {
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
        let tl = Timeline::new(records);
        let sliced = tl.slice(Some("c3"), None, None).unwrap();
        // Should include K2, A(/b), K3
        assert_eq!(sliced.len(), 3);
        assert!(matches!(&sliced[0], Record::Checkpoint(c) if c.gen_id == 2));
        assert!(matches!(&sliced[1], Record::Added { path, .. } if path == "/b"));
        assert!(matches!(&sliced[2], Record::Checkpoint(c) if c.gen_id == 3));
    }

    #[test]
    fn timeline_slice_from_to() {
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
        let tl = Timeline::new(records);
        let sliced = tl.slice(None, Some("c2"), Some("c3")).unwrap();
        // Should include K2, A(/b), K3
        assert_eq!(sliced.len(), 3);
        assert!(matches!(&sliced[0], Record::Checkpoint(c) if c.gen_id == 2));
        assert!(matches!(&sliced[1], Record::Added { path, .. } if path == "/b"));
        assert!(matches!(&sliced[2], Record::Checkpoint(c) if c.gen_id == 3));
    }

    #[test]
    fn timeline_slice_not_found() {
        let records = vec![Record::Checkpoint(Checkpoint {
            gen_id: 1,
            name: "init".into(),
        })];
        let tl = Timeline::new(records);
        assert!(tl.slice(Some("nonexistent"), None, None).is_err());
    }
}
