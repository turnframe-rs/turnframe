//! `turnframe-core` stays free of async runtimes, HTTP clients, database
//! drivers and application code — enforced here rather than in review.
//!
//! The first adopter asked for exactly this, and gave the reason: the
//! dependency direction is one of the things that makes adopting the library
//! tractable, and a convention that is only written down decays. So this test
//! walks the resolved dependency graph rather than reading a manifest, and
//! fails naming both the offender and the edge that pulled it in.
//!
//! It works offline and adds no dependency: the graph comes from
//! `cargo metadata`, which every machine that can run this test already has,
//! and the JSON is parsed with the `serde_json` the core already depends on.
//! Only *normal* dependency edges are followed. Development and build edges are
//! deliberately excluded, because the core's own tests do use a runtime — this
//! file's job is the library that ships, not the harness that checks it.
//!
//! Three properties are checked, and they fail differently on purpose:
//!
//! 1. no forbidden crate is reachable from the core at all;
//! 2. no other crate of this workspace is reachable, so the layering cannot
//!    invert;
//! 3. the core's direct dependencies are still the reviewed set, so a new one
//!    is a decision somebody makes on purpose rather than a diff nobody read.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;

/// The crate under guard.
const CORE: &str = "turnframe-core";

/// What the core must never reach through a normal dependency edge, with the
/// reason each entry is on the list.
///
/// Matching is on the resolved package name, which is an identity, not a
/// substring: `tokio` is forbidden and a hypothetical `tokio-inspired-parser`
/// is not, and neither is caught or missed by accident.
const FORBIDDEN: &[(&str, &str)] = &[
    // Async runtimes and executors. The core is synchronous; the one async
    // signature it declares is a trait, and a trait needs no runtime.
    ("tokio", "async runtime"),
    ("tokio-util", "async runtime"),
    ("async-std", "async runtime"),
    ("smol", "async runtime"),
    ("async-executor", "async runtime"),
    ("async-global-executor", "async runtime"),
    ("futures-executor", "async runtime"),
    ("actix-rt", "async runtime"),
    ("mio", "event loop"),
    ("polling", "event loop"),
    // Anything that opens a socket. A crate of pure types has nobody to talk
    // to, and an adopter must be free to choose their own client stack.
    ("reqwest", "HTTP client"),
    ("hyper", "HTTP stack"),
    ("hyper-util", "HTTP stack"),
    ("h2", "HTTP stack"),
    ("ureq", "HTTP client"),
    ("isahc", "HTTP client"),
    ("surf", "HTTP client"),
    ("curl", "HTTP client"),
    ("tonic", "gRPC stack"),
    ("axum", "HTTP server"),
    ("actix-web", "HTTP server"),
    ("tower", "service stack"),
    ("socket2", "socket layer"),
    ("rustls", "TLS stack"),
    ("native-tls", "TLS stack"),
    ("openssl", "TLS stack"),
    ("openssl-sys", "TLS stack"),
    // Database drivers. Persistence is an extension point, not a dependency:
    // the store traits live in `turnframe-store` and the adapters below it.
    ("sqlx", "database driver"),
    ("sqlx-core", "database driver"),
    ("sqlx-postgres", "database driver"),
    ("diesel", "database driver"),
    ("sea-orm", "database driver"),
    ("tokio-postgres", "database driver"),
    ("postgres", "database driver"),
    ("rusqlite", "database driver"),
    ("libsqlite3-sys", "database driver"),
    ("mysql_async", "database driver"),
    ("mongodb", "database driver"),
    ("redis", "database driver"),
    // Things that configure a process. A library that installs a subscriber, an
    // exporter or a command line has made a decision that belongs to the
    // application that embeds it.
    ("tracing-subscriber", "installs a global subscriber"),
    ("opentelemetry_sdk", "telemetry pipeline"),
    ("metrics-exporter-prometheus", "telemetry pipeline"),
    ("clap", "command-line surface"),
    ("dotenvy", "process environment"),
];

