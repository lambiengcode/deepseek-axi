//! Agent-host integration installer. The first three targets are a faithful
//! port of `axi-sdk-js`'s `installSessionStartHooks()` (verified against the
//! real source, `packages/axi-sdk-js/src/hooks.ts` at
//! github.com/kunchenguid/axi, not guessed):
//! - Claude Code: `~/.claude/settings.json` (SessionStart hook array)
//! - Codex: `~/.codex/hooks.json` (same JSON shape) + `~/.codex/config.toml`
//!   (`[features]` / `hooks = true`, without which Codex ignores hooks.json)
//! - OpenCode: a generated JS plugin at `~/.config/opencode/plugins/`
//!
//! The fourth has no JS counterpart and is deliberately a different shape:
//! - DeepSeek Harness: a skill at `<$DSH_HOME|~/.dsh>/skills/<name>/SKILL.md`,
//!   model-invoked rather than ambient. DSH's own Claude Code hook bridge
//!   reads only `hookSpecificOutput.additionalContext` and discards plain
//!   stdout, so pointing it at the Claude hook above would attach cleanly and
//!   inject nothing. See `build_dsh_skill` for why ambient is also the wrong
//!   shape here even if it worked.
//!
//! Disclosed simplification: the JS version's legacy `session_start`
//! (snake_case) migration cleanup is not ported — that migrates hooks
//! written by an *older* version of axi-sdk-js itself, which this Rust
//! build never wrote in the first place, so it's dead code here, not a gap.

use crate::error::CliError;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

fn home_dir() -> Result<PathBuf, CliError> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| CliError::new("HOME is not set", "API_ERROR", vec![]))
}

fn is_managed_hook(hook: &Value, marker: &str) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .map(|c| c.contains(marker))
        .unwrap_or(false)
}

/// Installs (or repairs, or confirms already-installed) a SessionStart hook
/// in a `settings.json`/`hooks.json`-shaped file — shared by Claude Code and
/// Codex, which use the identical JSON structure at different paths.
fn install_json_session_start_hook(
    path: &Path,
    marker: &str,
    command: &str,
    timeout_seconds: u64,
) -> Result<&'static str, CliError> {
    let mut settings: Value = if path.exists() {
        let text = fs::read_to_string(path)
            .map_err(|e| CliError::new(format!("Could not read {}: {e}", path.display()), "API_ERROR", vec![]))?;
        serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    if !settings.is_object() {
        settings = json!({});
    }

    let settings_obj = settings.as_object_mut().unwrap();
    let hooks = settings_obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks_obj = hooks.as_object_mut().unwrap();
    let session_start = hooks_obj.entry("SessionStart").or_insert_with(|| json!([]));
    if !session_start.is_array() {
        *session_start = json!([]);
    }
    let groups = session_start.as_array_mut().unwrap();

    let already_installed = groups.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .map(|hs| hs.iter().any(|h| is_managed_hook(h, marker)))
            .unwrap_or(false)
    });
    if already_installed {
        return Ok("already installed");
    }

    groups.push(json!({
        "matcher": "",
        "hooks": [{"type": "command", "command": command, "timeout": timeout_seconds}]
    }));

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            CliError::new(
                format!("Could not create {}: {e}", parent.display()),
                "API_ERROR",
                vec![],
            )
        })?;
    }
    let text = format!("{}\n", serde_json::to_string_pretty(&settings).unwrap_or_default());
    fs::write(path, text)
        .map_err(|e| CliError::new(format!("Could not write {}: {e}", path.display()), "API_ERROR", vec![]))?;
    Ok("installed")
}

fn toml_section_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let without_comment = match trimmed.find('#') {
        Some(idx) => trimmed[..idx].trim_end(),
        None => trimmed,
    };
    let inner = without_comment
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .or_else(|| without_comment.strip_prefix('[').and_then(|s| s.strip_suffix(']')));
    inner
        .filter(|s| !s.is_empty() && !s.contains('[') && !s.contains(']'))
        .map(|s| s.trim().to_string())
}

