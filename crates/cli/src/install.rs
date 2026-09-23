//! `diavlos mcp install --for <tool>`: write the MCP config for an agent
//! tool so it has the room tools.
//!
//! Config writing, not adapters. Every tool here already speaks MCP; all we
//! do is add one stdio server entry to the file it already reads, and leave
//! everything else in that file exactly as we found it. Some of these files
//! hold the user's credentials, so we merge, never rewrite, and we keep a
//! backup of the first version we touched.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

/// The name the server is registered under, in every tool.
pub const SERVER_NAME: &str = "diavlos";

/// Where and how one tool wants its MCP servers written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// A JSON file with a top-level `mcpServers` object.
    Json,
    /// A TOML file with an `[mcp_servers.<name>]` table.
    Toml,
}

/// One tool we know how to install into.
pub struct Tool {
    /// What the user types after `--for`.
    pub id: &'static str,
    /// What we call it in output.
    pub label: &'static str,
    /// The agent key it acts as unless `--as` says otherwise. Its own id,
    /// except for a tool that writes another tool's file: the program that
    /// reads the file is the one acting.
    key: &'static str,
    format: Format,
    /// `true` when the tool wants a `"type": "stdio"` field.
    typed: bool,
    /// Where the file lives, relative to the home directory.
    user_path: &'static [&'static str],
    /// Where the project-scoped file lives, relative to the working dir.
    project_path: Option<&'static [&'static str]>,
    /// Said after a successful write.
    pub note: Option<&'static str>,
}

/// Every tool `--for` accepts. `vibe-kanban` is here on purpose: it has no
/// config of its own, it edits the agents' files, which are already in this
/// list. Saying so is more use than silently doing nothing.
pub const TOOLS: &[Tool] = &[
    Tool {
        id: "claude-code",
        label: "Claude Code",
        key: "claude-code",
        format: Format::Json,
        typed: true,
        user_path: &[".claude.json"],
        project_path: Some(&[".mcp.json"]),
        note: None,
    },
    Tool {
        id: "codex",
        label: "Codex",
        key: "codex",
        format: Format::Toml,
        typed: false,
        user_path: &[".codex", "config.toml"],
        project_path: Some(&[".codex", "config.toml"]),
        note: None,
    },
    Tool {
        id: "cursor",
        label: "Cursor",
        key: "cursor",
        format: Format::Json,
        typed: true,
        user_path: &[".cursor", "mcp.json"],
        project_path: Some(&[".cursor", "mcp.json"]),
        note: Some("Cursor keeps its own enable list. If the tools do not appear, turn the server on in Settings, or run `agent mcp enable diavlos`."),
    },
    Tool {
        id: "gemini-cli",
        label: "Gemini CLI",
        key: "gemini-cli",
        format: Format::Json,
        typed: false,
        user_path: &[".gemini", "settings.json"],
        project_path: Some(&[".gemini", "settings.json"]),
        note: None,
    },
    Tool {
        id: "superset",
        label: "Superset",
        key: "superset",
        format: Format::Json,
        typed: true,
        // Superset has no MCP config of its own. Its built-in chat reads the
        // workspace file, and agents it launches read their own config.
        user_path: &[".mcp.json"],
        project_path: Some(&[".mcp.json"]),
        note: Some("Superset's chat reads the workspace file. Agents it launches read their own config, so install for those too."),
    },
    Tool {
        id: "vibe-kanban",
        label: "Vibe Kanban",
        key: "claude-code",
        format: Format::Json,
        typed: true,
        user_path: &[".claude.json"],
        project_path: None,
        note: Some("Vibe Kanban has no MCP config of its own: it writes the config of whichever agent it runs. Install for claude-code, codex and gemini-cli and it will see the server."),
    },
];

pub fn tool(id: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.id == id)
}

/// Every id, for an error message.
pub fn tool_ids() -> String {
    TOOLS.iter().map(|t| t.id).collect::<Vec<_>>().join(", ")
}

/// What one install did.
pub struct Outcome {
    pub tool: &'static str,
    /// The key the tool's MCP server will act as.
    pub identity: String,
    pub path: PathBuf,
    pub changed: bool,
    pub backup: Option<PathBuf>,
}

/// The `diavlos` binary to put in the config. An absolute path, because a
/// tool started from a desktop icon often has a different PATH than a
/// terminal does.
pub fn binary() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if exe
            .file_stem()
            .map(|s| s.to_string_lossy().starts_with("diavlos"))
            .unwrap_or(false)
        {
            return exe.to_string_lossy().into_owned();
        }
    }
    "diavlos".into()
}

