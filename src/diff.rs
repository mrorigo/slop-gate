// Rust guideline compliant 2026-10-20
//! Deterministic line mapping shared by CLI and snapshot adapters.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};

/// Hashes newline-delimited byte lines into stable comparison keys.
pub(crate) fn line_hashes(bytes: &[u8]) -> Vec<u64> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    if bytes.last() == Some(&b'\n') {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|line| {
            let hash = blake3::hash(line);
            u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap_or([0; 8]))
        })
        .collect()
}

/// Returns one-based candidate lines absent from a deterministic LCS mapping.
///
/// A common prefix is skipped, then the matrix band grows until the edit
/// distance is exact. Equal-cost paths still prefer deletion, preserving
/// attribution when repeated lines permit several longest common subsequences.
///
/// # Errors
/// Returns an error if the cell budget is exceeded, dimensions overflow, or
/// cancellation is observed while comparing boundaries or between matrix rows.
pub(crate) fn added_line_numbers(
    baseline: &[u64],
    candidate: &[u64],
    max_cells: Option<usize>,
    cancelled: &AtomicBool,
) -> Result<Vec<usize>> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::invalid("line diff", "cancelled"));
    }
    let prefix = common_length(baseline.iter().zip(candidate), cancelled)?;
    let added = remaining_added_lines(
        &baseline[prefix..],
        &candidate[prefix..],
        max_cells,
        cancelled,
    )?;
    Ok(added.into_iter().map(|line| line + prefix).collect())
}

fn remaining_added_lines(
    baseline: &[u64],
    candidate: &[u64],
    max_cells: Option<usize>,
    cancelled: &AtomicBool,
) -> Result<Vec<usize>> {
    if candidate.is_empty() {
        return Ok(Vec::new());
    }
    if baseline.is_empty() {
        return Ok((1..=candidate.len()).collect());
    }
    let distances = build_exact_distance_table(baseline, candidate, max_cells, cancelled)?;
    Ok(collect_added_lines(baseline, candidate, &distances))
}

fn build_exact_distance_table(
    baseline: &[u64],
    candidate: &[u64],
    max_cells: Option<usize>,
    cancelled: &AtomicBool,
) -> Result<DistanceTable> {
    let full_band = baseline.len().max(candidate.len());
    let budget_band = affordable_band(baseline.len(), candidate.len(), max_cells);
    let mut band = baseline.len().abs_diff(candidate.len());
    loop {
        let table = build_distance_table(baseline, candidate, band, max_cells, cancelled)?;
        // Leaving this band costs more than `band` insertions/deletions. A path
        // costing at most `band` is therefore globally optimal. A full matrix
        // is exact regardless of its distance, including unrelated inputs.
        if table.get(0, 0) <= band || band == full_band {
            return Ok(table);
        }
        let next_band = band
            .saturating_mul(2)
            .max(1)
            .min(full_band)
            .min(budget_band);
        if next_band <= band {
            return Err(Error::invalid("line diff budget", "budget exceeded"));
        }
        band = next_band;
    }
}

fn affordable_band(
    baseline_lines: usize,
    candidate_lines: usize,
    max_cells: Option<usize>,
) -> usize {
    let full_band = baseline_lines.max(candidate_lines);
    let Some(limit) = max_cells else {
        return full_band;
    };
    let width = limit / baseline_lines.saturating_add(1);
    if width > candidate_lines {
        full_band
    } else {
        width.saturating_sub(1) / 2
    }
}

fn common_length<'a>(
    pairs: impl Iterator<Item = (&'a u64, &'a u64)>,
    cancelled: &AtomicBool,
) -> Result<usize> {
    let mut count = 0;
    for (left, right) in pairs {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::invalid("line diff", "cancelled"));
        }
        if left != right {
            break;
        }
        count += 1;
    }
    Ok(count)
}

struct DistanceTable {
    rows: usize,
    columns: usize,
    width: usize,
    band: usize,
    cells: Vec<usize>,
}

