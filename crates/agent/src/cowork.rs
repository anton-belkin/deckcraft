//! Connect Cowork: adds DeckCraft to Claude Desktop's local MCP servers
//! (`~/Library/Application Support/Claude/claude_desktop_config.json`, `mcpServers.deckcraft`), as
//! `deckcraft --agent-stdio`. Other servers and settings are left exactly as they are; the file is
//! backed up first, and a file that isn't valid JSON is never overwritten.
//!
//! Claude Desktop reads the file when it starts and writes its in-memory copy back when it quits,
//! so the user quits it (⌘Q) and opens it again after this.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// The server's name in the config.
pub const NAME: &str = "deckcraft";

/// Where Claude Desktop keeps its config on this platform.
pub fn config_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/Claude/claude_desktop_config.json"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Claude").join("claude_desktop_config.json"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|c| c.join("Claude").join("claude_desktop_config.json"))
    }
}

/// The entry for this binary (`--agent-port` only when it isn't the default).
pub fn entry(exe: &str, port: u16) -> Value {
    let mut args = vec![json!("--agent-stdio")];
    if port != crate::server::DEFAULT_PORT {
        args.push(json!("--agent-port"));
        args.push(json!(port.to_string()));
    }
    json!({"command": exe, "args": args})
}

/// Is DeckCraft in the config, pointing at `exe`? (`Some(false)`: there, but another binary.)
pub fn configured(path: &Path, exe: &str) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let e = v.get("mcpServers")?.get(NAME)?;
    Some(e.get("command").and_then(Value::as_str) == Some(exe))
}

/// What a connect did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connected {
    pub path: PathBuf,
    pub backup: Option<PathBuf>,
    /// The entry was there already, the same.
    pub unchanged: bool,
}

/// Writes the `deckcraft` entry into the config at `path` (created when missing).
pub fn connect(path: &Path, exe: &str, port: u16) -> Result<Connected, String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("can't read {}: {e}", path.display())),
    };
    let mut root: Map<String, Value> = match &existing {
        None => Map::new(),
        Some(t) if t.trim().is_empty() => Map::new(),
        Some(t) => match serde_json::from_str::<Value>(t) {
            Ok(Value::Object(m)) => m,
            Ok(_) => return Err(format!("{} isn't a JSON object: left as it is", path.display())),
            Err(e) => return Err(format!("{} isn't valid JSON ({e}): left as it is", path.display())),
        },
    };
    let want = entry(exe, port);
    let servers = root.entry("mcpServers").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else { return Err("mcpServers in the config isn't an object: left as it is".into()) };
    if servers.get(NAME) == Some(&want) {
        return Ok(Connected { path: path.to_path_buf(), backup: None, unchanged: true });
    }
    servers.insert(NAME.into(), want);
    let backup = match &existing {
        Some(t) => {
            let b = path.with_file_name(format!("claude_desktop_config.json.bak-deckcraft-{}", crate::now_ms()));
            std::fs::write(&b, t).map_err(|e| format!("can't back up the config to {}: {e}", b.display()))?;
            Some(b)
        }
        None => None,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())?;
    // Write next to it, then rename: a crash halfway never leaves a broken config.
    let tmp = path.with_extension("json.deckcraft-tmp");
    std::fs::write(&tmp, text + "\n").map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("can't replace {}: {e}", path.display()))?;
    Ok(Connected { path: path.to_path_buf(), backup, unchanged: false })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("deckcraft-cowork-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn adds_deckcraft_keeping_every_other_server_and_setting() {
        let p = dir("keep").join("claude_desktop_config.json");
        std::fs::write(&p, r#"{"mcpServers":{"wordcraft":{"command":"/w","args":["x"]}},"preferences":{"a":1}}"#).unwrap();
        let c = connect(&p, "/Apps/deckcraft", 7984).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["wordcraft"]["command"], "/w");
        assert_eq!(v["preferences"]["a"], 1);
        assert_eq!(v["mcpServers"]["deckcraft"]["args"], json!(["--agent-stdio"]));
        assert!(std::fs::read_to_string(c.backup.unwrap()).unwrap().contains("wordcraft"));
        assert_eq!(configured(&p, "/Apps/deckcraft"), Some(true));
        assert!(connect(&p, "/Apps/deckcraft", 7984).unwrap().unchanged, "a second connect changes nothing");
        assert!(!connect(&p, "/Apps/deckcraft", 9000).unwrap().unchanged);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["deckcraft"]["args"], json!(["--agent-stdio", "--agent-port", "9000"]));
    }

    #[test]
    fn creates_a_missing_config_and_never_overwrites_a_broken_one() {
        let d = dir("new");
        let p = d.join("sub").join("claude_desktop_config.json");
        assert!(connect(&p, "/d", 7984).unwrap().backup.is_none());
        assert_eq!(configured(&p, "/d"), Some(true));
        let bad = d.join("broken.json");
        std::fs::write(&bad, "{ not json").unwrap();
        assert!(connect(&bad, "/d", 7984).is_err());
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{ not json");
    }
}