/// The key a tool's MCP server acts as. Never `default`, which is the
/// person who installed Diavlos: an agent holding it could sign approvals
/// as them. Unless `--as` names another key, each tool gets its own, named
/// after it, so what Claude Code says is not mistaken for what Codex says.
/// Every session of one tool shares that tool's key.
pub fn agent_label<'a>(t: &'a Tool, identity: &'a str) -> &'a str {
    if identity.is_empty() || identity == "default" {
        t.key
    } else {
        identity
    }
}

/// Where this tool's entry would be written. Two tools can share a file:
/// Claude Code and Superset both read a project's `.mcp.json`.
pub fn target_path(t: &Tool, project: bool) -> Result<PathBuf> {
    path_for(t, project)
}

/// One key per file within one install. A file has one `diavlos` entry, so
/// two tools that share it must act as the same key; the first one listed
/// decides. Returns each target's key, and for a shared file the tool it
/// shares with.
pub fn plan_keys<'a>(
    targets: &[(&'a Tool, PathBuf)],
    identity: &str,
) -> Vec<(String, Option<&'a str>)> {
    let mut seen: Vec<(&PathBuf, String, &'a str)> = Vec::new();
    let mut out = Vec::new();
    for (t, path) in targets {
        if let Some((_, key, first)) = seen.iter().find(|(p, _, _)| *p == path) {
            out.push((key.clone(), Some(*first)));
        } else {
            let key = agent_label(t, identity).to_string();
            seen.push((path, key.clone(), t.label));
            out.push((key, None));
        }
    }
    out
}

/// The args the tool should run. `--as` is always there, so the entry
/// never falls back to the default key.
fn args(identity: &str) -> Vec<String> {
    vec!["--as".into(), identity.into(), "mcp".into()]
}

/// `DIAVLOS_HOME`, but only when the user has actually set one. We do not
/// bake in the default; that would freeze a path the user may move.
fn env_pairs(home: Option<&Path>) -> Vec<(String, String)> {
    match home {
        Some(h) => vec![("DIAVLOS_HOME".into(), h.to_string_lossy().into_owned())],
        None => vec![],
    }
}

/// The JSON entry a tool expects under `mcpServers.diavlos`.
fn json_entry(t: &Tool, identity: &str, home: Option<&Path>) -> Value {
    let mut entry = Map::new();
    if t.typed {
        entry.insert("type".into(), json!("stdio"));
    }
    entry.insert("command".into(), json!(binary()));
    entry.insert("args".into(), json!(args(agent_label(t, identity))));
    let env = env_pairs(home);
    if !env.is_empty() {
        let map: Map<String, Value> = env.into_iter().map(|(k, v)| (k, json!(v))).collect();
        entry.insert("env".into(), Value::Object(map));
    }
    Value::Object(entry)
}

/// Where this tool's file is for this scope.
fn path_for(t: &Tool, project: bool) -> Result<PathBuf> {
    if project {
        let rel = t.project_path.ok_or_else(|| {
            anyhow::anyhow!("{} has no project-scoped config; drop --project", t.label)
        })?;
        let mut p = std::env::current_dir()?;
        for part in rel {
            p.push(part);
        }
        return Ok(p);
    }
    let base = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("cannot find your home directory"))?;
    let mut p = base.home_dir().to_path_buf();
    for part in t.user_path {
        p.push(part);
    }
    Ok(p)
}

/// Write `text` to `path` without ever leaving a half-written file there:
/// a temp file beside it, then a rename.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("make {}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("diavlos-tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // These files hold tokens. Keep them to the owner.
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Copy the file aside once, before the first time we change it. Some of
/// these files hold the user's logins; a backup is cheap.
fn back_up(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let backup = path.with_extension(format!(
        "{}.diavlos-backup",
        path.extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    if backup.exists() {
        return Ok(Some(backup));
    }
    std::fs::copy(path, &backup).with_context(|| format!("back up {}", path.display()))?;
    Ok(Some(backup))
}

/// Install into one tool. Returns what happened; does not print.
pub fn install(
    t: &'static Tool,
    project: bool,
    identity: &str,
    home: Option<&Path>,
    dry_run: bool,
) -> Result<Outcome> {
    let path = path_for(t, project)?;
    let (text, changed) = match t.format {
        Format::Json => render_json(t, &path, identity, home)?,
        Format::Toml => render_toml(t, &path, identity, home)?,
    };
    if dry_run || !changed {
        return Ok(Outcome {
            tool: t.label,
            identity: agent_label(t, identity).to_string(),
            path,
            changed,
            backup: None,
        });
    }
    let backup = back_up(&path)?;
    write_atomic(&path, &text)?;
    Ok(Outcome {
        tool: t.label,
        identity: agent_label(t, identity).to_string(),
        path,
        changed,
        backup,
    })
}

/// The file's new contents, and whether anything actually changed.
fn render_json(
    t: &Tool,
    path: &Path,
    identity: &str,
    home: Option<&Path>,
) -> Result<(String, bool)> {
    let mut root: Value = if path.exists() {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        if raw.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&raw).with_context(|| {
                format!(
                    "{} is not valid JSON. Fix it or move it aside; we will not overwrite it.",
                    path.display()
                )
            })?
        }
    } else {
        json!({})
    };
    if !root.is_object() {
        anyhow::bail!("{} is not a JSON object", path.display());
    }
    let entry = json_entry(t, identity, home);
    let servers = root
        .as_object_mut()
        .expect("checked")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        anyhow::bail!("mcpServers in {} is not an object", path.display());
    }
    let servers = servers.as_object_mut().expect("checked");
    if servers.get(SERVER_NAME) == Some(&entry) {
        return Ok((String::new(), false));
    }
    servers.insert(SERVER_NAME.into(), entry);
    Ok((format!("{}\n", serde_json::to_string_pretty(&root)?), true))
}

