//! `RootRegistry` — the server-declared filesystem roots a caller-supplied
//! root/cwd argument must name (#1042, #888 §0 layer 2 / §1.1 H3).
//!
//! # Why this exists
//!
//! #904 closed `group_id` and #925 closed the identifier families that become a
//! path *component*. Neither touches the other half of §0's "server-declared
//! roots": the `ft_*`/`fm_*`/`git_*`/`gh_*` families take a caller-supplied
//! **absolute root** and check it with `is_dir()` and nothing else. No segment
//! validator can express that check, because there is no predicate over a
//! string that separates a repo from `~/.ssh` — the difference is not in the
//! path's shape, it is in whether anybody ever declared it.
//!
//! Today those commands are safe for exactly the reason `group_id` used to be:
//! the only caller able to invoke a `#[tauri::command]` is our own in-process
//! webview. That is a fact about the transport, not a credential. Reproduce the
//! command surface over a socket (#888) and every peer that can connect becomes
//! "the webview" — and an arbitrary absolute root is then an arbitrary read.
//!
//! So the trust moves off the transport and onto **server-held state**: a root
//! is usable iff it is in this registry, and only sources the engine itself
//! trusts can put one there.
//!
//! # This is wire enforcement, not a local sandbox
//!
//! The registry does not exist to constrain the local desktop. The local webview
//! already owns the disk; it may admit anything, so desktop behaviour does not
//! regress by construction. The teeth are entirely in what *cannot* admit: the
//! wire gets no admit path at all (`admit_root` is classified off-roster), so a
//! remote peer can **use** declared roots and can never **mint** one.
//!
//! # Never persisted
//!
//! There is deliberately no `Serialize`/`Deserialize` on either type in this
//! module, and the registry is rebuilt from its trusted sources on every boot.
//! A persisted registry file would be precisely the replay-poisonable artifact
//! this design has to defeat: an entry admitted once — or injected into the file
//! — would outlive every reason it was admitted for. Rebuilt-from-trusted-sources
//! makes replay poisoning structurally impossible at this layer, which is why
//! the absent impls are load-bearing rather than an omission.
//!
//! # The descendant rule
//!
//! [`RootRegistry::resolve`] accepts a candidate whose canonical form **equals
//! or is a descendant of** a declared root, and refuses everything else —
//! notably an **ancestor**, which would grant strictly *more* than was declared.
//! A subdirectory grants strictly less, so a pane that `cd`s around inside an
//! admitted repo keeps working without anything new being declared, while a pane
//! that `cd`s to `~/.ssh` resolves to nothing.
//!
//! # `plain` vs `canonical`, and why both
//!
//! `std::fs::canonicalize` on Windows returns an extended-length path
//! (`\\?\C:\…`). That is exactly right as a **comparison key** — it resolves
//! symlinks and junctions, and it normalizes the case Windows does not care
//! about — and exactly wrong as a **working path**: MSYS git does not want one
//! as a subprocess `current_dir`, and no user wants to read one. So
//! [`DeclaredRoot`] carries both, and [`DeclaredRoot::as_path`] hands back
//! `plain` — the caller's own path, lexically normalized the way
//! `fileedit::safe_resolve` already normalizes a root. Commands therefore go on
//! feeding git/gh/the filesystem the same shape of path they do today.
//!
//! For that split to be safe the two must never name *different* directories,
//! which is the entire reason a `..` component is refused: `plain` folds `..`
//! lexically while the OS resolves it after following symlinks, so one crafted
//! candidate can canonicalize inside a declared root while its lexical fold
//! lands outside every root. See [`RootError::ParentTraversal`].
//!
//! # Not `AsRef<Path>`
//!
//! Mirroring `GroupId`'s refusal: [`DeclaredRoot`] has exactly one accessor,
//! [`DeclaredRoot::as_path`], so every place a declared root becomes a working
//! path is greppable. A blanket `AsRef<Path>` would make it invisible again.
//! There is likewise no public constructor, no `From<String>` and no
//! `Deserialize` — the only way to mint a `DeclaredRoot` is
//! [`RootRegistry::resolve`], so holding one is proof that *some* trusted source
//! declared a root containing it.
//!
//! Holding one is **not** membership. "May this caller touch this root?" is a
//! separate question, exactly as it is for a `GroupId`.
//!
//! See `doc/design/groupid-and-path-roots.md`.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;

