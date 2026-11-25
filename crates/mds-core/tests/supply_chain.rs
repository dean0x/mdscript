//! The dependency gate of the core and the WASM build (#405, TP-98): no HTTP, TLS, URL,
//! async runtime or sender crate is in the normal-dependency tree of `mds-core` or
//! `mds-wasm` for any target, and no workspace crate depends on a logger crate.
//!
//! The gate reads two sources, and neither needs network access:
//! - `cargo metadata --no-deps --offline --locked` for the dependency kinds of each
//!   workspace crate, so a dev-dependency of a workspace crate is not in its tree;
//! - `Cargo.lock` for every other edge. The lock holds the packages of every target and
//!   every feature, and the optional edges that it keeps, so the gate checks a tree that
//!   is at least as wide as the tree of any build.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::process::Command;

/// The crates that the core and the WASM build must never depend on: HTTP, TLS, URL,
/// async runtimes, root lists and the native sender.
const NETWORK_CRATES: [&str; 21] = [
    "ureq",
    "ureq-proto",
    "http",
    "httparse",
    "rustls",
    "rustls-webpki",
    "rustls-pki-types",
    "ring",
    "webpki-roots",
    "webpki-root-certs",
    "url",
    "hyper",
    "hyper-util",
    "h2",
    "reqwest",
    "tokio",
    "async-std",
    "smol",
    "native-tls",
    "openssl",
    "mds-fetch",
];

/// Logger crates. A logger would write the request text that ureq logs at the debug and
/// trace levels, so no workspace crate may depend on one, also for its tests.
const LOGGER_CRATES: [&str; 6] = [
    "env_logger",
    "tracing-subscriber",
    "simplelog",
    "pretty_env_logger",
    "fern",
    "log4rs",
];

/// The normal-dependency names of each workspace crate.
type NormalDeps = HashMap<String, BTreeSet<String>>;

/// One `[[package]]` of a lock file.
#[derive(Debug, Default)]
struct Package {
    name: String,
    version: String,
    /// The package has a `source`: it is not a workspace package.
    registry: bool,
    /// Each dependency: the name and, when the lock gives it, the version.
    dependencies: Vec<(String, Option<String>)>,
}

/// The packages of a lock file. The value of `name`, `version` and each item of a
/// `dependencies` list is a basic string; other keys are skipped.
fn parse_lock(text: &str) -> Vec<Package> {
    let mut packages: Vec<Package> = Vec::new();
    let mut in_dependencies = false;
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            packages.push(Package::default());
            in_dependencies = false;
            continue;
        }
        let Some(package) = packages.last_mut() else {
            continue;
        };
        if in_dependencies {
            if line == "]" {
                in_dependencies = false;
            } else if let Some(entry) = quoted(line.trim_end_matches(',')) {
                let mut parts = entry.split(' ');
                let name = parts.next().unwrap_or_default().to_owned();
                let version = parts.next().map(str::to_owned);
                package.dependencies.push((name, version));
            }
        } else if let Some(value) = line.strip_prefix("name = ").and_then(quoted) {
            package.name = value.to_owned();
        } else if let Some(value) = line.strip_prefix("version = ").and_then(quoted) {
            package.version = value.to_owned();
        } else if line.starts_with("source = ") {
            package.registry = true;
        } else if line == "dependencies = [" {
            in_dependencies = true;
        }
    }
    packages
}

fn quoted(text: &str) -> Option<&str> {
    text.strip_prefix('"')?.strip_suffix('"')
}