impl DistanceTable {
    fn get(&self, row: usize, column: usize) -> usize {
        let start = row.saturating_sub(self.band);
        if row >= self.rows
            || column >= self.columns
            || column < start
            || column > row.saturating_add(self.band)
        {
            return usize::MAX;
        }
        self.cells[row * self.width + column - start]
    }

    fn fill(&mut self, baseline: &[u64], candidate: &[u64], cancelled: &AtomicBool) -> Result<()> {
        for old_index in (0..self.rows).rev() {
            if cancelled.load(Ordering::Relaxed) {
                return Err(Error::invalid("line diff", "cancelled"));
            }
            let start = old_index.saturating_sub(self.band);
            let end = old_index.saturating_add(self.band).min(candidate.len());
            for new_index in (start..=end).rev() {
                let distance = if old_index == baseline.len() {
                    candidate.len() - new_index
                } else if new_index == candidate.len() {
                    baseline.len() - old_index
                } else if baseline[old_index] == candidate[new_index] {
                    self.get(old_index + 1, new_index + 1)
                } else {
                    self.get(old_index + 1, new_index)
                        .min(self.get(old_index, new_index + 1))
                        .saturating_add(1)
                };
                self.cells[old_index * self.width + new_index - start] = distance;
            }
        }
        Ok(())
    }
}

fn build_distance_table(
    baseline: &[u64],
    candidate: &[u64],
    band: usize,
    max_cells: Option<usize>,
    cancelled: &AtomicBool,
) -> Result<DistanceTable> {
    let columns = candidate
        .len()
        .checked_add(1)
        .ok_or_else(|| Error::invalid("line diff", "candidate is too large"))?;
    let rows = baseline
        .len()
        .checked_add(1)
        .ok_or_else(|| Error::invalid("line diff", "baseline is too large"))?;
    let width = band.saturating_mul(2).saturating_add(1).min(columns);
    let matrix_len = rows
        .checked_mul(width)
        .ok_or_else(|| Error::invalid("line diff budget", "matrix size overflow"))?;
    if max_cells.is_some_and(|limit| matrix_len > limit) {
        return Err(Error::invalid("line diff budget", "budget exceeded"));
    }
    let mut table = DistanceTable {
        rows,
        columns,
        width,
        band,
        cells: vec![usize::MAX; matrix_len],
    };
    table.fill(baseline, candidate, cancelled)?;
    Ok(table)
}

