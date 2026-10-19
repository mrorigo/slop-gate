// Rust guideline compliant 2026-10-19
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
/// Common boundaries bound the insertion/deletion distance and therefore the
/// matrix band. Equal-cost paths still prefer deletion, preserving attribution
/// when repeated lines permit several longest common subsequences.
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
    let baseline = &baseline[prefix..];
    let candidate = &candidate[prefix..];
    if candidate.is_empty() {
        return Ok(Vec::new());
    }
    if baseline.is_empty() {
        return Ok((prefix + 1..=prefix + candidate.len()).collect());
    }
    let suffix = common_length(baseline.iter().rev().zip(candidate.iter().rev()), cancelled)?;
    // Replacing the middle while keeping the suffix bounds the edit distance.
    // Every optimal path fits inside that band. Retain the suffix in the table
    // because anchoring it directly can change which repeated lines are added.
    let distance_bound = (baseline.len() - suffix)
        .checked_add(candidate.len() - suffix)
        .ok_or_else(|| Error::invalid("line diff budget", "distance bound overflow"))?;
    let distances =
        build_distance_table(baseline, candidate, distance_bound, max_cells, cancelled)?;
    Ok(collect_added_lines(baseline, candidate, &distances)
        .into_iter()
        .map(|line| line + prefix)
        .collect())
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
    for old_index in (0..rows).rev() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::invalid("line diff", "cancelled"));
        }
        let start = old_index.saturating_sub(band);
        let end = old_index.saturating_add(band).min(candidate.len());
        for new_index in (start..=end).rev() {
            let distance = if old_index == baseline.len() {
                candidate.len() - new_index
            } else if new_index == candidate.len() {
                baseline.len() - old_index
            } else if baseline[old_index] == candidate[new_index] {
                table.get(old_index + 1, new_index + 1)
            } else {
                table
                    .get(old_index + 1, new_index)
                    .min(table.get(old_index, new_index + 1))
                    .saturating_add(1)
            };
            table.cells[old_index * width + new_index - start] = distance;
        }
    }
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
