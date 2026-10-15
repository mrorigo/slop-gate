use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use slop_gate::api::{
    ChangeManifest, EntryKind, ResourceLimits, SnapshotEntry, SourceSnapshot, build_index,
    evaluate, function_records, parse_policy,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let policy = parse_policy(b"")?;
    let baseline = SourceSnapshot {
        id: "captured-base".into(),
        entries: vec![SnapshotEntry {
            path: "src/lib.rs".into(),
            kind: EntryKind::File,
            mode: 0o100644,
            bytes: b"fn answer() -> u8 { 42 }\n".to_vec(),
        }],
    };
    let candidate = SourceSnapshot {
        id: "uncommitted-candidate".into(),
        entries: vec![SnapshotEntry {
            path: "src/lib.rs".into(),
            kind: EntryKind::File,
            mode: 0o100644,
            bytes: b"fn answer() -> u8 { if true { 42 } else { 0 } }\nfn added() { if true { if true { if true { if true { if true { if true { if true { if true { if true { if true {} } } } } } } } } } }\n".to_vec(),
        }],
    };
    let cancelled = AtomicBool::new(false);
    let baseline_index = build_index(&baseline, &policy, &cancelled, ResourceLimits::default())?;
    let _persisted = baseline_index.to_json()?;
    let manifest = ChangeManifest {
        changed: BTreeSet::from(["src/lib.rs".into()]),
        ..ChangeManifest::default()
    };
    let report = evaluate(
        &baseline_index,
        &candidate,
        &manifest,
        &policy,
        &cancelled,
        ResourceLimits::default(),
    )?;
    println!(
        "findings: {}, complete: {}",
        report.findings.len(),
        report.completeness.complete
    );
    let candidate_index = build_index(&candidate, &policy, &cancelled, ResourceLimits::default())?;
    println!(
        "candidate functions: {}",
        function_records(&candidate_index).len()
    );
    for record in &report.coverage {
        println!("{}: {:?}", record.path, record.status);
    }
    Ok(())
}