/// Why a candidate is not a usable root.
///
/// Payload-light on purpose, the way `GroupIdError` is: the caller already holds
/// the offending string and can decide whether echoing it into a log line or a
/// wire error is appropriate. The variant is what has to be diagnosable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootError {
    /// Not an absolute path. This is the rule that refuses a Windows
    /// drive-relative `C:foo` (a `Prefix` with no `RootDir`, for which
    /// `is_absolute()` is `false`) as well as any plain relative path: both
    /// resolve against a *process* current directory, which is nobody's
    /// declaration — and in a daemon is not even a meaningful location.
    NotAbsolute,
    /// Contains a `..` component. Refused rather than folded, because `plain`
    /// folds it lexically while **Unix** resolves it after following symlinks.
    ///
    /// Those two answers differ whenever a symlink precedes the `..`, and the
    /// difference is exploitable in the direction that matters: a candidate can
    /// canonicalize *inside* a declared root — so the containment check passes —
    /// while its lexical fold names a directory outside every declared root,
    /// which is then what [`DeclaredRoot::as_path`] hands to `current_dir`. A
    /// root or cwd argument has no legitimate `..` in it, so refusing is free.
    ///
    /// Windows is not exposed to that divergence: Win32 path normalization folds
    /// `..` lexically before the filesystem sees the path, so the two agree
    /// there. The refusal is uniform anyway — this crate is the core a Linux
    /// daemon links (#888), and a rule that holds on one platform's path
    /// semantics only is a rule the next reader has to re-derive.
    ParentTraversal,
    /// Not a directory (or does not exist). A root names a directory; this is
    /// the same probe every root-taking command performs today, moved inside the
    /// gate rather than added beside it.
    NotADirectory,
    /// The path is a directory but could not be canonicalized — a permission
    /// error, or a race with something deleting it between the two calls.
    Unresolvable(io::ErrorKind),
    /// Well-formed, exists, and no trusted source ever declared a root
    /// containing it. **This is the new refusal** — every other variant is a
    /// shape or existence failure the current code already rejects.
    NotDeclared,
}

impl RootError {
    /// The machine code for `fileedit::err(code, msg)`.
    ///
    /// Only [`RootError::NotDeclared`] introduces a new one. The rest map onto
    /// codes the frontend already switches on, and deliberately so: a
    /// non-existent root answers `not-found` before this change and after it, so
    /// the *new* code appears exactly when the *new* refusal fires and never as
    /// a rename of an old one.
    pub fn code(&self) -> &'static str {
        match self {
            RootError::NotAbsolute | RootError::ParentTraversal => "invalid-path",
            RootError::NotADirectory | RootError::Unresolvable(_) => "not-found",
            RootError::NotDeclared => "root-not-declared",
        }
    }
}

impl fmt::Display for RootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RootError::NotAbsolute => write!(f, "root must be an absolute path"),
            RootError::ParentTraversal => write!(f, "root must not contain a '..' component"),
            RootError::NotADirectory => write!(f, "root is not a directory"),
            RootError::Unresolvable(k) => write!(f, "root could not be resolved: {k:?}"),
            RootError::NotDeclared => write!(f, "root is not a declared root"),
        }
    }
}

impl std::error::Error for RootError {}

/// A root a trusted source declared, resolved for one call.
///
/// Minted only by [`RootRegistry::resolve`]. No public constructor, no
/// `From<String>`, no `Deserialize`, no `AsRef<Path>` — see the module docs for
/// why each absence is load-bearing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredRoot {
    /// The caller's own path, lexically normalized. What commands actually use.
    plain: PathBuf,
    /// The fully resolved form the containment decision was made on. Kept so a
    /// later caller can compare roots without re-touching the filesystem.
    canonical: PathBuf,
}

impl DeclaredRoot {
    /// The one way a declared root becomes a working path.
    ///
    /// Returns `plain`, not `canonical`: on Windows the canonical form is an
    /// extended-length `\\?\C:\…` path, which is the right comparison key and
    /// the wrong thing to hand a git subprocess or a display string.
    ///
    /// This is deliberately a named method rather than an `AsRef<Path>` impl, so
    /// that every place a declared root reaches the filesystem can be found by
    /// grepping for `as_path()`.
    pub fn as_path(&self) -> &Path {
        &self.plain
    }
}

