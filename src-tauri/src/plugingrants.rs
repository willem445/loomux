//! Install-time capability approval for pane plugins (#377) — the consent
//! boundary `doc/design/pane-plugins.md`'s trust model promises and, until
//! this module existed, did not have.
//!
//! **The gap this closes.** Installing a plugin is a folder copy
//! (`plugins.rs::install_plugin_from`); whatever its `plugin.json` declared
//! in `capabilities` was granted the moment the copy succeeded, with no human
//! ever being shown — let alone asked about — what the plugin could reach.
//! The manifest author decided; the human running loomux did not.
//!
//! **Where the decision is enforced: OPEN, not install.** A folder sitting on
//! disk grants nothing — nothing runs, nothing attaches, no broker session
//! exists. `pluginbroker::plugin_open_window` is where a plugin actually
//! attaches, renders, and gets a [`crate::pluginbroker::PluginSession`] the
//! broker will answer requests against, so that is where the grant is
//! required (`validate_open_request`). Install and the boot-time seeding of
//! the bundled example stay approval-free by design: gating the *copy* would
//! both prompt for a plugin the human may never open and prompt at a moment
//! (headless boot seeding) when no human is present to answer.
//!
//! **Fail closed.** No record for a plugin id means NOT approved — a missing,
//! empty, or corrupt `grants.json` (quarantined by
//! `uistate::load_or_quarantine`, same discipline as `tabs.json`) denies every
//! open rather than defaulting to the old auto-grant. The rule is a subset
//! test, not equality: a plugin may open iff every capability it requests is
//! in the persisted grant. So **widening** a manifest (a re-install, an
//! upgrade that adds `fs.read`) refuses and re-prompts, while **narrowing**
//! one needs no second conversation — the human already consented to strictly
//! more than what is being asked for.
//!
//! **Who can approve: only a human, structurally.** The one writer is
//! [`plugin_approve_capabilities`], granted to the trusted `main` webview
//! alone (`permissions/sets/plugins.toml` -> `main-ui`). A plugin's own child
//! webview gets `plugin-broker` and nothing else
//! (`capabilities/plugin.json`), so a plugin cannot approve itself; there is
//! no MCP tool, no CLI path and no orchestration command that reaches this
//! store, and none may ever be added (CLAUDE.md constraint 9 — an agent must
//! never self-approve a security gate; that applies to loomux's own agents
//! writing `grants.json` to make a test pass, too. Tests inject
//! `plugins_root` and go through these functions).
//!
//! **Storage.** `<data dir>/loomux/plugins/grants.json`, a sibling of the
//! per-plugin `storage/` dir, written with `uistate.rs`'s atomic-write +
//! quarantine discipline rather than a second storage layer. It is a plain
//! file inside the plugins root, which `plugins::discover_installed` skips
//! (that scan only descends into directories), so it cannot be mistaken for
//! an installed plugin. Every function here takes `plugins_root` as a
//! parameter — the same injection `plugins.rs` uses so a tempdir can stand in
//! for the real data dir under test.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Stable machine code before the first ": ", same house style as
/// `plugins.rs`/`fileedit.rs`, so the frontend can branch on the reason
/// (`src/pluginhost.ts`'s `pluginErrorCode`) without parsing prose.
fn err(code: &str, msg: impl AsRef<str>) -> String {
    format!("{code}: {}", msg.as_ref())
}

/// The error code a refused open carries when the human has not approved (or
/// has not yet approved the *current*, widened) capability set. Named once
/// here because three places have to agree on it: the refusal in
/// `pluginbroker::validate_open_request`, the frontend's consent surface
/// (`src/pluginpaneview.ts`, which shows the approval prompt on exactly this
/// code and nothing else), and the tests.
pub const NOT_APPROVED_CODE: &str = "capability-not-approved";

/// One human decision, persisted. `capabilities` is the exact set that was
/// shown and approved (sorted + deduped so the file is stable and a subset
/// test is cheap); `approved_at_version` records WHICH version of the plugin
/// the human was looking at, so a later audit can tell a grant made against
/// 1.0.0 from one made against 2.0.0 even when the capability set didn't
/// change. `decided_at` is unix epoch seconds (this crate has no date
/// library — see `sessions.rs`/`obs.rs` for the same choice).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRecord {
    pub capabilities: Vec<String>,
    pub decided_at: u64,
    pub approved_at_version: String,
}

/// pluginId -> the human's decision for it. A `BTreeMap` (not a `HashMap`) so
/// the on-disk file has a stable key order and a diff of it is readable.
pub type GrantStore = BTreeMap<String, GrantRecord>;