/// The names of the packages that `roots` (workspace crate names) reach, the roots
/// included. With `normal`, a workspace crate follows only its normal dependencies of
/// every target; without it, every edge of the lock. A dependency that names no
/// package, a name with two packages and no version, or a workspace crate that
/// `normal` does not list fails the gate.
fn closure(
    packages: &[Package],
    normal: Option<&NormalDeps>,
    roots: &[&str],
) -> Result<BTreeSet<String>, String> {
    let mut queue = VecDeque::new();
    for root in roots {
        let index = packages
            .iter()
            .position(|package| package.name == *root && !package.registry)
            .ok_or_else(|| format!("the lock has no workspace crate {root}"))?;
        queue.push_back(index);
    }
    let mut seen = vec![false; packages.len()];
    let mut names = BTreeSet::new();
    // Each package is expanded once, so the loop ends after the package count.
    while let Some(index) = queue.pop_front() {
        if std::mem::replace(&mut seen[index], true) {
            continue;
        }
        let package = &packages[index];
        names.insert(package.name.clone());
        let kept = match (normal, package.registry) {
            (Some(normal), false) => Some(
                normal
                    .get(&package.name)
                    .ok_or_else(|| format!("no dependency kinds for {}", package.name))?,
            ),
            _ => None,
        };
        for (name, version) in &package.dependencies {
            if kept.is_some_and(|kept| !kept.contains(name)) {
                continue;
            }
            let matches: Vec<usize> = (0..packages.len())
                .filter(|&other| {
                    packages[other].name == *name
                        && version
                            .as_ref()
                            .is_none_or(|v| packages[other].version == *v)
                })
                .collect();
            match matches.as_slice() {
                [one] => queue.push_back(*one),
                [] => return Err(format!("{} names a missing package {name}", package.name)),
                _ => return Err(format!("{} names {name} with no version", package.name)),
            }
        }
    }
    Ok(names)
}

/// The names of `banned` that the tree of `roots` holds.
fn banned_in(
    lock: &str,
    normal: Option<&NormalDeps>,
    roots: &[&str],
    banned: &[&str],
) -> Result<Vec<String>, String> {
    let names = closure(&parse_lock(lock), normal, roots)?;
    Ok(banned
        .iter()
        .filter(|name| names.contains(**name))
        .map(|name| (*name).to_owned())
        .collect())
}

const WORKSPACE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn workspace_lock() -> String {
    std::fs::read_to_string(format!("{WORKSPACE}/Cargo.lock"))
        .expect("the workspace Cargo.lock is readable")
}

/// The normal-dependency names of each workspace crate, of every target and with every
/// optional dependency, from `cargo metadata`. `--no-deps` and `--offline` read the
/// workspace manifests only.
fn workspace_normal_deps() -> NormalDeps {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--locked",
            "--format-version",
            "1",
        ])
        .current_dir(WORKSPACE)
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().expect("a package list");
    packages
        .iter()
        .map(|package| {
            let names = package["dependencies"]
                .as_array()
                .expect("a dependency list")
                .iter()
                .filter(|dependency| dependency["kind"].is_null())
                .map(|dependency| dependency["name"].as_str().unwrap().to_owned())
                .collect();
            (package["name"].as_str().unwrap().to_owned(), names)
        })
        .collect()
}

/// TP-98: the core and the WASM build depend on no HTTP, TLS, URL, async or sender
/// crate. Controls: the tree holds crates that the core and the WASM build use, and the
/// lock lists a dev-dependency of the WASM crate that the tree leaves out. The same gate
/// finds the network crates of the sender.
#[test]
fn the_core_and_wasm_trees_hold_no_network_crate() {
    let lock = workspace_lock();
    let normal = workspace_normal_deps();
    let roots = ["mds-core", "mds-wasm"];
    assert_eq!(
        banned_in(&lock, Some(&normal), &roots, &NETWORK_CRATES),
        Ok(Vec::new())
    );
    let packages = parse_lock(&lock);
    let names = closure(&packages, Some(&normal), &roots).unwrap();
    for used in ["mds-core", "mds-wasm", "serde_json", "wasm-bindgen"] {
        assert!(names.contains(used), "{used}");
    }
    let wasm = packages
        .iter()
        .find(|package| package.name == "mds-wasm")
        .unwrap();
    assert!(wasm
        .dependencies
        .iter()
        .any(|(name, _)| name == "wasm-bindgen-test"));
    assert!(!names.contains("wasm-bindgen-test"));
    let sender = banned_in(&lock, Some(&normal), &["mds-fetch"], &NETWORK_CRATES).unwrap();
    for found in ["ureq", "rustls", "ring", "webpki-root-certs", "mds-fetch"] {
        assert!(
            sender.iter().any(|name| name == found),
            "{found}: {sender:?}"
        );
    }
}

