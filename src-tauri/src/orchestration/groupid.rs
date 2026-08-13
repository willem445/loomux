//! `GroupId` — the validated identifier every group-scoped path is built from
//! (#904).
//!
//! # Why this type exists
//!
//! CLAUDE.md hard constraint 6 says orchestration commands *trust* `group_id` as
//! a path segment. That trust was never a credential: it was derived from
//! **process locality** — the only caller able to invoke a `#[tauri::command]`
//! is our own in-process webview. `group_dir()` was `self.root.join(group)`, no
//! validation, no canonicalization, and the one membership guard on that path
//! lived in `save_attachment` alone.
//!
//! #888 §0 layer 2 is what breaks it: reproduce the command surface over a
//! socket and "the caller is our webview" stops being a fact about the process
//! and becomes an assumption about the network. A peer that can send
//! `group_id: "../../../../Users/me/.ssh"` gets a path join for free.
//!
//! So the trust moves off the transport and onto a **type**. A `GroupId` can be
//! built exactly one way — [`GroupId::parse`] — and that constructor refuses
//! anything that is not a single, path-safe, ASCII identifier. There is no
//! `From<String>`, no public field, no `new_unchecked`. Deserialization goes
//! through the same check, so a hand-edited state file cannot mint one either.
//!
//! Deliberately **not** implemented: `AsRef<Path>`. A `GroupId` must not be
//! joinable directly — it becomes a path only inside
//! `OrchRegistry::group_dir`, the single declared assembly point (#904 scope
//! item 2). See `doc/design/groupid-and-path-roots.md`.
//!
//! # The alphabet, and why it is this one
//!
//! Group ids are loomux-minted tokens, never user prose: `group_id_for_repo`
//! emits `{slug}-{8hex}` (slug = ASCII alphanumerics/`-`/`_` from the repo
//! directory name, ≤24 chars), `create_group_ex` may append `-{n}` to
//! disambiguate concurrent groups on one repo, and [`SOLO_GROUP`] is the fixed
//! constant `__solo__`. A strict alphabet therefore costs nothing and rejects
//! every path-shaped attack by construction rather than by enumeration:
//! `/` and `\` (separators), `.` (so `..` and `.` are unspellable), `:` (drive
//! letters, NTFS alternate data streams), NUL and every other control byte,
//! whitespace, and every non-ASCII byte (which is where Unicode
//! normalization/homoglyph confusion would otherwise live).
//!
//! Same reasoning as [`crate::orchestration::mergeq::valid_id_component`], which
//! closed the same hole for merge-queue batch ids. Kept as a separate type
//! rather than a shared predicate because a `bool` returned by a free function
//! is a fact about a moment; a `GroupId` is a fact that travels with the value.
//!
//! **Rejected, never rewritten.** `parse` does not trim, lowercase, or sanitize:
//! an id is either usable as-is or refused. Normalizing would let two distinct
//! strings name one directory, which is how a membership check and a path join
//! end up disagreeing.
//!
//! [`SOLO_GROUP`]: crate::orchestration::SOLO_GROUP

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

use serde::de::{self, Deserialize, Deserializer};
use serde::{Serialize, Serializer};

/// Why a candidate string is not a usable group id.
///
/// Carries the offending detail so the refusal is diagnosable from a log line
/// without echoing the whole (possibly attacker-chosen) string back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupIdError {
    /// Empty string. A group id names a directory; the empty name would resolve
    /// to the orchestration root itself.
    Empty,
    /// Longer than [`GroupId::MAX_LEN`]. Minted ids top out around 36 bytes.
    TooLong(usize),
    /// A byte outside `[A-Za-z0-9_-]`. This is the rule that makes `..`, `/`,
    /// `\`, `:`, NUL and every non-ASCII byte unspellable.
    IllegalChar(char),
    /// Starts with `-`. Path-safe, but a bare `-foo` is an option to any
    /// command line the id is ever interpolated into — the same hazard
    /// `valid_id_component` refuses for batch ids.
    LeadingDash,
    /// A Windows reserved device name (`CON`, `NUL`, `COM3`, …). Such a path
    /// does not name a file on Windows at all; it opens a device.
    ReservedDeviceName,
}

