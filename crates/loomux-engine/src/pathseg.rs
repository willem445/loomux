//! `PathSegment` — the one validated-identifier mechanism every caller-supplied
//! id that becomes a path component is expressed through (#925).
//!
//! # Why this exists as one type rather than one type per family
//!
//! #904 closed `group_id` with [`GroupId`](crate::groupid::GroupId): one
//! validating constructor, a strict alphabet, `Deserialize` through the same
//! gate, and no `AsRef<Path>`. #925 is the same treatment for what #904
//! deliberately left — and the obvious way to write it is a second newtype per
//! remaining family, each re-spelling the same five checks.
//!
//! That would have made the *fourth* copy of those checks, not the second. All
//! of these already existed, all of them answering "is this string usable as a
//! single path component":
//!
//! | validator | rules |
//! | --- | --- |
//! | `GroupId::parse` | non-empty, ≤64, `[A-Za-z0-9_-]`, no leading `-`, no device name |
//! | `mergeq::valid_id_component` | non-empty, ≤64, `[A-Za-z0-9_-]`, no leading `-` |
//! | `orchestration::sanitize_session` | non-empty, ≤64, `[A-Za-z0-9_-]` |
//! | `orchestration::digest::is_safe_session_id` | non-empty, no `/`, no `\`, not `.`/`..` |
//!
//! Read down that table and the problem is not the duplication, it is the
//! **drift**: the four are not the same check, they are four points on a slope,
//! and the weakest of them was the one guarding a live `Path::join`
//! (`read_session_transcript_events`'s copilot arm). A rule copied four times is
//! a rule that is only as strong as whichever copy an attacker reaches.
//!
//! So the mechanism is one type, and a family that needs its own *shape* wraps
//! it rather than restating it. `GroupId` stays a distinct type — a group id and
//! a session id must not be interchangeable at a call site — but it no longer
//! owns a private copy of the rules; it delegates here.
//!
//! # The alphabet, and why it is this one
//!
//! Identical to `GroupId`'s, and for the same reason: every id in this family is
//! **loomux-minted or vendor-minted**, never user prose. Claude session ids are
//! hyphenated hex UUIDs, opencode's are `ses_` + hex + base62, group ids are
//! `{slug}-{8hex}`, merge-queue batch ids are `mq-{hex}`. A strict alphabet
//! therefore costs nothing real and rejects every path-shaped attack by
//! construction rather than by enumeration:
//!
//! - `.` is not in it, so `..` and `.` are **unspellable** — no traversal;
//! - `/` and `\` are not in it, so no id names more than one component;
//! - `:` is not in it, so no drive letter and no NTFS alternate data stream;
//! - NUL, every other control byte and all whitespace are out, so no truncation
//!   at the syscall and no log-line injection;
//! - non-ASCII is out, which is where Unicode normalization and homoglyph
//!   confusion would otherwise live.
//!
//! The two rules that are not consequences of the alphabet each earn their own
//! line. A leading `-` is a legal path component but an *option* to any command
//! line the id is interpolated into. And a Windows reserved device name (`CON`,
//! `NUL`, `COM3`, …) is a path that opens a device rather than naming a file.
//!
//! ## `:` is not a theoretical entry on that list
//!
//! `is_safe_session_id` — the weakest row in the table above — rejected `/`,
//! `\`, `.` and `..` and nothing else, and it guarded
//! `copilot_session_state_root().join(session_id)`. `"C:"` passed it. On
//! Windows `"C:"` parses as a `Prefix` component, and `Path::join` **replaces**
//! the receiver when the argument carries a prefix — so the "session directory"
//! became `C:`, and the read that followed resolved drive-relative to the
//! process's own current directory, outside the session-state root entirely. No
//! separator required.
//!
//! # Refused, never rewritten
//!
//! [`PathSegment::parse`] does not trim, lowercase or sanitize. An id is either
//! usable exactly as written or it is refused. Normalizing would let two
//! distinct strings name one directory, which is how a membership check and a
//! path join end up disagreeing about which thing they are talking about.
//!
//! This is the one place the older validators genuinely differed rather than
//! merely drifting: `mergeq::valid_id_component` and
//! `orchestration::sanitize_session` both `trim()` first. Neither is converted
//! here — that would be a behavior change (`" mq-1 "` is accepted today) wearing
//! a refactor's clothes, and it is not what #925 asks for.
//!
//! # Deliberately not implemented: `AsRef<Path>`
//!
//! A `PathSegment` must not be joinable directly, for the same reason a
//! `GroupId` must not be: holding a validated id is not the same as being
//! allowed to build a path from it wherever you like. The absence is what makes
//! the compiler — not a textual scan — the thing that stops a validated id
//! reaching an undeclared `join` as a *value*.
//!
//! Because of Rust's orphan rule, the only crate an
//! `impl AsRef<Path> for PathSegment` can be written in is this one. The
//! source-scanning tests in `src-tauri/tests/groupid.rs` assert its absence and
//! walk this crate's source root for exactly that reason; see
//! `doc/design/groupid-and-path-roots.md`.

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

use serde::de::{self, Deserialize, Deserializer};
use serde::{Serialize, Serializer};