/// The direct normal dependencies the core is reviewed to have.
///
/// Every entry is either the serialization contract itself or a piece of the
/// determinism the architecture rests on: canonical hashing, ordered maps,
/// schema generation and validation for model-facing arguments, typed errors,
/// identifiers and timestamps.
const REVIEWED_DIRECT_DEPENDENCIES: &[&str] = &[
    "async-trait",
    "blake3",
    "chrono",
    "indexmap",
    "jsonschema",
    "schemars",
    "serde",
    "serde_json",
    "thiserror",
    "uuid",
];

/// The resolved workspace graph, read once for the whole test binary.
fn metadata() -> &'static Value {
    static METADATA: OnceLock<Value> = OnceLock::new();
    METADATA.get_or_init(|| {
        let manifest = workspace_root().join("Cargo.toml");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        // Offline first, because this check must not depend on a network. A
        // machine that has not resolved the workspace yet gets the online
        // attempt as a fallback rather than a confusing failure.
        let offline = run_metadata(&cargo, &manifest, true);
        let output = match offline {
            Ok(json) => json,
            Err(offline_error) => run_metadata(&cargo, &manifest, false).unwrap_or_else(|error| {
                panic!(
                    "`cargo metadata` did not produce a dependency graph.\n\
                     offline attempt: {offline_error}\n\
                     online attempt:  {error}"
                )
            }),
        };
        serde_json::from_str(&output).expect("`cargo metadata` emits JSON")
    })
}

/// Runs `cargo metadata` and returns its stdout.
fn run_metadata(cargo: &str, manifest: &Path, offline: bool) -> Result<String, String> {
    let mut command = Command::new(cargo);
    command
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--all-features")
        .arg("--manifest-path")
        .arg(manifest);
    if offline {
        command.arg("--offline");
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run `{cargo} metadata`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("non-UTF-8 output: {error}"))
}

/// The directory holding the workspace manifest, found by walking up from this
/// crate rather than by counting `..` segments.
fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file()
            && std::fs::read_to_string(&manifest).is_ok_and(|text| text.contains("[workspace]"))
        {
            return dir;
        }
        if !dir.pop() {
            panic!("no workspace manifest above {}", env!("CARGO_MANIFEST_DIR"));
        }
    }
}

/// Package name by resolved package id.
fn names() -> HashMap<&'static str, &'static str> {
    metadata()["packages"]
        .as_array()
        .expect("`packages` is an array")
        .iter()
        .map(|package| {
            (
                package["id"].as_str().expect("a package has an id"),
                package["name"].as_str().expect("a package has a name"),
            )
        })
        .collect()
}

/// Normal-edge adjacency by resolved package id.
///
/// A dependency with no `dep_kinds` would be indistinguishable from a
/// development one, so an empty list is a hard failure rather than a guess: it
/// means the `cargo` in use is too old for this check to mean anything.
fn normal_edges() -> HashMap<&'static str, Vec<&'static str>> {
    metadata()["resolve"]["nodes"]
        .as_array()
        .expect("`resolve.nodes` is an array")
        .iter()
        .map(|node| {
            let id = node["id"].as_str().expect("a node has an id");
            let deps = node["deps"]
                .as_array()
                .expect("a node has deps")
                .iter()
                .filter(|dep| {
                    let kinds = dep["dep_kinds"]
                        .as_array()
                        .expect("cargo reports dependency kinds");
                    assert!(
                        !kinds.is_empty(),
                        "cargo reported a dependency of {id} with no kind, so normal and \
                         development edges cannot be told apart"
                    );
                    // A `null` kind is a normal dependency; `dev` and `build`
                    // are the other two and are not followed.
                    kinds.iter().any(|kind| kind["kind"].is_null())
                })
                .map(|dep| dep["pkg"].as_str().expect("a dep names a package"))
                .collect();
            (id, deps)
        })
        .collect()
}

/// The resolved id of a workspace member.
fn member_id(name: &str) -> &'static str {
    let names = names();
    metadata()["workspace_members"]
        .as_array()
        .expect("`workspace_members` is an array")
        .iter()
        .filter_map(Value::as_str)
        .find(|id| names.get(id).copied() == Some(name))
        .unwrap_or_else(|| panic!("{name} is not a member of this workspace"))
}