/// Same for Codex's TOML. `toml_edit` keeps the user's comments and layout.
fn render_toml(
    t: &Tool,
    path: &Path,
    identity: &str,
    home: Option<&Path>,
) -> Result<(String, bool)> {
    use toml_edit::{Array, DocumentMut, Item, Table, Value as TVal};

    let mut doc: DocumentMut = if path.exists() {
        std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?
            .parse()
            .with_context(|| {
                format!(
                    "{} is not valid TOML. Fix it or move it aside; we will not overwrite it.",
                    path.display()
                )
            })?
    } else {
        DocumentMut::new()
    };

    let before = doc.to_string();

    let servers = doc.entry("mcp_servers").or_insert(Item::Table({
        let mut t = Table::new();
        t.set_implicit(true);
        t
    }));
    let servers = servers
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("mcp_servers in {} is not a table", path.display()))?;

    let mut entry = Table::new();
    entry["command"] = toml_edit::value(binary());
    let mut arr = Array::new();
    for a in args(agent_label(t, identity)) {
        arr.push(a);
    }
    entry["args"] = Item::Value(TVal::Array(arr));
    let env = env_pairs(home);
    if !env.is_empty() {
        let mut e = Table::new();
        for (k, v) in env {
            e[&k] = toml_edit::value(v);
        }
        entry["env"] = Item::Table(e);
    }
    servers[SERVER_NAME] = Item::Table(entry);

    let after = doc.to_string();
    Ok((after.clone(), after != before))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "diavlos-install-{}-{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn json_install_keeps_everything_else() {
        let dir = tmp("json");
        let path = dir.join("claude.json");
        // A file that already holds the user's account state.
        std::fs::write(
            &path,
            r#"{"oauthAccount":{"token":"secret"},"mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();
        let t = tool("claude-code").unwrap();
        let (text, changed) = render_json(t, &path, "default", None).unwrap();
        assert!(changed);
        let v: Value = serde_json::from_str(&text).unwrap();
        // The account is untouched and the other server survived.
        assert_eq!(v["oauthAccount"]["token"], "secret");
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        // Ours is there, with the fields Claude Code writes.
        assert_eq!(v["mcpServers"]["diavlos"]["type"], "stdio");
        assert_eq!(
            v["mcpServers"]["diavlos"]["args"],
            json!(["--as", "claude-code", "mcp"])
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn json_install_is_idempotent() {
        let dir = tmp("idem");
        let path = dir.join("mcp.json");
        let t = tool("cursor").unwrap();
        let (text, changed) = render_json(t, &path, "default", None).unwrap();
        assert!(changed);
        std::fs::write(&path, &text).unwrap();
        let (_, changed) = render_json(t, &path, "default", None).unwrap();
        assert!(!changed, "second install should be a no-op");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gemini_entry_has_no_type_field() {
        let t = tool("gemini-cli").unwrap();
        let e = json_entry(t, "default", None);
        assert!(e.get("type").is_none());
        assert_eq!(e["args"], json!(["--as", "gemini-cli", "mcp"]));
    }

    #[test]
    fn identity_and_home_reach_the_entry() {
        let t = tool("claude-code").unwrap();
        let e = json_entry(t, "fixer", Some(Path::new("/srv/dv")));
        assert_eq!(e["args"][0], "--as");
        assert_eq!(e["args"][1], "fixer");
        assert_eq!(e["args"][2], "mcp");
        assert_eq!(e["env"]["DIAVLOS_HOME"], "/srv/dv");
    }

    #[test]
    fn toml_install_keeps_comments_and_other_servers() {
        let dir = tmp("toml");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# my notes\nmodel = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
        )
        .unwrap();
        let (text, changed) = render_toml(tool("codex").unwrap(), &path, "default", None).unwrap();
        assert!(changed);
        assert!(text.contains("# my notes"), "comment must survive");
        assert!(text.contains("model = \"o3\""));
        assert!(text.contains("[mcp_servers.other]"));
        assert!(text.contains("[mcp_servers.diavlos]"));
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["diavlos"]["args"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()),
            Some(vec!["--as", "codex", "mcp"])
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn toml_install_is_idempotent() {
        let dir = tmp("toml2");
        let path = dir.join("config.toml");
        let (text, _) = render_toml(tool("codex").unwrap(), &path, "default", None).unwrap();
        std::fs::write(&path, &text).unwrap();
        let (_, changed) = render_toml(tool("codex").unwrap(), &path, "default", None).unwrap();
        assert!(!changed);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_broken_file_is_refused_not_overwritten() {
        let dir = tmp("broken");
        let path = dir.join("mcp.json");
        std::fs::write(&path, "{ not json").unwrap();
        let t = tool("cursor").unwrap();
        let err = render_json(t, &path, "default", None).unwrap_err();
        assert!(format!("{err:#}").contains("not valid JSON"));
        // Still there, untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_tool_id_resolves() {
        for t in TOOLS {
            assert!(tool(t.id).is_some());
        }
        assert!(tool("nope").is_none());
        assert!(tool_ids().contains("claude-code"));
    }

    /// The fix for an agent signing approvals with the installer's key: no
    /// tool's entry may run as `default`, whether `--as` was left out, left
    /// empty or given as `default`.
    #[test]
    fn no_tool_ever_runs_as_the_default_key() {
        for t in TOOLS {
            for given in ["default", ""] {
                let e = json_entry(t, given, None);
                assert_eq!(
                    e["args"],
                    json!(["--as", t.key, "mcp"]),
                    "{} with --as {given:?}",
                    t.id
                );
                assert_eq!(agent_label(t, given), t.key);
            }
            // The key must be one the helper accepts as a key name.
            assert_ne!(t.key, "default");
            diavlos_core::names::validate_name(t.key)
                .unwrap_or_else(|e| panic!("{} is not a valid key label: {e}", t.key));
        }
    }

    #[test]
    fn codex_toml_names_its_own_key() {
        let dir = tmp("toml-key");
        let path = dir.join("config.toml");
        let (text, _) = render_toml(tool("codex").unwrap(), &path, "default", None).unwrap();
        assert!(
            text.contains(r#"args = ["--as", "codex", "mcp"]"#),
            "got:\n{text}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tools_sharing_a_file_share_one_key() {
        let cc = tool("claude-code").unwrap();
        let ss = tool("superset").unwrap();
        let cx = tool("codex").unwrap();
        let shared = PathBuf::from("/p/.mcp.json");
        let plan = plan_keys(
            &[
                (cc, shared.clone()),
                (cx, PathBuf::from("/p/.codex/config.toml")),
                (ss, shared),
            ],
            "default",
        );
        assert_eq!(plan[0], ("claude-code".to_string(), None));
        assert_eq!(plan[1], ("codex".to_string(), None));
        assert_eq!(plan[2], ("claude-code".to_string(), Some("Claude Code")));
        // An explicit key is used for all of them.
        let plan = plan_keys(
            &[(cc, PathBuf::from("/a")), (cx, PathBuf::from("/b"))],
            "ops-bot",
        );
        assert!(plan.iter().all(|(k, _)| k == "ops-bot"));
    }

    #[test]
    fn vibe_kanban_acts_as_the_agent_that_reads_its_file() {
        // It writes Claude Code's file; Claude Code is what runs.
        assert_eq!(
            agent_label(tool("vibe-kanban").unwrap(), "default"),
            "claude-code"
        );
    }

    #[test]
    fn an_explicit_agent_key_is_kept() {
        let t = tool("cursor").unwrap();
        assert_eq!(agent_label(t, "reviewer"), "reviewer");
        assert_eq!(
            json_entry(t, "reviewer", None)["args"],
            json!(["--as", "reviewer", "mcp"])
        );
    }
}
