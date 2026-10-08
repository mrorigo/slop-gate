// Rust guideline compliant 2026-09-27
//! Emits a content fingerprint of the analyzer sources and parser dependencies.
//!
//! The fingerprint is mixed into the index-artifact compatibility key so a
//! change to extraction, normalization, or summarization invalidates artifacts
//! automatically. Previously the key was a hand-maintained string that no test
//! tied to the analyzer, so a parser change shipped inside a patch release and
//! stale artifacts failed validation with an unactionable message.
//!
//! The digest is FNV-1a rather than a crate hash so the build script has no
//! dependencies and produces the same value on every toolchain.

use std::path::Path;

const ANALYZER_INPUTS: [&str; 8] = [
    "Cargo.toml",
    "Cargo.lock",
    "src/analysis/extract.rs",
    "src/analysis/python.rs",
    "src/analysis/typescript.rs",
    "src/analysis/model.rs",
    "src/analysis/artifact.rs",
    "src/analysis/mod.rs",
];

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn main() {
    let mut hash = FNV_OFFSET;
    for input in ANALYZER_INPUTS {
        println!("cargo::rerun-if-changed={input}");
        let contents = std::fs::read(Path::new(input))
            .unwrap_or_else(|error| panic!("failed to read analyzer input {input}: {error}"));
        hash = absorb(hash, input.as_bytes());
        hash = absorb(hash, &[0]);
        hash = absorb(hash, &contents);
        hash = absorb(hash, &[0]);
    }
    println!("cargo::rustc-env=SLOP_GATE_ANALYZER_SOURCE_FINGERPRINT={hash:016x}");
}

fn absorb(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}