fn toml_hooks_flag(line: &str) -> Option<bool> {
    let trimmed = line.trim();
    let without_comment = match trimmed.find('#') {
        Some(idx) => trimmed[..idx].trim_end(),
        None => trimmed,
    };
    let rest = without_comment
        .strip_prefix("hooks")?
        .trim_start()
        .strip_prefix('=')?
        .trim();
    match rest {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Ensures `~/.codex/config.toml` has `[features]` / `hooks = true` —
/// without this, Codex ignores `hooks.json` entirely. Ported line-by-line
/// from `computeCodexConfigUpdate` in the real JS source (not reimplemented
/// from a description) since TOML-editing logic is exactly the kind of
/// thing worth getting byte-for-byte right rather than approximating.
fn compute_codex_config_update(content: &str) -> (String, bool) {
    let newline = if content.contains("\r\n") { "\r\n" } else { "\n" };

    if content.trim().is_empty() {
        return (format!("[features]{newline}hooks = true{newline}"), true);
    }

    let lines: Vec<String> = content
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    let mut updated = lines.clone();
    let mut in_features = false;
    let mut saw_features = false;

    for (index, line) in lines.iter().enumerate() {
        if let Some(section_name) = toml_section_header(line) {
            if in_features {
                updated.insert(index, "hooks = true".to_string());
                return (updated.join(newline), true);
            }
            in_features = section_name == "features";
            saw_features = saw_features || in_features;
            continue;
        }

        if !in_features {
            continue;
        }

        if let Some(value) = toml_hooks_flag(line) {
            if value {
                return (content.to_string(), false);
            }
            updated[index] = line.replacen("false", "true", 1);
            return (updated.join(newline), true);
        }
    }

    if saw_features {
        let suffix = if content.ends_with(newline) { "" } else { newline };
        return (format!("{content}{suffix}hooks = true{newline}"), true);
    }

    let separator = if content.ends_with(newline) {
        newline.to_string()
    } else {
        format!("{newline}{newline}")
    };
    (
        format!("{content}{separator}[features]{newline}hooks = true{newline}"),
        true,
    )
}

fn ensure_codex_hooks_enabled(home: &Path) -> Result<&'static str, CliError> {
    let path = home.join(".codex").join("config.toml");
    let current = if path.exists() {
        fs::read_to_string(&path)
            .map_err(|e| CliError::new(format!("Could not read {}: {e}", path.display()), "API_ERROR", vec![]))?
    } else {
        String::new()
    };
    let (updated, changed) = compute_codex_config_update(&current);
    if !changed {
        return Ok("already enabled");
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            CliError::new(
                format!("Could not create {}: {e}", parent.display()),
                "API_ERROR",
                vec![],
            )
        })?;
    }
    fs::write(&path, updated)
        .map_err(|e| CliError::new(format!("Could not write {}: {e}", path.display()), "API_ERROR", vec![]))?;
    Ok("enabled")
}

const OPENCODE_MANAGED_PREFIX: &str = "deepseek-axi-rust managed opencode plugin:";