/// No workspace crate depends on a logger crate, for any kind of dependency. Control:
/// the workspace tree holds the sender and the `log` facade that ureq uses.
#[test]
fn no_workspace_tree_holds_a_logger() {
    let lock = workspace_lock();
    let members: Vec<String> = parse_lock(&lock)
        .into_iter()
        .filter(|package| !package.registry)
        .map(|package| package.name)
        .collect();
    let roots: Vec<&str> = members.iter().map(String::as_str).collect();
    assert!(roots.contains(&"mds-fetch"), "{roots:?}");
    assert_eq!(
        banned_in(&lock, None, &roots, &LOGGER_CRATES),
        Ok(Vec::new())
    );
    let names = closure(&parse_lock(&lock), None, &roots).unwrap();
    assert!(names.contains("ureq") && names.contains("log"));
}

/// A planted lock where the core reaches `ureq` and, through it, `rustls`.
const PLANTED: &str = r#"
[[package]]
name = "mds-core"
version = "0.7.0"
dependencies = [
 "serde",
 "ureq",
]

[[package]]
name = "mds-wasm"
version = "0.7.0"
dependencies = [
 "mds-core",
]

[[package]]
name = "mds-cli"
version = "0.7.0"
dependencies = [
 "env_logger",
 "mds-core",
]

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "ureq"
version = "3.5.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "rustls 0.23.46",
]

[[package]]
name = "rustls"
version = "0.23.46"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "rustls"
version = "0.21.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "env_logger"
version = "0.11.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

/// The dependency kinds of [`PLANTED`] with `core` as the normal dependencies of
/// `mds-core`.
fn planted_kinds(core: &[&str]) -> NormalDeps {
    let set = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    HashMap::from([
        ("mds-core".to_owned(), set(core)),
        ("mds-wasm".to_owned(), set(&["mds-core"])),
        ("mds-cli".to_owned(), set(&["env_logger", "mds-core"])),
    ])
}

/// TP-98 control: the gate finds a planted HTTP crate and the TLS crate below it, from
/// the core and from the WASM build, and a planted logger in another workspace crate.
/// The same crate as a dev-dependency of the core is not in its tree.
#[test]
fn the_gate_finds_planted_crates() {
    let normal = planted_kinds(&["serde", "ureq"]);
    let expected = Ok(vec!["ureq".to_owned(), "rustls".to_owned()]);
    for root in ["mds-core", "mds-wasm"] {
        let found = banned_in(PLANTED, Some(&normal), &[root], &NETWORK_CRATES);
        assert_eq!(found, expected, "{root}");
    }
    assert_eq!(
        banned_in(PLANTED, None, &["mds-cli"], &LOGGER_CRATES),
        Ok(vec!["env_logger".to_owned()])
    );
    let dev_only = planted_kinds(&["serde"]);
    assert_eq!(
        banned_in(PLANTED, Some(&dev_only), &["mds-wasm"], &NETWORK_CRATES),
        Ok(Vec::new())
    );
}

/// The gate fails closed on a lock that it can not follow: a missing root, a missing
/// dependency, an ambiguous name or a workspace crate with no dependency kinds.
#[test]
fn the_gate_fails_closed_on_an_unknown_edge() {
    let normal = planted_kinds(&["serde", "ureq"]);
    let check = |lock: &str, normal: &NormalDeps| {
        banned_in(lock, Some(normal), &["mds-core"], &NETWORK_CRATES)
    };
    assert!(check(PLANTED, &normal).is_ok());
    assert!(banned_in(PLANTED, Some(&normal), &["mds-napi"], &NETWORK_CRATES).is_err());
    let renamed = PLANTED.replace(" \"serde\",\n", " \"serde_json\",\n");
    assert!(check(&renamed, &planted_kinds(&["serde_json", "ureq"])).is_err());
    assert!(check(
        &PLANTED.replace("\"rustls 0.23.46\"", "\"rustls 9.9.9\""),
        &normal
    )
    .is_err());
    assert!(check(
        &PLANTED.replace("\"rustls 0.23.46\"", "\"rustls\""),
        &normal
    )
    .is_err());
    let mut unknown = normal.clone();
    unknown.remove("mds-core");
    assert!(check(PLANTED, &unknown).is_err());
}