/// What the store says about one plugin's CURRENT declared capability set —
/// the pure decision `plugin_open_window` and the frontend's consent surface
/// both read, implemented once here so the rule can't be re-derived (or
/// quietly softened) at a second call site. Same house move as
/// `pluginbroker::check_request`.
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalStatus {
    /// Every requested capability is covered by the persisted grant.
    Approved,
    /// No record at all — a fresh install, a `grants.json` that was deleted
    /// or quarantined as corrupt, or a plugin whose grant a human revoked by
    /// editing the file. All of them fail closed identically.
    NeverApproved,
    /// A record exists, but the plugin now asks for capabilities it doesn't
    /// cover. `added` is exactly those — the delta the re-prompt shows, so a
    /// human upgrading a plugin sees "this now also wants fs.read" rather
    /// than re-reading the whole list to spot what changed.
    Widened { added: Vec<String> },
}

/// Sorted + deduped, so an approved set and a requested set compare
/// order-independently and the persisted file doesn't churn on manifest key
/// order.
pub fn normalize_capabilities(capabilities: &[String]) -> Vec<String> {
    let mut out = capabilities.to_vec();
    out.sort();
    out.dedup();
    out
}

pub fn grants_path(plugins_root: &Path) -> PathBuf {
    plugins_root.join("grants.json")
}

/// Read the whole store. A missing file (nothing approved yet), a corrupt one
/// (quarantined aside by `load_or_quarantine`, exactly as `tabs.json` is), or
/// one whose JSON doesn't match this shape all collapse to an EMPTY store —
/// which denies every plugin, never grants one. Losing the file costs the
/// human one re-approval; misreading it the other way would cost them the
/// consent boundary.
pub fn load_grants(plugins_root: &Path) -> GrantStore {
    crate::uistate::load_or_quarantine(&grants_path(plugins_root))
        .and_then(|raw| serde_json::from_str::<GrantStore>(&raw).ok())
        .unwrap_or_default()
}

pub fn save_grants(plugins_root: &Path, store: &GrantStore) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(store).map_err(|e| err("io", e.to_string()))?;
    crate::uistate::write_atomic(&grants_path(plugins_root), &raw).map_err(|e| err("io", e))
}

/// The pure rule: a plugin may open iff every capability it requests is
/// already in the persisted grant. Subset, not equality — see the module doc
/// comment on why narrowing needs no re-prompt.
pub fn approval_status(store: &GrantStore, plugin_id: &str, requested: &[String]) -> ApprovalStatus {
    let Some(record) = store.get(plugin_id) else {
        return ApprovalStatus::NeverApproved;
    };
    let added: Vec<String> = normalize_capabilities(requested)
        .into_iter()
        .filter(|c| !record.capabilities.contains(c))
        .collect();
    if added.is_empty() {
        ApprovalStatus::Approved
    } else {
        ApprovalStatus::Widened { added }
    }
}

/// `approval_status` reduced to the one bit the open gate needs.
pub fn is_approved(plugins_root: &Path, plugin_id: &str, requested: &[String]) -> bool {
    approval_status(&load_grants(plugins_root), plugin_id, requested) == ApprovalStatus::Approved
}

/// Persist one decision, replacing any earlier record for that id (a
/// re-approval after a widened manifest is a NEW decision about a NEW set,
/// not a merge with the old one — merging would silently keep capabilities a
/// downgraded plugin no longer declares).
pub fn record_grant(
    plugins_root: &Path,
    plugin_id: &str,
    capabilities: &[String],
    version: &str,
    decided_at: u64,
) -> Result<GrantRecord, String> {
    let record = GrantRecord {
        capabilities: normalize_capabilities(capabilities),
        decided_at,
        approved_at_version: version.to_string(),
    };
    let mut store = load_grants(plugins_root);
    store.insert(plugin_id.to_string(), record.clone());
    save_grants(plugins_root, &store)?;
    Ok(record)
}