/// The set of roots trusted sources have declared.
///
/// In-memory for the lifetime of the process and never written to disk (module
/// docs). Populated by the local trusted display side and by the engine's own
/// derivations; the wire has no admit path.
///
/// Unbounded by design: entries are a few hundred bytes of path each and a
/// session declares a handful. Eviction would need an answer to "is this root
/// still in use by an open pane, a watcher, a queued delivery?", and a wrong
/// answer there is a live desktop regression — a cost with no matching benefit
/// while the ceiling is measured in kilobytes.
#[derive(Debug, Default)]
pub struct RootRegistry {
    /// Canonicalized keys. `BTreeSet` rather than `HashSet` so the eventual
    /// `roots_list` (deferred to the daemon work) enumerates deterministically.
    ///
    /// `self.roots` contains `self.root` as a substring, which is the needle
    /// `tests/groupid.rs`'s one-join scan uses for the *orchestration* root. It
    /// only flags a line that also carries `.join(`/`.push(`/`.clone()`/
    /// `.to_path_buf()`/`.parent()`, and none of the three lines below does — so
    /// if a later edit puts one of those on the same line as this field, expect
    /// a confusing failure from an unrelated test rather than a real finding.
    roots: RwLock<BTreeSet<PathBuf>>,
}

impl RootRegistry {
    /// An empty registry. Every process starts here — nothing is inherited from
    /// disk.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a root. Idempotent: the key is the canonical form, so two
    /// spellings of one directory (a symlink and its target, two casings on
    /// Windows) declare it once.
    ///
    /// Takes a `&Path` rather than a `&str` because half its callers hold a
    /// `PathBuf` the engine itself derived (a group directory, a worktree it
    /// created) and `to_string_lossy` on those is a silent corruption for a
    /// non-UTF-8 path. The command wrapper's `String` coerces with
    /// `Path::new`. [`RootRegistry::resolve`] keeps `&str` for the opposite
    /// reason: every one of its callers holds a wire-supplied string.
    pub fn admit(&self, path: &Path) -> Result<(), RootError> {
        let canonical = canonical_dir(path)?;
        self.roots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(canonical);
        Ok(())
    }

    /// Resolve a caller-supplied root against the declared set.
    ///
    /// Succeeds iff the candidate's canonical form equals, or is a descendant
    /// of, some declared root. An ancestor is refused: it grants more than was
    /// declared.
    ///
    /// Containment is compared canonical-against-canonical, which is what makes
    /// it symlink-sound in both directions — a link *into* a declared root
    /// resolves inside and grants nothing new, and a link *out of* one resolves
    /// outside and is refused.
    pub fn resolve(&self, candidate: &str) -> Result<DeclaredRoot, RootError> {
        let plain = Path::new(candidate);
        let canonical = canonical_dir(plain)?;
        let declared = self.roots.read().unwrap_or_else(|e| e.into_inner());
        // `Path::starts_with` is component-wise and true for equal paths, so this
        // one call is the whole "equals or is a descendant" rule. A string
        // prefix test would additionally accept a *sibling* whose name merely
        // extends a declared root's (`…/repo-evil` under `…/repo`).
        if declared.iter().any(|root| canonical.starts_with(root)) {
            Ok(DeclaredRoot {
                plain: lexical_normalize(plain),
                canonical,
            })
        } else {
            Err(RootError::NotDeclared)
        }
    }

