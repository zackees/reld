use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use anyhow::{Context, Result};
use blake3::Hasher as Blake3;
use chrono::{TimeZone, Utc};
use globset::{Glob, GlobSetBuilder};
use indexmap::IndexMap;
use petgraph::graph::DiGraph;
use rayon::prelude::*;
use regex::RegexSet;
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::archive;
use crate::model::{Artifact, AuditResult, Policy};
use crate::{report, rules, store};

const POLICY: &str = r#"
include = ["target/**", "dist/**", "reports/**"]
deny = ["**/*.tmp", "**/*.secret", "**/private/**"]
max_bytes = 1048576
"#;

const RECORDS: &str = "path,kind,payload,deps\n\
target/release/artifact-auditor,binary,portable-binary,libcore;libreport\n\
dist/report.json,report,{\"status\":\"ok\"},artifact-auditor\n\
reports/summary.txt,text,all-checks-green,report.json\n\
target/cache/private/token.secret,secret,discard-me,\n";

fn artifacts(copies: usize) -> Result<Vec<Artifact>> {
    let mut reader = csv::Reader::from_reader(RECORDS.as_bytes());
    let rows = reader
        .records()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let observed_at = Utc
        .timestamp_opt(1_700_000_000, 0)
        .single()
        .context("valid fixture timestamp")?;
    let mut artifacts = Vec::with_capacity(rows.len() * copies);
    for copy in 0..copies {
        for (row_index, row) in rows.iter().enumerate() {
            let path = format!("copy-{copy}/{}", &row[0]);
            let mut labels = IndexMap::new();
            labels.insert("copy".to_owned(), copy.to_string());
            labels.insert("source".to_owned(), "benchmark-fixture".to_owned());
            artifacts.push(Artifact {
                // Stable IDs make Cargo's reference executable a byte-for-byte behavioral oracle
                // for every linker replay on this target.
                id: Uuid::from_u128(((copy as u128) << 64) | (row_index as u128 + 1)),
                path,
                kind: row[1].to_owned(),
                bytes: row[2].as_bytes().repeat(64 + copy),
                dependencies: row[3]
                    .split(';')
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect(),
                labels,
                observed_at,
            });
        }
    }
    Ok(artifacts)
}

fn classify(policy: &Policy, artifacts: &[Artifact]) -> Result<Vec<bool>> {
    let mut includes = GlobSetBuilder::new();
    let mut denies = GlobSetBuilder::new();
    for pattern in &policy.include {
        includes.add(Glob::new(&format!("**/{pattern}"))?);
    }
    for pattern in &policy.deny {
        denies.add(Glob::new(&format!("**/{pattern}"))?);
    }
    let includes = includes.build()?;
    let denies = denies.build()?;
    let suspicious = RegexSet::new([r"(?i)secret", r"(?i)private", r"\.tmp$"])?;
    Ok(artifacts
        .par_iter()
        .map(|artifact| {
            includes.is_match(&artifact.path)
                && !denies.is_match(&artifact.path)
                && !suspicious.is_match(&artifact.path)
                && artifact.bytes.len() <= policy.max_bytes
        })
        .collect())
}

fn graph(artifacts: &[Artifact]) -> (usize, u64) {
    let mut graph = DiGraph::<&str, ()>::new();
    let nodes = artifacts
        .iter()
        .map(|artifact| graph.add_node(artifact.path.as_str()))
        .collect::<Vec<_>>();
    for (index, artifact) in artifacts.iter().enumerate() {
        for dependency in &artifact.dependencies {
            let target = dependency.len() % nodes.len();
            graph.add_edge(nodes[index], nodes[target], ());
        }
    }
    let mut stable = BTreeMap::new();
    for node in graph.node_indices() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        graph[node].hash(&mut hasher);
        stable.insert(graph[node], hasher.finish());
    }
    (
        graph.edge_count(),
        stable.values().fold(0, |acc, value| acc ^ value),
    )
}

pub fn run(copies: usize) -> Result<AuditResult> {
    let policy: Policy = toml::from_str(POLICY)?;
    let artifacts = artifacts(copies.max(1))?;
    let accepted_mask = classify(&policy, &artifacts)?;
    let accepted = accepted_mask.iter().filter(|accepted| **accepted).count();
    let rejected = artifacts.len() - accepted;
    let (graph_edges, graph_hash) = graph(&artifacts);
    let sqlite_rows = store::persist(&artifacts)?;
    let compressed_bytes = archive::encode(&artifacts)?;
    let rendered_report = report::render(&artifacts, accepted, rejected)?;
    let walk_entries = exercise_walkdir_api();

    let client = reqwest::blocking::Client::builder()
        .user_agent("reld-link-benchmark/1")
        .build()?;
    let endpoint = Url::parse("https://example.invalid/artifacts")?;
    let request = client.get(endpoint).build()?;

    let mut blake3 = Blake3::new();
    blake3.update(rendered_report.as_bytes());
    blake3.update(request.url().as_str().as_bytes());
    blake3.update(&graph_hash.to_le_bytes());
    blake3.update(rules::compiled_policy());
    let mut sha256 = Sha256::new();
    sha256.update(blake3.finalize().as_bytes());
    sha256.update(compressed_bytes.to_le_bytes());
    sha256.update(walk_entries.to_le_bytes());
    let fingerprint = format!("{:x}", sha256.finalize());

    Ok(AuditResult {
        fingerprint,
        accepted,
        rejected,
        graph_edges,
        sqlite_rows,
        compressed_bytes,
        rendered_report,
    })
}

fn exercise_walkdir_api() -> usize {
    WalkDir::new(".")
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .count()
}
