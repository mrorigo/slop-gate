// Rust guideline compliant 2026-10-15
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
/// # Errors
/// Returns an error if the cell budget is exceeded, dimensions overflow, or
/// cancellation is observed between baseline rows.
pub(crate) fn added_line_numbers(
    baseline: &[u64],
    candidate: &[u64],
    max_cells: Option<usize>,
    cancelled: &AtomicBool,
) -> Result<Vec<usize>> {
    let cells = baseline
        .len()
        .checked_mul(candidate.len())
        .ok_or_else(|| Error::invalid("line diff budget", "cell count overflow"))?;
    if max_cells.is_some_and(|limit| cells > limit) {
        return Err(Error::invalid("line diff budget", "budget exceeded"));
    }
    let (columns, lcs) = build_lcs_table(baseline, candidate, cancelled)?;
    Ok(collect_added_lines(baseline, candidate, columns, &lcs))
}

fn build_lcs_table(
    baseline: &[u64],
    candidate: &[u64],
    cancelled: &AtomicBool,
) -> Result<(usize, Vec<usize>)> {
    let columns = candidate
        .len()
        .checked_add(1)
        .ok_or_else(|| Error::invalid("line diff", "candidate is too large"))?;
    let rows = baseline
        .len()
        .checked_add(1)
        .ok_or_else(|| Error::invalid("line diff", "baseline is too large"))?;
    let matrix_len = rows
        .checked_mul(columns)
        .ok_or_else(|| Error::invalid("line diff budget", "matrix size overflow"))?;
    let mut lcs = vec![0usize; matrix_len];
    for old_index in (0..baseline.len()).rev() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::invalid("line diff", "cancelled"));
        }
        for new_index in (0..candidate.len()).rev() {
            let cell = old_index * columns + new_index;
            lcs[cell] = if baseline[old_index] == candidate[new_index] {
                1 + lcs[(old_index + 1) * columns + new_index + 1]
            } else {
                lcs[(old_index + 1) * columns + new_index]
                    .max(lcs[old_index * columns + new_index + 1])
            };
        }
    }
    Ok((columns, lcs))
}

fn collect_added_lines(
    baseline: &[u64],
    candidate: &[u64],
    columns: usize,
    lcs: &[usize],
) -> Vec<usize> {
    let mut added = Vec::new();
    let (mut old_index, mut new_index) = (0, 0);
    while new_index < candidate.len() {
        if old_index < baseline.len() && baseline[old_index] == candidate[new_index] {
            old_index += 1;
            new_index += 1;
        } else if old_index < baseline.len()
            && lcs[(old_index + 1) * columns + new_index]
                >= lcs[old_index * columns + new_index + 1]
        {
            old_index += 1;
        } else {
            added.push(new_index + 1);
            new_index += 1;
        }
    }
    added
}