/// Every package reachable from the core over normal edges, with the edge that
/// first reached it.
fn reachable_from_core() -> HashMap<&'static str, Option<&'static str>> {
    let edges = normal_edges();
    let root = member_id(CORE);
    let mut parents: HashMap<&str, Option<&str>> = HashMap::from([(root, None)]);
    let mut queue = VecDeque::from([root]);
    while let Some(current) = queue.pop_front() {
        for dependency in edges.get(current).into_iter().flatten() {
            if parents.contains_key(dependency) {
                continue;
            }
            parents.insert(dependency, Some(current));
            queue.push_back(dependency);
        }
    }
    parents
}

/// Renders `core -> ... -> package`, which is the part of a failure that tells
/// you what to actually change.
fn path_to(parents: &HashMap<&str, Option<&str>>, package: &str) -> String {
    let names = names();
    let mut chain = Vec::new();
    let mut cursor = Some(package);
    while let Some(id) = cursor {
        chain.push(names.get(id).copied().unwrap_or(id));
        cursor = parents.get(id).copied().flatten();
    }
    chain.reverse();
    chain.join(" -> ")
}

/// No async runtime, HTTP client, database driver or process-configuring crate
/// is reachable from the core.
#[test]
fn the_core_reaches_no_forbidden_crate() {
    let parents = reachable_from_core();
    let names = names();
    let mut offences = Vec::new();
    for id in parents.keys() {
        let Some(name) = names.get(id).copied() else {
            continue;
        };
        if let Some((_, reason)) = FORBIDDEN.iter().find(|(forbidden, _)| *forbidden == name) {
            offences.push(format!("  {name} ({reason}) via {}", path_to(&parents, id)));
        }
    }
    offences.sort();
    assert!(
        offences.is_empty(),
        "{CORE} must stay free of async runtimes, HTTP clients, database drivers and \
         application dependencies, and now reaches:\n{}\n\nRemove the edge, or move the code \
         that needs it into the crate that owns that concern.",
        offences.join("\n")
    );
    println!(
        "{CORE} reaches {} packages over normal edges, none of them forbidden",
        parents.len() - 1
    );
}

/// The layering cannot invert: nothing else in this workspace is below the
/// core.
#[test]
fn the_core_depends_on_no_other_workspace_crate() {
    let parents = reachable_from_core();
    let names = names();
    let root = member_id(CORE);
    let mut offences: Vec<String> = metadata()["workspace_members"]
        .as_array()
        .expect("`workspace_members` is an array")
        .iter()
        .filter_map(Value::as_str)
        .filter(|id| *id != root && parents.contains_key(id))
        .map(|id| {
            format!(
                "  {} via {}",
                names.get(id).copied().unwrap_or(id),
                path_to(&parents, id)
            )
        })
        .collect();
    offences.sort();
    assert!(
        offences.is_empty(),
        "{CORE} is the bottom of this workspace and must depend on none of its other crates, \
         and now reaches:\n{}",
        offences.join("\n")
    );
}

/// The core's direct dependencies are still the reviewed set.
///
/// This is the check that catches what the list of forbidden names cannot: a
/// new direct dependency nobody thought to forbid, which arrives with a
/// transitive closure nobody looked at. Adding one is allowed — it just has to
/// be a decision, recorded here.
#[test]
fn the_core_has_only_its_reviewed_direct_dependencies() {
    let edges = normal_edges();
    let names = names();
    let root = member_id(CORE);
    let actual: BTreeSet<&str> = edges
        .get(root)
        .into_iter()
        .flatten()
        .filter_map(|id| names.get(id).copied())
        .collect();
    let reviewed: BTreeSet<&str> = REVIEWED_DIRECT_DEPENDENCIES.iter().copied().collect();

    let added: Vec<&str> = actual.difference(&reviewed).copied().collect();
    let removed: Vec<&str> = reviewed.difference(&actual).copied().collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "the direct dependencies of {CORE} are no longer the reviewed set.\n\
         added: {added:?}\nremoved: {removed:?}\n\n\
         Adding one is a decision about what every adopter has to compile: make it on purpose, \
         then record it in REVIEWED_DIRECT_DEPENDENCIES in this file."
    );
}