    /// How many distinct roots are declared. Diagnostics, and the observable the
    /// idempotence/canonicalization tests assert on.
    pub fn declared_count(&self) -> usize {
        self.roots.read().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// The shape gate both entry points share, plus canonicalization.
///
/// Order matters and is cheapest-first: the two structural checks touch no
/// filesystem, and `is_dir` answers before `canonicalize` so a missing root
/// reports `not-found` exactly as it does today rather than as an io error kind.
fn canonical_dir(path: &Path) -> Result<PathBuf, RootError> {
    // SCRATCH MUTATION (#1042 red-before-green, never merged): all three
    // structural pre-checks removed — absolute, no `..`, is-a-directory — so
    // canonicalize is the only thing standing.
    let _ = Component::ParentDir;
    std::fs::canonicalize(path).map_err(|e| RootError::Unresolvable(e.kind()))
}

/// Fold `.` away without touching the filesystem, preserving verbatim/UNC
/// prefixes and the root. `..` never reaches here — [`canonical_dir`] refuses it
/// — so this is a normalization with no security opinion, which is the only
/// thing a lexical pass over a path containing symlinks can honestly be. This
/// helper is intentionally duplicated per module in this codebase — house style.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create `link` pointing at the directory `target`.
    ///
    /// On Windows a real symlink needs `SeCreateSymbolicLinkPrivilege`
    /// (Developer Mode, or an elevated process), which a CI runner may or may
    /// not have — so it falls back to a **junction**, which needs no privilege
    /// and which `canonicalize` (`GetFinalPathNameByHandle`) resolves
    /// identically. The fallback is what keeps the symlink assertions from
    /// quietly not running on the one platform whose path semantics this module
    /// is most careful about.
    #[cfg(windows)]
    fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return Ok(());
        }
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "neither symlink_dir nor `mklink /J` could create a directory link",
            ))
        }
    }

    #[cfg(unix)]
    fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    /// `&str` for a path, for the `resolve` calls — which take one because their
    /// real callers hold a wire-supplied string.
    fn s(p: &Path) -> String {
        p.to_str().expect("test paths are UTF-8").to_string()
    }

    #[test]
    fn admit_refuses_a_path_that_is_not_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("notadir.txt");
        fs::write(&file, b"x").unwrap();

        assert_eq!(
            Err(RootError::NotADirectory),
            RootRegistry::new().admit(&file)
        );

        let missing = tmp.path().join("does-not-exist");
        assert_eq!(
            Err(RootError::NotADirectory),
            RootRegistry::new().admit(&missing)
        );
    }

    #[test]
    fn admit_canonicalizes_so_two_spellings_of_one_directory_declare_it_once() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("repo");
        fs::create_dir(&real).unwrap();
        let alias = tmp.path().join("repo-link");
        link_dir(&real, &alias).unwrap();

        let reg = RootRegistry::new();
        reg.admit(&real).unwrap();
        // Idempotent for the identical spelling…
        reg.admit(&real).unwrap();
        // …and for a link that resolves to the same directory, which is the half
        // that only canonicalization can collapse.
        reg.admit(&alias).unwrap();
        assert_eq!(1, reg.declared_count());

        // The stored key is the canonical form, not either spelling: resolving
        // through the alias lands inside the root admitted under its real name.
        assert!(reg.resolve(&s(&alias)).is_ok());
    }

    #[test]
    fn nothing_resolves_against_an_empty_registry() {
        // The negative control: without it, a `resolve` that refused everything
        // would satisfy every refusal test below.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("repo");
        fs::create_dir(&dir).unwrap();

        assert_eq!(
            Err(RootError::NotDeclared),
            RootRegistry::new().resolve(&s(&dir))
        );
    }

    #[test]
    fn resolve_accepts_the_declared_root_and_its_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let deep = root.join("src").join("nested");
        fs::create_dir_all(&deep).unwrap();

        let reg = RootRegistry::new();
        reg.admit(&root).unwrap();

        assert_eq!(
            lexical_normalize(&root),
            reg.resolve(&s(&root)).unwrap().as_path()
        );
        // The descendant rule: a subdirectory grants strictly less than the
        // ancestor that was declared, so it needs no declaration of its own.
        assert_eq!(
            lexical_normalize(&deep),
            reg.resolve(&s(&deep)).unwrap().as_path()
        );
    }

    #[test]
    fn resolve_refuses_an_ancestor_of_a_declared_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        fs::create_dir(&root).unwrap();

        let reg = RootRegistry::new();
        reg.admit(&root).unwrap();

        // An ancestor grants strictly MORE than was declared — the whole point
        // of the rule being one-directional.
        assert_eq!(
            Err(RootError::NotDeclared),
            reg.resolve(&s(tmp.path())),
            "the parent of a declared root must not resolve"
        );
    }

    #[test]
    fn resolve_refuses_a_sibling_whose_name_merely_extends_the_declared_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let sibling = tmp.path().join("repo-evil");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&sibling).unwrap();

        let reg = RootRegistry::new();
        reg.admit(&root).unwrap();

        // `"…/repo-evil".starts_with("…/repo")` is TRUE as a string and false as
        // a path. This is the assertion that pins the comparison to components.
        assert_eq!(
            Err(RootError::NotDeclared),
            reg.resolve(&s(&sibling)),
            "containment must be component-wise, not a string prefix"
        );
    }

    #[test]
    fn resolve_refuses_a_drive_relative_or_relative_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        fs::create_dir(&root).unwrap();

        let reg = RootRegistry::new();
        reg.admit(&root).unwrap();

        // `C:repo` is drive-RELATIVE: on Windows it resolves against whatever
        // the process happens to have as the current directory of drive C, which
        // no one declared. `Path::is_absolute` is false for it (a `Prefix` with
        // no `RootDir`), and false for a bare relative path everywhere — so one
        // check refuses both, and it refuses them on the shape rather than on
        // the accident of the target not existing.
        assert_eq!(Err(RootError::NotAbsolute), reg.resolve("C:repo"));
        assert_eq!(Err(RootError::NotAbsolute), reg.resolve("repo"));
        assert_eq!(Err(RootError::NotAbsolute), reg.resolve(""));
    }

    #[test]
    fn resolve_refuses_a_symlink_whose_target_leaves_every_declared_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let secrets = tmp.path().join("secrets");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&secrets).unwrap();
        let escape = root.join("escape");
        link_dir(&secrets, &escape).unwrap();

        let reg = RootRegistry::new();
        reg.admit(&root).unwrap();

        // Lexically this path is inside the declared root. Only canonicalization
        // sees that it is not.
        assert_eq!(
            Err(RootError::NotDeclared),
            reg.resolve(&s(&escape)),
            "a link out of a declared root must be refused"
        );
    }

    #[test]
    fn resolve_follows_a_link_that_stays_inside_and_hands_back_the_callers_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let real = root.join("src");
        fs::create_dir_all(&real).unwrap();
        let inward = root.join("src-link");
        link_dir(&real, &inward).unwrap();

        let reg = RootRegistry::new();
        reg.admit(&root).unwrap();

        // A link INTO a declared root grants nothing new, so it resolves…
        let declared = reg.resolve(&s(&inward)).unwrap();
        // …and what comes back is the caller's own path, not the link target and
        // not the canonical form. On Windows the canonical form is a `\\?\C:\…`
        // extended-length path, which is exactly what must not reach a git
        // subprocess's `current_dir` or a display string.
        assert_eq!(lexical_normalize(&inward), declared.as_path());
    }

    #[test]
    fn resolve_refuses_a_parent_component_that_would_split_plain_from_canonical() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        // Four levels, matched by the four `..` below, so the canonical form
        // lands back exactly on the declared root.
        let deep = root.join("a").join("b").join("c").join("d");
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir(root.join("x")).unwrap();
        let link = root.join("link");
        link_dir(&deep, &link).unwrap();

        let reg = RootRegistry::new();
        reg.admit(&root).unwrap();

        // On Unix, `<root>/link/../../../../x` canonicalizes to `<root>/x` —
        // INSIDE the declared root, so containment would pass — while the
        // lexical fold that produces `plain` pops `link` and then three
        // components of `<root>` itself, landing outside every declared root.
        // `as_path()` would then hand a caller that escaped path. The `..`
        // refusal is what stops the two from ever disagreeing.
        //
        // On Windows the same string is refused by the same check, but the
        // escape it forecloses does not exist there: Win32 normalization folds
        // `..` lexically before the filesystem sees it, so without the check
        // this candidate fails as `NotADirectory` rather than resolving. Stated
        // rather than glossed — the assertion is identical on all three
        // platforms, the hazard it closes is not.
        let candidate = format!("{}/link/../../../../x", s(&root));
        match reg.resolve(&candidate) {
            Err(RootError::ParentTraversal) => {}
            other => panic!(
                "a '..' component must be refused before containment is even \
                 considered; got: {other:?}"
            ),
        }
    }

    #[test]
    fn only_the_undeclared_refusal_introduces_a_new_error_code() {
        // Slice C maps these into `fileedit::err(code, …)`. The shape/existence
        // refusals must keep answering what a root-taking command answers today,
        // so the new code appears exactly when the new refusal fires.
        assert_eq!("root-not-declared", RootError::NotDeclared.code());
        assert_eq!("not-found", RootError::NotADirectory.code());
        assert_eq!(
            "not-found",
            RootError::Unresolvable(io::ErrorKind::PermissionDenied).code()
        );
        assert_eq!("invalid-path", RootError::NotAbsolute.code());
        assert_eq!("invalid-path", RootError::ParentTraversal.code());
    }
}