/// Why a candidate string is not usable as a single path component.
///
/// Carries the offending detail so a refusal is diagnosable from a log line
/// without echoing the whole (possibly attacker-chosen) string back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SegmentError {
    /// Empty string. A segment names a directory entry; the empty name would
    /// resolve to the root it was about to be joined onto.
    Empty,
    /// Longer than [`PathSegment::MAX_LEN`].
    TooLong(usize),
    /// A byte outside `[A-Za-z0-9_-]`. This is the rule that makes `..`, `/`,
    /// `\`, `:`, NUL and every non-ASCII byte unspellable.
    IllegalChar(char),
    /// Starts with `-`. Path-safe, but a bare `-foo` is an option to any command
    /// line the id is ever interpolated into.
    LeadingDash,
    /// A Windows reserved device name (`CON`, `NUL`, `COM3`, …). Such a path
    /// does not name a file on Windows at all; it opens a device.
    ReservedDeviceName,
}

impl fmt::Display for SegmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SegmentError::Empty => write!(f, "identifier is empty"),
            SegmentError::TooLong(n) => {
                write!(f, "identifier is {n} bytes, max {}", PathSegment::MAX_LEN)
            }
            SegmentError::IllegalChar(c) => {
                write!(f, "identifier contains {c:?}; only [A-Za-z0-9_-] is allowed")
            }
            SegmentError::LeadingDash => write!(f, "identifier starts with '-'"),
            SegmentError::ReservedDeviceName => {
                write!(f, "identifier is a reserved Windows device name")
            }
        }
    }
}

impl std::error::Error for SegmentError {}

/// Windows device names. Reserved with *any* extension and case-insensitively,
/// but the alphabet already bans `.`, so a plain stem comparison is complete
/// here.
pub(crate) const RESERVED_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Longest accepted identifier. The real shapes top out at 36 bytes (a Claude
/// session UUID), so this is headroom rather than a fit — it exists to keep a
/// hostile id from pushing a path past a filesystem limit.
pub const MAX_SEGMENT_LEN: usize = 64;

/// The one gate, as a free function, so [`PathSegment`] and
/// [`GroupId`](crate::groupid::GroupId) share the *checks* without either
/// having to be expressible as the other.
///
/// Returns the borrowed input on success rather than an owned copy: `GroupId`
/// and `PathSegment` each allocate their own, and a shared constructor that
/// allocated first would make one of them allocate twice.
pub fn check_segment(s: &str) -> Result<(), SegmentError> {
    if s.is_empty() {
        return Err(SegmentError::Empty);
    }
    if s.len() > MAX_SEGMENT_LEN {
        return Err(SegmentError::TooLong(s.len()));
    }
    if let Some(c) = s
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(SegmentError::IllegalChar(c));
    }
    if s.starts_with('-') {
        return Err(SegmentError::LeadingDash);
    }
    // `eq_ignore_ascii_case` rather than allocating a lowercase copy: the
    // alphabet is ASCII by the check above, so case folding is byte-local.
    if RESERVED_DEVICE_NAMES
        .iter()
        .any(|r| s.eq_ignore_ascii_case(r))
    {
        return Err(SegmentError::ReservedDeviceName);
    }
    Ok(())
}

/// A validated single path component.
///
/// The only constructor is [`PathSegment::parse`]. Holding one is proof the
/// string inside names exactly one child of whatever directory it is used
/// under — never a sibling, never an ancestor, never a device.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathSegment(String);

impl PathSegment {
    /// Longest accepted identifier. See [`MAX_SEGMENT_LEN`].
    pub const MAX_LEN: usize = MAX_SEGMENT_LEN;

    /// The one gate. Every `PathSegment` in the process came through here.
    pub fn parse(s: &str) -> Result<Self, SegmentError> {
        check_segment(s)?;
        Ok(PathSegment(s.to_string()))
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

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Deref for PathSegment {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for PathSegment {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Lets a `HashMap<PathSegment, _>` be probed with a `&str` without minting one
/// just to look it up.
impl Borrow<str> for PathSegment {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for PathSegment {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for PathSegment {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<PathSegment> for str {
    fn eq(&self, other: &PathSegment) -> bool {
        self == other.0.as_str()
    }
}

impl<'a> PartialEq<PathSegment> for &'a str {
    fn eq(&self, other: &PathSegment) -> bool {
        *self == other.0.as_str()
    }
}

impl PartialEq<String> for PathSegment {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl PartialEq<PathSegment> for String {
    fn eq(&self, other: &PathSegment) -> bool {
        self == &other.0
    }
}

impl FromStr for PathSegment {
    type Err = SegmentError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PathSegment::parse(s)
    }
}

impl TryFrom<&str> for PathSegment {
    type Error = SegmentError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        PathSegment::parse(s)
    }
}

impl TryFrom<String> for PathSegment {
    type Error = SegmentError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        PathSegment::parse(&s)
    }
}

/// Transparent on the wire: a `PathSegment` serializes as the bare string, so
/// no persisted file or frontend payload changes shape.
impl Serialize for PathSegment {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.0)
    }
}

/// Validating on the way *in*: a state file edited by hand — or written by an
/// older build, or by anything at all — cannot smuggle an unchecked id past the
/// constructor. Deserialization is a construction site like any other.
impl<'de> Deserialize<'de> for PathSegment {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        PathSegment::parse(&s).map_err(de::Error::custom)
    }
}
