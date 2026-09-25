//! The MCP Registry refuses a listing whose packages do not name it, or
//! whose versions are not the ones published. Catch that here, before a
//! release, rather than at `mcp-publisher publish`.

use std::path::PathBuf;

use serde_json::Value;

const NAME: &str = "io.github.harisnopen/diavlos";

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn json(path: &str) -> Value {
    let text = std::fs::read_to_string(repo().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn server_json_matches_the_packages() {
    let version = env!("CARGO_PKG_VERSION");
    let server = json("server.json");
    assert_eq!(server["name"], NAME);
    assert_eq!(server["version"], version, "server.json version");
    let packages = server["packages"].as_array().expect("packages");
    assert!(!packages.is_empty());
    for p in packages {
        assert_eq!(
            p["version"], version,
            "server.json {} version",
            p["registryType"]
        );
        assert_eq!(p["identifier"], "diavlos");
    }

    let npm = json("packaging/npm/package.json");
    assert_eq!(
        npm["version"], version,
        "packaging/npm/package.json version"
    );
    assert_eq!(npm["mcpName"], NAME, "packaging/npm/package.json mcpName");
}

#[test]
fn crate_readme_names_the_listing() {
    // crates.io strips HTML comments, so the line has to be plain text.
    let readme = std::fs::read_to_string(repo().join("crates/cli/README.md")).unwrap();
    let line = format!("mcp-name: {NAME}");
    assert!(
        readme.lines().any(|l| l.trim() == line),
        "crates/cli/README.md needs the line `{line}`"
    );
    assert!(
        !readme.contains(&format!("<!-- {line}")),
        "the line must not be in a comment"
    );
}