/// Unix epoch seconds; the only impure input any of the above takes, kept out
/// of them so every rule here is testable with a fixed timestamp.
pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Approve exactly the set a human was just shown for `plugin_id`.
///
/// `shown` is not taken on trust as the thing to persist: the manifest is
/// re-read from disk here and the two must MATCH, or this refuses with
/// `manifest-changed`. That closes the window between the moment the consent
/// surface read `list_plugins` and the moment the human pressed Approve — a
/// plugin folder rewritten in between must not be able to convert a consent
/// to `["storage"]` into a grant of whatever it declares now. It also means
/// the persisted set is always a set that really exists in a manifest, never
/// a caller's construction, and that `approved_at_version` is the version on
/// disk at the moment of the decision rather than one the caller asserted.
pub fn approve_capabilities(
    plugins_root: &Path,
    plugin_id: &str,
    shown: &[String],
    decided_at: u64,
) -> Result<GrantRecord, String> {
    let manifest = crate::plugins::manifest_for_installed(plugins_root, plugin_id)?;
    let declared = normalize_capabilities(&manifest.capabilities);
    if declared != normalize_capabilities(shown) {
        return Err(err(
            "manifest-changed",
            format!(
                "`{plugin_id}` now declares [{}], not the [{}] that was approved — review it again",
                declared.join(", "),
                normalize_capabilities(shown).join(", ")
            ),
        ));
    }
    record_grant(plugins_root, plugin_id, &declared, &manifest.version, decided_at)
}

// ---------- pre-seeded grants (the bundled example: OPEN HUMAN DECISION) ----------

/// Plugin ids whose declared capabilities are granted automatically at boot,
/// without a human ever seeing the prompt.
///
/// **Empty on purpose, and that is the fail-closed default: every plugin,
/// including the bundled `resource-monitor` example, prompts on first open.**
/// Whether the FIRST-PARTY bundled example should instead ship pre-approved —
/// trading the "no capability is ever live without a human decision"
/// invariant for a zero-click demo — is an OPEN HUMAN DECISION (#377, plan
/// §3's "Bundled resource-monitor" bullet). This module deliberately makes
/// both outcomes reachable without a redesign: choosing pre-seeding is adding
/// `crate::plugins::BUNDLED_EXAMPLE_PLUGIN_ID` to this list, and nothing
/// else. No agent may make that call unprompted (CLAUDE.md constraint 9) —
/// it is a security decision, and widening a consent gate "for convenience"
/// is exactly the shape that constraint exists to stop.
pub const PRE_SEEDED_GRANT_PLUGIN_IDS: &[&str] = &[];

/// Seed a grant for each id in `plugin_ids` that doesn't already have one,
/// from that plugin's own installed manifest. Called once from `lib.rs`'s
/// `.setup()` with [`PRE_SEEDED_GRANT_PLUGIN_IDS`] — a no-op while that list
/// is empty, which is today's shipped behavior.
///
/// **Never overwrites an existing record**, so it can't undo a human's
/// decision (the same rule `plugins::seed_bundled_example_plugin` follows for
/// the folder itself): a human who approved a narrower set, or who revoked a
/// grant by editing `grants.json`, keeps that across every later boot.
/// Best-effort: a plugin that isn't installed, or whose manifest no longer
/// parses, is skipped silently — this runs during startup and must never
/// block it.
pub fn seed_declared_grants(plugins_root: &Path, plugin_ids: &[&str], decided_at: u64) {
    if plugin_ids.is_empty() {
        return;
    }
    let store = load_grants(plugins_root);
    for id in plugin_ids {
        if store.contains_key(*id) {
            continue;
        }
        let Ok(manifest) = crate::plugins::manifest_for_installed(plugins_root, id) else {
            continue;
        };
        let seeded = record_grant(
            plugins_root,
            id,
            &manifest.capabilities,
            &manifest.version,
            decided_at,
        );
        crate::obs::breadcrumb(
            "plugins",
            &format!(
                "seed_declared_grants: `{id}` -> {}",
                match &seeded {
                    Ok(r) => format!("granted [{}]", r.capabilities.join(", ")),
                    Err(e) => format!("skipped ({e})"),
                }
            ),
        );
    }
}

// ---------- tauri command ----------
//
// Thin wrapper: the logic lives in the `pub fn`s above so `tests/plugins.rs`
// exercises it against a tempdir with no Tauri runtime.

/// Persist the human's approval of `capabilities` for `plugin_id`. Reachable
/// ONLY from the trusted `main` webview (`permissions/sets/plugins.toml`,
/// aggregated into `main-ui`) — never from a plugin's own child webview, and
/// never from any agent-facing surface. See this module's doc comment.
#[tauri::command]
pub fn plugin_approve_capabilities(plugin_id: String, capabilities: Vec<String>) -> Result<(), String> {
    approve_capabilities(
        &crate::plugins::plugins_root_dir(),
        &plugin_id,
        &capabilities,
        now_epoch_secs(),
    )
    .map(|_| ())
}
