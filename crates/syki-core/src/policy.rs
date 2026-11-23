//! GUI-independent security policy engine and SHA-256 plugin trust registry.
//!
//! Centralizes trust and execution decisions for plugin loading/reloading,
//! external program execution, PTY creation, and RPC-exposed plugin commands,
//! while recording every evaluation into a bounded in-memory audit ring buffer.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum number of recent [`PolicyDecision`] entries retained in memory.
pub const AUDIT_LOG_CAPACITY: usize = 128;

/// Shell / script-host interpreters denied by default unless explicitly approved.
const DANGEROUS_SHELL_INTERPRETERS: &[&str] = &[
    "cmd",
    "cmd.exe",
    concat!("power", "shell"),
    concat!("power", "shell.exe"),
    "pwsh",
    "pwsh.exe",
    "wscript",
    "wscript.exe",
    "cscript",
    "cscript.exe",
    "mshta",
    "mshta.exe",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyVerdict {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDecision {
    pub verdict: PolicyVerdict,
    pub rule_id: String,
    pub reason: String,
    pub subject: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrustRegistry {
    /// plugin_id -> trusted SHA-256 hex digest
    pub plugins: BTreeMap<String, String>,
    /// plugin_id -> dev mode opt-in (allows reload without re-prompt ONLY after initial trust)
    pub dev_mode_plugins: BTreeSet<String>,
    /// approved program names (basename lowercase, e.g. "git", "python", "pandoc")
    pub allowed_programs: BTreeSet<String>,
    /// approved pty open flag
    pub allow_pty: bool,
}

pub struct PolicyDecisionEngine {
    registry: TrustRegistry,
    audit_log: VecDeque<PolicyDecision>,
}

impl PolicyDecisionEngine {
    pub fn new(registry: TrustRegistry) -> Self {
        Self {
            registry,
            audit_log: VecDeque::with_capacity(AUDIT_LOG_CAPACITY),
        }
    }

    pub fn sha256_hex(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{byte:02x}");
        }
        hex
    }

    pub fn evaluate_plugin_load(
        &mut self,
        plugin_id: &str,
        content_bytes: &[u8],
    ) -> PolicyDecision {
        let id = plugin_id.trim();
        if id.is_empty() {
            return self.record(
                PolicyVerdict::Deny,
                "plugin.load.invalid_id",
                "plugin_id must not be empty",
                plugin_id,
            );
        }

        let actual_hash = Self::sha256_hex(content_bytes);
        match self.registry.plugins.get(id) {
            None => self.record(
                PolicyVerdict::Ask,
                "plugin.load.untrusted",
                format!("untrusted plugin requires initial approval (sha256={actual_hash})"),
                id,
            ),
            Some(trusted_hash) if trusted_hash.eq_ignore_ascii_case(&actual_hash) => self.record(
                PolicyVerdict::Allow,
                "plugin.load.trusted",
                "plugin SHA-256 matches trusted registry entry",
                id,
            ),
            Some(_) if self.registry.dev_mode_plugins.contains(id) => self.record(
                PolicyVerdict::Allow,
                "plugin.load.dev_mode_hash_changed",
                format!(
                    "initially trusted plugin changed hash under dev mode (sha256={actual_hash})"
                ),
                id,
            ),
            Some(trusted_hash) => self.record(
                PolicyVerdict::Ask,
                "plugin.load.hash_mismatch",
                format!(
                    "plugin SHA-256 changed from {trusted_hash} to {actual_hash}; re-approval required"
                ),
                id,
            ),
        }
    }

    pub fn evaluate_plugin_reload(
        &mut self,
        plugin_id: &str,
        content_bytes: &[u8],
    ) -> PolicyDecision {
        let id = plugin_id.trim();
        if id.is_empty() {
            return self.record(
                PolicyVerdict::Deny,
                "plugin.reload.invalid_id",
                "plugin_id must not be empty",
                plugin_id,
            );
        }

        let actual_hash = Self::sha256_hex(content_bytes);
        match self.registry.plugins.get(id) {
            None => self.record(
                PolicyVerdict::Deny,
                "plugin.reload.untrusted_denied",
                "untrusted plugins cannot be auto-enabled via plugin.reload",
                id,
            ),
            Some(trusted_hash) if trusted_hash.eq_ignore_ascii_case(&actual_hash) => self.record(
                PolicyVerdict::Allow,
                "plugin.reload.trusted",
                "reloaded plugin SHA-256 matches trusted registry entry",
                id,
            ),
            Some(_) if self.registry.dev_mode_plugins.contains(id) => self.record(
                PolicyVerdict::Allow,
                "plugin.reload.dev_mode_allowed",
                format!(
                    "reloaded plugin hash changed under dev mode for initially trusted plugin (sha256={actual_hash})"
                ),
                id,
            ),
            Some(trusted_hash) => self.record(
                PolicyVerdict::Ask,
                "plugin.reload.hash_changed",
                format!(
                    "reloaded plugin SHA-256 changed from {trusted_hash} to {actual_hash}; re-approval required"
                ),
                id,
            ),
        }
    }

    pub fn evaluate_program_exec(&mut self, program: &str, args: &[String]) -> PolicyDecision {
        let trimmed = program.trim();
        if trimmed.is_empty() || trimmed.contains('\0') {
            return self.record(
                PolicyVerdict::Deny,
                "exec.invalid_program",
                "program path is empty or contains NUL byte",
                program,
            );
        }

        let has_path_sep = trimmed.contains('/') || trimmed.contains('\\');
        let normalized_full = trimmed.to_ascii_lowercase();
        let basename_lower = extract_basename(trimmed).to_ascii_lowercase();

        if basename_lower.is_empty() {
            return self.record(
                PolicyVerdict::Deny,
                "exec.invalid_program",
                "program path has no executable basename",
                trimmed,
            );
        }

        let is_dangerous_shell = DANGEROUS_SHELL_INTERPRETERS.contains(&basename_lower.as_str());

        if has_path_sep {
            if self.registry.allowed_programs.contains(&normalized_full) {
                return self.record(
                    PolicyVerdict::Allow,
                    "exec.allowed_path",
                    format!(
                        "explicitly approved executable path ({} args)",
                        args.len()
                    ),
                    trimmed,
                );
            }
            if is_dangerous_shell {
                return self.record(
                    PolicyVerdict::Deny,
                    "exec.denied_shell",
                    format!(
                        "dangerous shell interpreter '{basename_lower}' is denied by default"
                    ),
                    trimmed,
                );
            }
            return self.record(
                PolicyVerdict::Ask,
                "exec.unapproved_path",
                format!(
                    "path-qualified executable is not in allowlist ({} args)",
                    args.len()
                ),
                trimmed,
            );
        }

        if is_dangerous_shell {
            if self.registry.allowed_programs.contains(&basename_lower) {
                return self.record(
                    PolicyVerdict::Allow,
                    "exec.allowed_shell",
                    format!(
                        "explicitly approved shell interpreter '{basename_lower}' ({} args)",
                        args.len()
                    ),
                    trimmed,
                );
            }
            return self.record(
                PolicyVerdict::Deny,
                "exec.denied_shell",
                format!("dangerous shell interpreter '{basename_lower}' is denied by default"),
                trimmed,
            );
        }

        if self.is_bare_program_allowed(&basename_lower) {
            return self.record(
                PolicyVerdict::Allow,
                "exec.allowed",
                format!("program is in allowlist ({} args)", args.len()),
                trimmed,
            );
        }

        self.record(
            PolicyVerdict::Ask,
            "exec.unapproved_program",
            format!("program '{basename_lower}' is not in allowlist ({} args)", args.len()),
            trimmed,
        )
    }

    pub fn evaluate_pty_open(&mut self) -> PolicyDecision {
        if self.registry.allow_pty {
            self.record(
                PolicyVerdict::Allow,
                "pty.open.allowed",
                "PTY creation is approved in trust registry",
                "pty",
            )
        } else {
            self.record(
                PolicyVerdict::Ask,
                "pty.open.requires_approval",
                "PTY creation requires explicit user approval",
                "pty",
            )
        }
    }

    pub fn evaluate_rpc_plugin_run(
        &mut self,
        plugin_id: &str,
        command_id: &str,
        expose_rpc: bool,
    ) -> PolicyDecision {
        let id = plugin_id.trim();
        let cmd = command_id.trim();
        let subject = format!("{id}:{cmd}");

        if !expose_rpc {
            return self.record(
                PolicyVerdict::Deny,
                "rpc.plugin_run.not_exposed",
                "plugin command does not opt in to expose_rpc",
                subject,
            );
        }

        if id.is_empty() || cmd.is_empty() {
            return self.record(
                PolicyVerdict::Deny,
                "rpc.plugin_run.invalid_target",
                "plugin_id and command_id must not be empty",
                subject,
            );
        }

        if !self.registry.plugins.contains_key(id) {
            return self.record(
                PolicyVerdict::Deny,
                "rpc.plugin_run.untrusted_plugin",
                "untrusted plugin cannot be invoked via RPC",
                subject,
            );
        }

        self.record(
            PolicyVerdict::Allow,
            "rpc.plugin_run.allowed",
            "trusted plugin command explicitly opted in to expose_rpc",
            subject,
        )
    }

    pub fn approve_plugin(&mut self, plugin_id: &str, sha256_hex: &str) {
        let id = plugin_id.trim();
        let hash = sha256_hex.trim().to_ascii_lowercase();
        if id.is_empty() || hash.is_empty() {
            return;
        }
        self.registry.plugins.insert(id.to_string(), hash);
    }

    /// Enables or disables dev mode for a plugin.
    /// Returns `false` if the plugin has not been initially trusted.
    pub fn set_dev_mode(&mut self, plugin_id: &str, enabled: bool) -> bool {
        let id = plugin_id.trim();
        if !self.registry.plugins.contains_key(id) {
            self.registry.dev_mode_plugins.remove(id);
            return false;
        }
        if enabled {
            self.registry.dev_mode_plugins.insert(id.to_string());
        } else {
            self.registry.dev_mode_plugins.remove(id);
        }
        true
    }

    pub fn approve_program(&mut self, program: &str) {
        let trimmed = program.trim();
        if trimmed.is_empty() || trimmed.contains('\0') {
            return;
        }
        self.registry
            .allowed_programs
            .insert(trimmed.to_ascii_lowercase());
    }

    pub fn audit_log(&self) -> Vec<PolicyDecision> {
        self.audit_log.iter().cloned().collect()
    }

    pub fn registry(&self) -> &TrustRegistry {
        &self.registry
    }

    fn is_bare_program_allowed(&self, basename_lower: &str) -> bool {
        if self.registry.allowed_programs.contains(basename_lower) {
            return true;
        }
        if let Some(stem) = basename_lower.strip_suffix(".exe") {
            if self.registry.allowed_programs.contains(stem) {
                return true;
            }
        }
        false
    }

    fn record(
        &mut self,
        verdict: PolicyVerdict,
        rule_id: &str,
        reason: impl Into<String>,
        subject: impl Into<String>,
    ) -> PolicyDecision {
        let decision = PolicyDecision {
            verdict,
            rule_id: rule_id.to_string(),
            reason: reason.into(),
            subject: subject.into(),
            timestamp_ms: now_epoch_ms(),
        };
        if self.audit_log.len() >= AUDIT_LOG_CAPACITY {
            self.audit_log.pop_front();
        }
        self.audit_log.push_back(decision.clone());
        decision
    }
}

fn extract_basename(program: &str) -> &str {
    program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            PolicyDecisionEngine::sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            PolicyDecisionEngine::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn untrusted_plugin_asks_on_load_and_denies_on_reload() {
        let mut engine = PolicyDecisionEngine::new(TrustRegistry::default());
        let code = b"console.log('hello');";

        let load = engine.evaluate_plugin_load("demo.plugin", code);
        assert_eq!(load.verdict, PolicyVerdict::Ask);

        let reload = engine.evaluate_plugin_reload("demo.plugin", code);
        assert_eq!(reload.verdict, PolicyVerdict::Deny);

        // Dev mode cannot be enabled before initial trust.
        assert!(!engine.set_dev_mode("demo.plugin", true));
        assert!(engine.registry().dev_mode_plugins.is_empty());
    }

    #[test]
    fn trusted_plugin_hash_change_respects_dev_mode() {
        let mut engine = PolicyDecisionEngine::new(TrustRegistry::default());
        let v1 = b"export const v = 1;";
        let v2 = b"export const v = 2;";
        let v1_hash = PolicyDecisionEngine::sha256_hex(v1);

        engine.approve_plugin("demo.plugin", &v1_hash);

        assert_eq!(
            engine.evaluate_plugin_load("demo.plugin", v1).verdict,
            PolicyVerdict::Allow
        );
        assert_eq!(
            engine.evaluate_plugin_reload("demo.plugin", v1).verdict,
            PolicyVerdict::Allow
        );

        // Hash changed without dev mode -> Ask on both load and reload.
        assert_eq!(
            engine.evaluate_plugin_load("demo.plugin", v2).verdict,
            PolicyVerdict::Ask
        );
        assert_eq!(
            engine.evaluate_plugin_reload("demo.plugin", v2).verdict,
            PolicyVerdict::Ask
        );

        // Enable dev mode after initial trust -> Allow on hash change.
        assert!(engine.set_dev_mode("demo.plugin", true));
        assert_eq!(
            engine.evaluate_plugin_load("demo.plugin", v2).verdict,
            PolicyVerdict::Allow
        );
        assert_eq!(
            engine.evaluate_plugin_reload("demo.plugin", v2).verdict,
            PolicyVerdict::Allow
        );

        // Disabling dev mode reverts hash-change reload to Ask.
        assert!(engine.set_dev_mode("demo.plugin", false));
        assert_eq!(
            engine.evaluate_plugin_reload("demo.plugin", v2).verdict,
            PolicyVerdict::Ask
        );
    }

    #[test]
    fn program_exec_blocks_dangerous_shells_and_unapproved_paths() {
        let mut engine = PolicyDecisionEngine::new(TrustRegistry::default());
        let no_args: Vec<String> = vec![];

        for shell in [
            "cmd",
            "cmd.exe",
            concat!("power", "shell"),
            concat!("power", "shell.exe"),
            concat!("POWER", "SHELL.EXE"),
            "pwsh",
            "pwsh.exe",
            "wscript",
            "cscript",
            "mshta",
            concat!(r"C:\Windows\System32\Windows", "Power", "Shell", r"\v1.0\", "power", "shell.exe"),
        ] {
            let d = engine.evaluate_program_exec(shell, &no_args);
            assert_eq!(
                d.verdict,
                PolicyVerdict::Deny,
                "expected Deny for shell {shell}"
            );
        }

        // Empty / invalid program -> Deny.
        assert_eq!(
            engine.evaluate_program_exec("", &no_args).verdict,
            PolicyVerdict::Deny
        );
        assert_eq!(
            engine.evaluate_program_exec("   ", &no_args).verdict,
            PolicyVerdict::Deny
        );

        // Unknown bare program -> Ask until approved.
        assert_eq!(
            engine.evaluate_program_exec("git", &["status".into()]).verdict,
            PolicyVerdict::Ask
        );
        engine.approve_program("git");
        assert_eq!(
            engine.evaluate_program_exec("git", &["status".into()]).verdict,
            PolicyVerdict::Allow
        );
        assert_eq!(
            engine.evaluate_program_exec("git.exe", &[]).verdict,
            PolicyVerdict::Allow
        );

        // Path-qualified executable is NOT auto-allowed by basename entry.
        assert_eq!(
            engine.evaluate_program_exec(r"C:\tmp\git.exe", &[]).verdict,
            PolicyVerdict::Ask
        );

        // Explicit approval of a path or shell allows it.
        engine.approve_program(r"C:\tmp\git.exe");
        assert_eq!(
            engine.evaluate_program_exec(r"C:\tmp\git.exe", &[]).verdict,
            PolicyVerdict::Allow
        );
        engine.approve_program("pwsh");
        assert_eq!(
            engine.evaluate_program_exec("pwsh", &[]).verdict,
            PolicyVerdict::Allow
        );
    }

    #[test]
    fn pty_open_respects_registry_flag() {
        let mut engine = PolicyDecisionEngine::new(TrustRegistry::default());
        assert_eq!(engine.evaluate_pty_open().verdict, PolicyVerdict::Ask);

        let mut allowed_reg = TrustRegistry::default();
        allowed_reg.allow_pty = true;
        let mut engine_allowed = PolicyDecisionEngine::new(allowed_reg);
        assert_eq!(
            engine_allowed.evaluate_pty_open().verdict,
            PolicyVerdict::Allow
        );
    }

    #[test]
    fn rpc_plugin_run_denies_when_not_exposed_or_untrusted() {
        let mut engine = PolicyDecisionEngine::new(TrustRegistry::default());

        // Not exposed -> Deny
        assert_eq!(
            engine.evaluate_rpc_plugin_run("p1", "cmd", false).verdict,
            PolicyVerdict::Deny
        );
        // Exposed but untrusted -> Deny
        assert_eq!(
            engine.evaluate_rpc_plugin_run("p1", "cmd", true).verdict,
            PolicyVerdict::Deny
        );

        // Trusted but not exposed -> still Deny
        let hash = PolicyDecisionEngine::sha256_hex(b"ok");
        engine.approve_plugin("p1", &hash);
        assert_eq!(
            engine.evaluate_rpc_plugin_run("p1", "cmd", false).verdict,
            PolicyVerdict::Deny
        );

        // Trusted AND exposed -> Allow
        assert_eq!(
            engine.evaluate_rpc_plugin_run("p1", "cmd", true).verdict,
            PolicyVerdict::Allow
        );
    }

    #[test]
    fn audit_log_caps_at_128_entries_as_ring_buffer() {
        let mut engine = PolicyDecisionEngine::new(TrustRegistry::default());
        for i in 0..150 {
            let subject = format!("prog-{i}");
            let _ = engine.evaluate_program_exec(&subject, &[]);
        }

        let log = engine.audit_log();
        assert_eq!(log.len(), AUDIT_LOG_CAPACITY);
        assert_eq!(log.first().unwrap().subject, "prog-22");
        assert_eq!(log.last().unwrap().subject, "prog-149");
    }

    #[test]
    fn serde_json_format_matches_contract() {
        let decision = PolicyDecision {
            verdict: PolicyVerdict::Ask,
            rule_id: "exec.unapproved_program".to_string(),
            reason: "not approved".to_string(),
            subject: "pandoc".to_string(),
            timestamp_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&decision).unwrap();
        assert!(json.contains(r#""verdict":"ask""#));
        assert!(json.contains(r#""ruleId":"exec.unapproved_program""#));
        assert!(json.contains(r#""timestampMs":1700000000000"#));
    }
}