fn collect_added_lines(
    baseline: &[u64],
    candidate: &[u64],
    distances: &DistanceTable,
) -> Vec<usize> {
    let mut added = Vec::new();
    let (mut old_index, mut new_index) = (0, 0);
    while new_index < candidate.len() {
        if old_index < baseline.len() && baseline[old_index] == candidate[new_index] {
            old_index += 1;
            new_index += 1;
        } else if old_index < baseline.len()
            && distances.get(old_index + 1, new_index) <= distances.get(old_index, new_index + 1)
        {
            old_index += 1;
        } else {
            added.push(new_index + 1);
            new_index += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::added_line_numbers;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn distant_edits_in_large_files_fit_the_cell_budget() {
        for baseline in [vec![1; 10_000], (0..10_000).collect::<Vec<u64>>()] {
            let mut candidate = baseline.clone();
            for position in [0, 5_000, 9_999] {
                candidate[position] = u64::MAX;
            }
            assert_eq!(
                added_line_numbers(
                    &baseline,
                    &candidate,
                    Some(4_000_000),
                    &AtomicBool::new(false)
                )
                .unwrap(),
                vec![1, 5_001, 10_000]
            );
        }
    }

    #[test]
    fn distant_insertions_and_deletions_keep_candidate_line_numbers() {
        let baseline = (0..10_000).collect::<Vec<u64>>();
        let mut candidate = baseline.clone();
        candidate.remove(100);
        candidate.insert(9_000, u64::MAX);
        candidate.insert(9_001, u64::MAX - 1);
        assert_eq!(
            added_line_numbers(&baseline, &candidate, Some(70_000), &AtomicBool::new(false))
                .unwrap(),
            vec![9_001, 9_002]
        );
    }

    #[test]
    fn small_edits_in_large_files_fit_the_cell_budget() {
        let baseline = vec![1; 10_000];
        for position in [0, 5_000, 9_999] {
            let mut candidate = baseline.clone();
            candidate[position] = 2;
            assert_eq!(
                added_line_numbers(&baseline, &candidate, Some(50_005), &AtomicBool::new(false))
                    .unwrap(),
                vec![position + 1]
            );
        }
    }

    #[test]
    fn repeated_suffix_keeps_the_earliest_match() {
        assert_eq!(
            added_line_numbers(&[1], &[2, 1, 1], Some(8), &AtomicBool::new(false)).unwrap(),
            vec![1, 3]
        );
    }

    #[test]
    fn irreducible_diff_still_obeys_the_allocation_budget() {
        let cancelled = AtomicBool::new(false);
        assert!(added_line_numbers(&[1, 2, 3], &[4, 5, 6], Some(15), &cancelled).is_err());
        assert_eq!(
            added_line_numbers(&[1, 2, 3], &[4, 5, 6], Some(16), &cancelled).unwrap(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn empty_sides_and_identical_files_need_no_matrix() {
        let cancelled = AtomicBool::new(false);
        for (baseline, candidate, expected) in [
            (vec![], vec![], vec![]),
            (vec![], vec![1, 2], vec![1, 2]),
            (vec![1, 2], vec![], vec![]),
            (vec![1; 10_000], vec![1; 10_000], vec![]),
            (vec![1], vec![1, 2, 3], vec![2, 3]),
        ] {
            assert_eq!(
                added_line_numbers(&baseline, &candidate, Some(0), &cancelled).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn cancellation_applies_to_fast_paths() {
        for (baseline, candidate) in [(vec![], vec![1]), (vec![1; 10_000], vec![1; 10_000])] {
            assert!(
                added_line_numbers(&baseline, &candidate, None, &AtomicBool::new(true)).is_err()
            );
        }
    }

    #[test]
    fn banded_mapping_matches_full_lcs_for_all_short_sequences() {
        let mut sequences = vec![vec![]];
        for length in 1..=4 {
            for mut encoded in 0..3_usize.pow(length) {
                let mut sequence = Vec::new();
                for _ in 0..length {
                    sequence.push((encoded % 3) as u64);
                    encoded /= 3;
                }
                sequences.push(sequence);
            }
        }
        for baseline in &sequences {
            for candidate in &sequences {
                assert_eq!(
                    added_line_numbers(baseline, candidate, None, &AtomicBool::new(false)).unwrap(),
                    full_lcs_added_lines(baseline, candidate),
                    "baseline={baseline:?}, candidate={candidate:?}"
                );
            }
        }
    }

    fn full_lcs_added_lines(baseline: &[u64], candidate: &[u64]) -> Vec<usize> {
        let columns = candidate.len() + 1;
        let mut lcs = vec![0; (baseline.len() + 1) * columns];
        for old in (0..baseline.len()).rev() {
            for new in (0..candidate.len()).rev() {
                lcs[old * columns + new] = if baseline[old] == candidate[new] {
                    1 + lcs[(old + 1) * columns + new + 1]
                } else {
                    lcs[(old + 1) * columns + new].max(lcs[old * columns + new + 1])
                };
            }
        }
        let (mut old, mut new) = (0, 0);
        let mut added = Vec::new();
        while new < candidate.len() {
            if old < baseline.len() && baseline[old] == candidate[new] {
                old += 1;
                new += 1;
            } else if old < baseline.len()
                && lcs[(old + 1) * columns + new] >= lcs[old * columns + new + 1]
            {
                old += 1;
            } else {
                added.push(new + 1);
                new += 1;
            }
        }
        added
    }
}