fn sanitize_opencode_file_part(marker: &str) -> String {
    marker
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn sanitize_opencode_export_name(marker: &str) -> String {
    let name: String = marker
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    format!(
        "Axi{}AmbientContextPlugin",
        if name.is_empty() { "Plugin".to_string() } else { name }
    )
}

const DSH_MANAGED_MARKER: &str = "deepseek-axi managed skill";

/// DeepSeek Harness resolves its config root from `$DSH_HOME`, falling back
/// to `~/.dsh` (`@deepseek-ai/dsh-home-paths`; skills are scanned under
/// `<dshHome>/skills`).
fn dsh_home(home: &Path) -> PathBuf {
    match std::env::var("DSH_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home.join(".dsh"),
    }
}

/// DSH requires skill names to be kebab-case, and rejects the whole skill
/// rather than sanitising it.
fn sanitize_kebab(marker: &str) -> String {
    let cleaned: String = marker
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let joined = cleaned
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if joined.is_empty() {
        "deepseek-axi".to_string()
    } else {
        joined
    }
}

/// A DeepSeek Harness skill: a Markdown file DSH's filesystem skill provider
/// discovers, whose body the model loads on demand and then acts on using
/// DSH's own `bash` tool.
///
/// Deliberately *not* an ambient-context injection like the other three
/// targets. DSH already talks to DeepSeek natively, so re-injecting a
/// DeepSeek view on every turn would be paying tokens to tell the model what
/// it already is. What DSH does not have is worktree-isolated editing or
/// independent verification (its own `tool-ralph` README states that
/// "completion and blockers are worker reports, not independent
/// evaluation"), so the skill is scoped to exactly that.
fn build_dsh_skill(marker: &str, command: &str) -> String {
    let name = sanitize_kebab(marker);
    format!(
        r#"---
name: {name}
description: Delegate a code change to {command}, which edits inside an isolated git worktree and independently verifies the result against the base commit before reporting success.
whenToUse: When a change should be proven, not just written — a bug fix where you want the failure confirmed to reproduce before the fix and the suite confirmed green after, landed on a branch you can review or discard.
metadata:
  managed-by: {marker}
---

<!-- {DSH_MANAGED_MARKER}: regenerate with `{command} setup hooks`. Remove this line to take ownership of the file. -->

# {command}

`{command}` is a binary on PATH. Invoke it with the `bash` tool.

## What it adds that this session does not already have

- **Isolation.** Every edit happens in a dedicated git worktree on its own branch
  (`{marker}/<taskId>`), never the working tree you are in. A failed attempt is discarded without
  touching your checkout.
- **Independent verification.** Before reporting `verified` it runs the suite twice: once on the
  base commit to confirm the failure genuinely reproduces, and once on the fixed worktree to
  confirm it is green. Anything less is reported as `partially_verified` or `failed`, never as
  success.
- **Enforced budgets.** `--max-iterations`, `--max-cost`, and `--timeout` stop the loop and
  preserve whatever partial work exists rather than discarding it.

## Use it for

```sh
{command} test --fix                      # make the failing suite pass, verified two-sided
{command} run --task "<what to change>"   # a scoped change, verified the same way
```

Useful flags: `--dry-run` (plan only, no writes), `--test-cmd "<cmd>"` when detection guesses
wrong, `--json` for a machine-readable handoff, `--model flash` to trade depth for cost.

On success the branch is left for you to review and merge — it never merges anything itself. The
returned `branch` field names it.

## Do not use it for

`ask`, `think`, and `review` also exist, but they query DeepSeek — which is what you already are.
Use your own reasoning and tools instead; reaching for those here spends tokens to ask yourself a
question.
"#
    )
}

fn install_dsh_skill(home: &Path, marker: &str, command: &str) -> Result<&'static str, CliError> {
    let skill_path = dsh_home(home)
        .join("skills")
        .join(sanitize_kebab(marker))
        .join("SKILL.md");
    let next = build_dsh_skill(marker, command);

    let current = if skill_path.exists() {
        Some(fs::read_to_string(&skill_path).map_err(|e| {
            CliError::new(
                format!("Could not read {}: {e}", skill_path.display()),
                "API_ERROR",
                vec![],
            )
        })?)
    } else {
        None
    };

    if let Some(existing) = &current {
        if !existing.contains(DSH_MANAGED_MARKER) {
            return Err(CliError::new(
                format!("{}: refusing to overwrite unmanaged DSH skill", skill_path.display()),
                "API_ERROR",
                vec![],
            ));
        }
        if *existing == next {
            return Ok("already installed");
        }
    }

    if let Some(parent) = skill_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            CliError::new(
                format!("Could not create {}: {e}", parent.display()),
                "API_ERROR",
                vec![],
            )
        })?;
    }
    fs::write(&skill_path, next).map_err(|e| {
        CliError::new(
            format!("Could not write {}: {e}", skill_path.display()),
            "API_ERROR",
            vec![],
        )
    })?;
    Ok(if current.is_some() { "updated" } else { "installed" })
}