impl fmt::Display for GroupIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GroupIdError::Empty => write!(f, "group id is empty"),
            GroupIdError::TooLong(n) => {
                write!(f, "group id is {n} bytes, max {}", GroupId::MAX_LEN)
            }
            GroupIdError::IllegalChar(c) => {
                write!(f, "group id contains {c:?}; only [A-Za-z0-9_-] is allowed")
            }
            GroupIdError::LeadingDash => write!(f, "group id starts with '-'"),
            GroupIdError::ReservedDeviceName => {
                write!(f, "group id is a reserved Windows device name")
            }
        }
    }
}

impl std::error::Error for GroupIdError {}

/// Windows device names. Reserved with *any* extension and case-insensitively,
/// but the alphabet already bans `.`, so a plain stem comparison is complete
/// here.
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A validated orchestration group identifier.
///
/// The only constructor is [`GroupId::parse`]. Holding one is proof the string
/// inside is a single path-safe segment — which is what
/// `OrchRegistry::group_dir` demands before it will join anything onto the
/// orchestration root.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(String);

impl GroupId {
    /// Longest accepted id. Minted ids are ≤ ~36 bytes (`{slug≤24}-{8hex}` plus
    /// an optional `-{n}`), so this is headroom, not a fit — it exists to keep
    /// a hostile id from pushing a path past a filesystem limit.
    pub const MAX_LEN: usize = 64;

    /// The one gate. Every `GroupId` in the process came through here.
    pub fn parse(s: &str) -> Result<Self, GroupIdError> {
        if s.is_empty() {
            return Err(GroupIdError::Empty);
        }
        if s.len() > Self::MAX_LEN {
            return Err(GroupIdError::TooLong(s.len()));
        }
        if let Some(c) = s
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
        {
            return Err(GroupIdError::IllegalChar(c));
        }
        if s.starts_with('-') {
            return Err(GroupIdError::LeadingDash);
        }
        // `eq_ignore_ascii_case` rather than allocating a lowercase copy: the
        // alphabet is ASCII by the check above, so case folding is byte-local.
        if RESERVED_DEVICE_NAMES
            .iter()
            .any(|r| s.eq_ignore_ascii_case(r))
        {
            return Err(GroupIdError::ReservedDeviceName);
        }
        Ok(GroupId(s.to_string()))
    }

    /// The validated identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the newtype for the rare API that owns a `String` (JSON payload
    /// fields, env values). One-way on purpose: getting back requires `parse`.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Deref for GroupId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for GroupId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Lets a `HashMap<GroupId, _>` be probed with a `&str` without minting an id
/// just to look one up.
impl Borrow<str> for GroupId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for GroupId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for GroupId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for GroupId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl PartialEq<GroupId> for str {
    fn eq(&self, other: &GroupId) -> bool {
        self == other.0.as_str()
    }
}

impl<'a> PartialEq<GroupId> for &'a str {
    fn eq(&self, other: &GroupId) -> bool {
        *self == other.0.as_str()
    }
}

impl PartialEq<GroupId> for String {
    fn eq(&self, other: &GroupId) -> bool {
        self == &other.0
    }
}

impl FromStr for GroupId {
    type Err = GroupIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        GroupId::parse(s)
    }
}

impl TryFrom<&str> for GroupId {
    type Error = GroupIdError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        GroupId::parse(s)
    }
}

impl TryFrom<String> for GroupId {
    type Error = GroupIdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        GroupId::parse(&s)
    }
}

/// Transparent on the wire: a `GroupId` serializes as the bare string, so no
/// persisted file or frontend payload changes shape.
impl Serialize for GroupId {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.0)
    }
}

/// Validating on the way *in*: a state file edited by hand — or written by an
/// older build, or by anything at all — cannot smuggle an unchecked id past the
/// constructor. Deserialization is a construction site like any other.
impl<'de> Deserialize<'de> for GroupId {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        GroupId::parse(&s).map_err(de::Error::custom)
    }
}
