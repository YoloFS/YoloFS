// agfs CLI — journal/markers.rs
//
// The CKP/RST skeleton: checkpoint and restore records extracted from the journal.
// Provides lookup by gen_id or name, and segment range computation.

use super::types::*;
use anyhow::Result;

/// The CKP/RST skeleton of the journal.
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
