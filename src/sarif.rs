// Rust guideline compliant 2026-09-12
//! SARIF 2.1.0 projection for gate findings.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::gate::{CheckReport, Finding, Severity};

/// Renders a report as a SARIF 2.1.0 document.
pub(crate) fn render(report: &CheckReport) -> Value {
    let rule_ids = report
        .findings
        .iter()
        .map(|finding| finding.rule_id.clone())
        .collect::<BTreeSet<_>>();
    let rules = rule_ids
        .into_iter()
        .map(|id| json!({ "id": id, "name": id }))
        .collect::<Vec<_>>();
    let results = report
        .findings
        .iter()
        .map(render_finding)
        .collect::<Vec<_>>();
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "slop-gate",
                    "informationUri": "https://github.com/mrorigo/slop-gate",
                    "rules": rules
                }
            },
            "results": results
        }]
    })
}

fn render_finding(finding: &Finding) -> Value {
    let mut result = json!({
        "ruleId": finding.rule_id,
        "level": sarif_level(finding.severity),
        "message": { "text": finding.message },
        "locations": [{
            "physicalLocation": {
                "artifactLocation": { "uri": finding.location.path },
                "region": { "startLine": finding.location.line }
            }
        }],
        "properties": {
            "base_mass": finding.base_mass,
            "head_mass": finding.head_mass,
            "delta": finding.delta,
            "threshold": finding.threshold,
            "similarity": finding.similarity
        }
    });
    if let Some(properties) = result["properties"].as_object_mut() {
        for (key, value) in &finding.properties {
            properties.insert(key.clone(), Value::String(value.clone()));
        }
    }
    if let Some(location) = &finding.base_location {
        result["relatedLocations"] = json!([{
            "id": 1,
            "physicalLocation": {
                "artifactLocation": { "uri": location.path },
                "region": { "startLine": location.line }
            }
        }]);
    }
    result
}

fn sarif_level(severity: Severity) -> &'static str {
    match severity {
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::render;
    use crate::gate::{CheckReport, Finding, Location, Severity};

    #[test]
    fn renders_required_sarif_fields() {
        let report = CheckReport {
            findings: vec![Finding {
                rule_id: "near-clone".to_string(),
                severity: Severity::Error,
                message: "duplicate".to_string(),
                location: Location {
                    path: "src/new.rs".to_string(),
                    line: 8,
                },
                base_location: Some(Location {
                    path: "src/old.rs".to_string(),
                    line: 3,
                }),
                base_mass: None,
                head_mass: None,
                delta: None,
                threshold: Some(0.85),
                similarity: Some(1.0),
                properties: BTreeMap::from([(String::from("pattern_id"), String::from("allow"))]),
            }],
        };
        let sarif = render(&report);
        assert_eq!(sarif["version"], "2.1.0");
        assert_eq!(sarif["runs"][0]["results"][0]["ruleId"], "near-clone");
        assert_eq!(sarif["runs"][0]["results"][0]["level"], "error");
        assert_eq!(
            sarif["runs"][0]["results"][0]["properties"]["pattern_id"],
            "allow"
        );
    }
}