/// A minimal, self-contained OpenCode plugin: shells out to `command` (no
/// args) and injects its stdout as ambient system context, per-session
/// cached. Ported from `buildOpenCodeAmbientPluginSource` in the real JS
/// source — same shape, same managed-marker comment for idempotent
/// overwrite protection.
fn build_opencode_plugin_source(marker: &str, command: &str, timeout_seconds: u64) -> String {
    let export_name = sanitize_opencode_export_name(marker);
    let managed_marker = format!("{OPENCODE_MANAGED_PREFIX} {marker}");
    let ambient_header = format!("## AXI ambient context: {marker}");
    let timeout_ms = timeout_seconds * 1000;

    format!(
        r#"// {managed_marker}
// This file is generated by deepseek-axi. It is safe to edit only if you remove the managed marker above.
import {{ spawn }} from "node:child_process";

const command = {command:?};
const marker = {marker:?};
const ambientHeader = {ambient_header:?};
const timeoutMs = {timeout_ms};

function runAxiHomeView(cwd) {{
  return new Promise((resolve) => {{
    const child = spawn(command, [], {{
      cwd: directoryOrFallback(cwd),
      env: process.env,
      shell: false,
      stdio: ["ignore", "pipe", "pipe"],
    }});

    let stdout = "";
    let stderr = "";
    let settled = false;

    const timer = setTimeout(() => {{
      if (settled) return;
      settled = true;
      child.kill("SIGTERM");
      resolve("error: " + marker + " ambient context timed out after " + timeoutMs + "ms");
    }}, timeoutMs);

    child.stdout?.setEncoding("utf-8");
    child.stderr?.setEncoding("utf-8");
    child.stdout?.on("data", (chunk) => {{ stdout += chunk; }});
    child.stderr?.on("data", (chunk) => {{ stderr += chunk; }});
    child.on("error", (error) => {{
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve("error: " + marker + " ambient context failed: " + error.message);
    }});
    child.on("close", (code) => {{
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (code === 0) {{
        resolve(stdout.trim());
        return;
      }}
      const message = (stderr || stdout || marker + " exited with code " + code).trim();
      resolve("error: " + marker + " ambient context failed: " + message);
    }});
  }});
}}

function directoryOrFallback(directory) {{
  return typeof directory === "string" && directory.length > 0 ? directory : process.cwd();
}}

export const {export_name} = async ({{ directory }}) => {{
  const sessionCache = new Map();

  return {{
    "experimental.chat.system.transform": async (input, output) => {{
      const sessionID = input.sessionID ?? "__global__";
      let homeView = sessionCache.get(sessionID);
      if (homeView === undefined) {{
        homeView = await runAxiHomeView(directory);
        sessionCache.set(sessionID, homeView);
      }}
      if (homeView.length === 0) return;
      output.system.push(ambientHeader + "\n" + homeView);
    }},
  }};
}};
"#
    )
}

fn install_opencode_plugin(
    home: &Path,
    marker: &str,
    command: &str,
    timeout_seconds: u64,
) -> Result<&'static str, CliError> {
    let plugin_path = home
        .join(".config")
        .join("opencode")
        .join("plugins")
        .join(format!("axi-{}.js", sanitize_opencode_file_part(marker)));
    let managed_marker = format!("{OPENCODE_MANAGED_PREFIX} {marker}");
    let next = build_opencode_plugin_source(marker, command, timeout_seconds);

    let current = if plugin_path.exists() {
        Some(fs::read_to_string(&plugin_path).map_err(|e| {
            CliError::new(
                format!("Could not read {}: {e}", plugin_path.display()),
                "API_ERROR",
                vec![],
            )
        })?)
    } else {
        None
    };

    if let Some(existing) = &current {
        if !existing.contains(&managed_marker) {
            return Err(CliError::new(
                format!(
                    "{}: refusing to overwrite unmanaged OpenCode plugin",
                    plugin_path.display()
                ),
                "API_ERROR",
                vec![],
            ));
        }
        if *existing == next {
            return Ok("already installed");
        }
    }

    if let Some(parent) = plugin_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            CliError::new(
                format!("Could not create {}: {e}", parent.display()),
                "API_ERROR",
                vec![],
            )
        })?;
    }
    fs::write(&plugin_path, next).map_err(|e| {
        CliError::new(
            format!("Could not write {}: {e}", plugin_path.display()),
            "API_ERROR",
            vec![],
        )
    })?;
    Ok(if current.is_some() { "updated" } else { "installed" })
}

