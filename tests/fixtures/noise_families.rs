// Shapes an adopter triaged by hand as unavoidable duplication. They exist so
// block detection cannot regress into reporting idiomatic repetition. None of
// these should produce a block-scope near-clone finding.

/// Pairwise logical conjunction and disjunction over three values.
///
/// The parallel structure is the specification of Kleene-3 logic, not an
/// accident of copy and paste.
pub mod kleene {
    /// Strong Kleene-3 conjunction.
    pub fn and(left: Option<bool>, right: Option<bool>) -> Option<bool> {
        match (left, right) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        }
    }

    /// Strong Kleene-3 disjunction.
    pub fn or(left: Option<bool>, right: Option<bool>) -> Option<bool> {
        match (left, right) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        }
    }

    /// Conjunction of a list of values.
    pub fn and_all(values: &[Option<bool>]) -> Option<bool> {
        let mut result = Some(true);
        for value in values {
            result = and(result, *value);
        }
        result
    }

    /// Disjunction of a list of values.
    pub fn or_all(values: &[Option<bool>]) -> Option<bool> {
        let mut result = Some(false);
        for value in values {
            result = or(result, *value);
        }
        result
    }
}

/// Paired gather kernels.
///
/// The two kernels mirror each other on purpose: that mirroring is the
/// two-times speedup over a strided gather.
pub mod gather {
    /// Gathers contiguous rows into column-major buffers.
    pub fn column_to_row(source: &[f32], width: usize, height: usize) -> Vec<f32> {
        let mut output = vec![0.0_f32; width * height];
        for column in 0..width {
            for row in 0..height {
                output[row * width + column] = source[column * height + row];
            }
        }
        output
    }

    /// Gathers contiguous rows into row-major buffers.
    pub fn row_to_column(source: &[f32], width: usize, height: usize) -> Vec<f32> {
        let mut output = vec![0.0_f32; width * height];
        for row in 0..height {
            for column in 0..width {
                output[column * height + row] = source[row * width + column];
            }
        }
        output
    }
}

/// Struct-variant error constructors.
pub mod failures {
    /// A configuration error.
    #[derive(Debug)]
    pub struct ConfigError {
        /// Offending key.
        pub key: String,
    }

    /// A decoding error.
    #[derive(Debug)]
    pub struct DecodeError {
        /// Byte offset where decoding stopped.
        pub offset: usize,
    }

    impl ConfigError {
        /// Builds a configuration error for one key.
        pub fn new(key: impl Into<String>) -> Self {
            Self { key: key.into() }
        }
    }

    impl DecodeError {
        /// Builds a decoding error for one offset.
        pub fn new(offset: usize) -> Self {
            Self { offset }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::kleene::{and, and_all, or, or_all};

    /// Parallel table tests, correct as written.
    #[test]
    fn conjunction_table() {
        assert_eq!(and(Some(true), Some(true)), Some(true));
        assert_eq!(and(Some(true), Some(false)), Some(false));
        assert_eq!(and(None, Some(true)), None);
        assert_eq!(and_all(&[Some(true), Some(true)]), Some(true));
        assert_eq!(and_all(&[Some(true), None]), None);
    }

    /// The mirrored table for disjunction.
    #[test]
    fn disjunction_table() {
        assert_eq!(or(Some(true), Some(false)), Some(true));
        assert_eq!(or(Some(false), Some(false)), Some(false));
        assert_eq!(or(None, Some(true)), Some(true));
        assert_eq!(or_all(&[Some(false), Some(true)]), Some(true));
        assert_eq!(or_all(&[None, Some(false)]), None);
    }
}