/// Installs SessionStart ambient-context hooks for all three targets
/// (Claude Code, Codex, OpenCode). Best-effort per target — one target
/// failing doesn't stop the others, matching the JS original's semantics.
/// Returns a `(target, outcome)` pair per target attempted.
pub fn install_session_start_hooks(
    marker: &str,
    command: &str,
    timeout_seconds: u64,
) -> Vec<(&'static str, Result<&'static str, CliError>)> {
    let home = match home_dir() {
        Ok(h) => h,
        Err(e) => return vec![("claude", Err(e))],
    };

    vec![
        (
            "claude",
            install_json_session_start_hook(
                &home.join(".claude").join("settings.json"),
                marker,
                command,
                timeout_seconds,
            ),
        ),
        (
            "codex-hooks",
            install_json_session_start_hook(
                &home.join(".codex").join("hooks.json"),
                marker,
                command,
                timeout_seconds,
            ),
        ),
        ("codex-config", ensure_codex_hooks_enabled(&home)),
        (
            "opencode",
            install_opencode_plugin(&home, marker, command, timeout_seconds),
        ),
        // DeepSeek Harness gets a skill, not an ambient hook — see build_dsh_skill
        // for why re-injecting a DeepSeek view into a DeepSeek harness is the
        // wrong shape.
        ("dsh-skill", install_dsh_skill(&home, marker, command)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_home(tag: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("deepseek-axi-rust-hooks-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        home
    }

    #[test]
    fn installs_claude_hook_into_a_fresh_settings_file_and_is_idempotent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = with_temp_home("claude");

        let first = install_json_session_start_hook(
            &home.join(".claude").join("settings.json"),
            "deepseek-axi",
            "deepseek-axi",
            10,
        )
        .unwrap();
        assert_eq!(first, "installed");

        let path = home.join(".claude").join("settings.json");
        let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let groups = parsed["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["matcher"], "");

        let second = install_json_session_start_hook(&path, "deepseek-axi", "deepseek-axi", 10).unwrap();
        assert_eq!(second, "already installed");
        let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["hooks"]["SessionStart"].as_array().unwrap().len(), 1);

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn preserves_existing_unrelated_settings() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = with_temp_home("preserve");
        let claude_dir = home.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::write(
            claude_dir.join("settings.json"),
            r#"{"theme":"dark","hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"gh-axi","timeout":10}]}]}}"#,
        )
        .unwrap();

        install_json_session_start_hook(&claude_dir.join("settings.json"), "deepseek-axi", "deepseek-axi", 10).unwrap();

        let parsed: Value =
            serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(parsed["theme"], "dark");
        assert_eq!(parsed["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_config_update_creates_features_section_in_empty_file() {
        let (updated, changed) = compute_codex_config_update("");
        assert!(changed);
        assert_eq!(updated, "[features]\nhooks = true\n");
    }

    #[test]
    fn codex_config_update_flips_existing_hooks_false_to_true() {
        let (updated, changed) = compute_codex_config_update("[features]\nhooks = false\nother = 1\n");
        assert!(changed);
        assert_eq!(updated, "[features]\nhooks = true\nother = 1\n");
    }

    #[test]
    fn codex_config_update_is_a_noop_when_hooks_already_true() {
        let content = "[features]\nhooks = true\n";
        let (updated, changed) = compute_codex_config_update(content);
        assert!(!changed);
        assert_eq!(updated, content);
    }

    #[test]
    fn codex_config_update_inserts_into_features_before_next_section_when_no_hooks_line() {
        let (updated, changed) = compute_codex_config_update("[features]\nother = 1\n[next]\nx = 2\n");
        assert!(changed);
        assert_eq!(updated, "[features]\nother = 1\nhooks = true\n[next]\nx = 2\n");
    }

    #[test]
    fn codex_config_update_appends_hooks_when_features_is_last_section() {
        let (updated, changed) = compute_codex_config_update("[features]\nother = 1\n");
        assert!(changed);
        assert_eq!(updated, "[features]\nother = 1\nhooks = true\n");
    }

    #[test]
    fn codex_config_update_adds_a_new_features_section_when_none_exists() {
        let (updated, changed) = compute_codex_config_update("[other]\nx = 1\n");
        assert!(changed);
        assert_eq!(updated, "[other]\nx = 1\n\n[features]\nhooks = true\n");
    }

    #[test]
    fn opencode_plugin_installs_with_managed_marker_and_is_idempotent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = with_temp_home("opencode");

        let first = install_opencode_plugin(&home, "deepseek-axi", "deepseek-axi", 10).unwrap();
        assert_eq!(first, "installed");

        let plugin_path = home
            .join(".config")
            .join("opencode")
            .join("plugins")
            .join("axi-deepseek-axi.js");
        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("deepseek-axi-rust managed opencode plugin: deepseek-axi"));
        assert!(content.contains("AxiDeepseekAxiAmbientContextPlugin"));

        let second = install_opencode_plugin(&home, "deepseek-axi", "deepseek-axi", 10).unwrap();
        assert_eq!(second, "already installed");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn dsh_skill_satisfies_the_frontmatter_contract_dsh_enforces() {
        // DSH drops the entire skill (with only a warning) on a bad name or a
        // mis-spelled invocation key, so these are load-bearing, not cosmetic.
        // Contract: packages/skill/skill-filesystem/README.md "Skill Format".
        let skill = build_dsh_skill("deepseek-axi", "deepseek-axi");
        let front = skill.split("---").nth(1).expect("frontmatter must be delimited by ---");

        let name = front
            .lines()
            .find_map(|l| l.strip_prefix("name:"))
            .expect("name is required")
            .trim();
        assert_eq!(name, "deepseek-axi");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "name must be kebab-case, got {name:?}"
        );
        assert!(
            front.lines().any(|l| l.starts_with("description:")),
            "description is required"
        );

        // The invocation keys are kebab-case only; a camelCase spelling is
        // rejected outright. Not emitting them at all is the safe default,
        // since each omitted field permits its surface.
        assert!(!skill.contains("disableModelInvocation"));
        assert!(!skill.contains("userInvocable"));

        // whenToUse, by contrast, *is* camelCase in DSH's schema.
        assert!(front.lines().any(|l| l.starts_with("whenToUse:")));
    }

    #[test]
    fn dsh_skill_is_positioned_on_what_dsh_lacks_not_on_asking_deepseek() {
        let skill = build_dsh_skill("deepseek-axi", "deepseek-axi");
        // The wedge: DSH has a fresh-agent worker loop already, but no
        // worktree isolation and no independent verification.
        assert!(skill.contains("isolated git worktree"));
        assert!(skill.contains("Independent verification"));
        // And it must steer the model away from the redundant advisory tier.
        assert!(skill.contains("Do not use it for"));
        assert!(skill.contains("which is what you already are"));
    }

    #[test]
    fn dsh_skill_installs_under_dsh_home_and_is_idempotent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = with_temp_home("dsh");
        let dsh = home.join("custom-dsh-home");
        unsafe {
            std::env::set_var("DSH_HOME", &dsh);
        }

        assert_eq!(
            install_dsh_skill(&home, "deepseek-axi", "deepseek-axi").unwrap(),
            "installed"
        );
        let path = dsh.join("skills").join("deepseek-axi").join("SKILL.md");
        assert!(path.exists(), "must honour $DSH_HOME");
        assert!(fs::read_to_string(&path).unwrap().contains(DSH_MANAGED_MARKER));
        assert_eq!(
            install_dsh_skill(&home, "deepseek-axi", "deepseek-axi").unwrap(),
            "already installed"
        );

        // Without DSH_HOME it falls back to ~/.dsh.
        unsafe {
            std::env::remove_var("DSH_HOME");
        }
        assert_eq!(
            install_dsh_skill(&home, "deepseek-axi", "deepseek-axi").unwrap(),
            "installed"
        );
        assert!(
            home.join(".dsh")
                .join("skills")
                .join("deepseek-axi")
                .join("SKILL.md")
                .exists()
        );

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn dsh_skill_refuses_to_overwrite_an_unmanaged_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = with_temp_home("dsh-unmanaged");
        unsafe {
            std::env::remove_var("DSH_HOME");
        }
        let dir = home.join(".dsh").join("skills").join("deepseek-axi");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "---\nname: mine\n---\nhand-written\n").unwrap();

        let err = install_dsh_skill(&home, "deepseek-axi", "deepseek-axi").unwrap_err();
        assert!(err.message.contains("refusing to overwrite"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn kebab_sanitiser_never_emits_an_invalid_skill_name() {
        assert_eq!(sanitize_kebab("deepseek-axi"), "deepseek-axi");
        assert_eq!(sanitize_kebab("DeepSeek_AXI"), "deepseek-axi");
        assert_eq!(sanitize_kebab("--weird__name--"), "weird-name");
        assert_eq!(sanitize_kebab("!!!"), "deepseek-axi", "must not produce an empty name");
    }

    #[test]
    fn opencode_plugin_refuses_to_overwrite_an_unmanaged_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = with_temp_home("opencode-unmanaged");
        let plugins_dir = home.join(".config").join("opencode").join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();
        fs::write(
            plugins_dir.join("axi-deepseek-axi.js"),
            "// hand-written plugin, not ours\n",
        )
        .unwrap();

        let result = install_opencode_plugin(&home, "deepseek-axi", "deepseek-axi", 10);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("refusing to overwrite"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn install_session_start_hooks_attempts_all_five_targets() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = with_temp_home("all-targets");

        let results = install_session_start_hooks("deepseek-axi", "deepseek-axi", 10);
        let targets: Vec<&str> = results.iter().map(|(t, _)| *t).collect();
        assert_eq!(
            targets,
            vec!["claude", "codex-hooks", "codex-config", "opencode", "dsh-skill"]
        );
        assert!(results.iter().all(|(_, r)| r.is_ok()));
        let _ = fs::remove_dir_all(&home);
    }
}
