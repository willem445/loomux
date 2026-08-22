//! The daemon's config file, and the one decision it makes fail-closed.
//!
//! `remote-engine-protocol.md` §1.2 states two v1 requirements that are what
//! make "reach the daemon over an SSH tunnel" a trust boundary instead of a
//! hope. The second (`Origin` refusal on the WebSocket upgrade) belongs to the
//! listener, which does not exist yet. The first is this file:
//!
//! > The listener binds loopback (or a unix socket) and **refuses a routable
//! > interface** unless an explicit, loudly-named flag says otherwise.
//!
//! # Why the refusal is config-layer validation, not a piece of the listener
//!
//! It is enforced **when the config is parsed**, not when a socket is opened:
//! [`ServerConfig::parse`] returns [`ConfigError::RoutableBindRefused`] and no
//! config exists at all. That is what makes it C1a's rather than a fragment of
//! C2 smuggled forward — it is a statement about which files are valid, it
//! needs no socket to decide, and every test below runs without one.
//!
//! The shape follows `GroupId`'s (#904), which is this repo's precedent for
//! exactly this: **the unsafe state is unrepresentable rather than merely
//! checked.** [`ServerConfig`] holds an already-classified [`ListenTarget`],
//! and the only way to obtain one is through the gate. The `Deserialize` impl
//! is on a *private* raw shape, so a config file cannot deserialize its way
//! past the check either. C2 therefore receives a value the check has already
//! passed rather than a string it must remember to check — the refusal cannot
//! be forgotten by the slice that has the most else going on, and C2 must not
//! re-implement it (recorded in `remote-engine-protocol.md` §1.2 and §13).
//!
//! Nothing here opens a socket, resolves a name or touches a directory.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The default listen port. Unregistered and arbitrary — what is deliberate is
/// the host below.
pub const DEFAULT_PORT: u16 = 8788;

/// The default listen address when the config file names none.
///
/// A default of `0.0.0.0` is the café failure `remote-engine-protocol.md` §1.2
/// describes: it works exactly as well as loopback right up until it is
/// catastrophic, and nothing about the daemon's behaviour tells the operator
/// which one they got. Kept as a string because it is also what an operator
/// copies into a config file; `default_config_matches_the_documented_default`
/// pins it against the value [`ServerConfig::default`] actually uses.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8788";

/// The scheme prefix that names a unix-domain socket in a `listen:` value.
const UNIX_PREFIX: &str = "unix:";

/// Where a listener would bind — a value that has already passed the §1.2
/// check, which is the only way one can be obtained from a config.
///
/// `Routable` is a variant rather than an error case because the operator can
/// legitimately ask for it (§1.2's "explicit, loudly-named flag") and the
/// daemon then has to be able to say so loudly, every time it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenTarget {
    /// A loopback IP literal — the v1 default and the shape §1.2 assumes.
    Loopback(SocketAddr),
    /// A unix-domain socket path. Classified on every platform: classification
    /// is pure string work, and whether the host can actually bind one is the
    /// listener's error to report, not a reason for a config parser to behave
    /// differently per OS.
    Unix(PathBuf),
    /// A non-loopback IP literal, including the wildcards `0.0.0.0` and `::`.
    /// Only reachable through `allow_routable_bind`.
    Routable(SocketAddr),
}

impl ListenTarget {
    /// True when this target is reachable from off the machine — the property
    /// `allow_routable_bind` gates and the startup banner shouts about.
    pub fn is_routable(&self) -> bool {
        matches!(self, ListenTarget::Routable(_))
    }

    /// How the target is spelled back to the operator in the startup summary.
    pub fn describe(&self) -> String {
        match self {
            ListenTarget::Loopback(addr) => format!("{addr} (loopback)"),
            ListenTarget::Unix(path) => format!("unix:{} (unix socket)", path.display()),
            ListenTarget::Routable(addr) => format!("{addr} (ROUTABLE)"),
        }
    }
}

/// Everything that can make a config invalid.
///
/// One closed enum rather than `String`s, because the CLI maps these onto exit
/// codes and a caller that has to string-match a message is a caller that
/// silently stops branching when the wording is improved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The named config file could not be read.
    Read { path: PathBuf, msg: String },
    /// The file was read but is not a valid config — including an unknown key.
    Parse { path: PathBuf, msg: String },
    /// The `listen:` value is not a form this daemon understands.
    Listen { value: String, msg: String },
    /// A routable address was asked for without `allow_routable_bind: true`.
    RoutableBindRefused { addr: SocketAddr },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read { path, msg } => {
                write!(f, "cannot read config file {}: {msg}", path.display())
            }
            ConfigError::Parse { path, msg } => {
                write!(f, "invalid config file {}: {msg}", path.display())
            }
            ConfigError::Listen { value, msg } => {
                write!(f, "invalid `listen:` value {value:?}: {msg}")
            }
            // The message names the flag, because an error that says only "not
            // allowed" sends the operator to the source to find out how.
            ConfigError::RoutableBindRefused { addr } => write!(
                f,
                "refusing to bind {addr}: it is reachable from other machines, and this daemon has \
                 no authentication (see doc/design/remote-engine-protocol.md §1.2/§1.3). Bind a \
                 loopback address or a unix socket and reach it over SSH. If you genuinely mean to \
                 expose it, set `allow_routable_bind: true` in the config file"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The config file as it is written on disk.
///
/// **Private on purpose.** It is the only `Deserialize` in this module, and
/// keeping it unexported is what makes [`ServerConfig`] the sole public form —
/// a validated one. A `Deserialize` on the public type would be a second door
/// into the same state with no gate on it, which is precisely the shape #904
/// closed for `GroupId` by routing its `Deserialize` back through `parse`.
///
/// `deny_unknown_fields` is the same choice `workflow.rs` makes for
/// `.loomux/workflow.yml`, and for the same reason: this is a hand-edited file
/// on the machine, so a key nobody recognises is a typo, and a typo that is
/// silently ignored is a setting the operator believes is in force. That is
/// the OPPOSITE of the wire rule in `remote-engine-protocol.md` §4.4 ("both
/// sides ignore what they do not know"), which is right for two independently
/// updated peers and wrong for one local file — see
/// `doc/design/remote-engine-daemon.md` for why the two do not conflict.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServerConfig {
    /// `127.0.0.1:8788`, `[::1]:8788`, or `unix:/run/loomux/engine.sock`.
    #[serde(default = "default_listen")]
    listen: String,
    /// §1.2's "explicit, loudly-named flag". Default false, and the default is
    /// the security property.
    #[serde(default)]
    allow_routable_bind: bool,
    /// Overrides the engine's `obs::data_root()` for persisted state.
    #[serde(default)]
    state_root: Option<PathBuf>,
}

fn default_listen() -> String {
    DEFAULT_LISTEN.to_string()
}

/// A config that has been read AND validated: its listen target is one this
/// daemon is allowed to bind, and there is no way to construct one for which
/// that is untrue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    listen: ListenTarget,
    state_root: Option<PathBuf>,
}

impl Default for ServerConfig {
    /// The config a daemon run with no `--config` uses: loopback, nothing
    /// exposed. Built from the constant rather than by parsing, so `Default`
    /// cannot fail and there is no `expect` in the no-config path.
    fn default() -> Self {
        ServerConfig {
            listen: ListenTarget::Loopback(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                DEFAULT_PORT,
            )),
            state_root: None,
        }
    }
}

impl ServerConfig {
    /// Parse and validate config text. `path` is carried only so the error can
    /// name the file the operator has to go and edit.
    ///
    /// This is the gate. A config naming a routable address without
    /// `allow_routable_bind: true` does not load — it is not a config the
    /// daemon holds and then declines to serve.
    pub fn parse(text: &str, path: &Path) -> Result<ServerConfig, ConfigError> {
        // An empty (or comments-only) file is a legitimate "everything
        // default" config, and YAML deserializes it as null rather than as an
        // empty mapping — so serde would reject it with an "invalid type: unit
        // value" that reads like a syntax error. Answering it here keeps the
        // obvious way to start a config file from looking broken.
        if text.trim().is_empty() {
            return Ok(ServerConfig::default());
        }
        let raw: RawServerConfig =
            serde_norway::from_str(text).map_err(|e| ConfigError::Parse {
                path: path.to_path_buf(),
                msg: e.to_string(),
            })?;
        ServerConfig::from_raw(raw)
    }

    /// Read, parse and validate a config file.
    pub fn load(path: &Path) -> Result<ServerConfig, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
            path: path.to_path_buf(),
            msg: e.to_string(),
        })?;
        ServerConfig::parse(&text, path)
    }

    /// §1.2's first control, and the only place it is applied.
    fn from_raw(raw: RawServerConfig) -> Result<ServerConfig, ConfigError> {
        // Note which way this fails: a value that cannot be classified is an
        // error, never a fallback to the default. Silently binding loopback
        // when the operator asked for something else would be safe here, but
        // it is the habit that makes the opposite mistake somewhere else — and
        // it would hide a typo in the one field where a typo matters most.
        let listen = classify_listen(&raw.listen)?;
        if let ListenTarget::Routable(addr) = listen {
            if !raw.allow_routable_bind {
                return Err(ConfigError::RoutableBindRefused { addr });
            }
        }
        Ok(ServerConfig {
            listen,
            state_root: raw.state_root,
        })
    }

    /// Where a listener may bind. Already checked — see [`ServerConfig::parse`].
    pub fn listen(&self) -> &ListenTarget {
        &self.listen
    }

    /// Where persisted orchestration state lives for this daemon.
    ///
    /// Not `Result`, and it touches no disk: creating or validating the root
    /// belongs to whoever first writes under it, and a config check that
    /// created directories as a side effect would be a surprising thing for
    /// `--check-config` to do.
    ///
    /// That is also why the daemon does **not** call `obs::init_data_root()`
    /// here (#1153 phase 4): the `loomux`→`orrerix` move is a real filesystem
    /// rename, and `--check-config` is the one invocation that must be free of
    /// side effects. The default falls back to whichever root already exists,
    /// so a daemon on a pre-rename machine still finds its state. When this
    /// crate grows an actual serve loop, `init_data_root()` belongs at the top
    /// of it, next to where the desktop app calls it — see
    /// `doc/design/rebrand-filesystem.md`.
    pub fn state_root(&self) -> PathBuf {
        match &self.state_root {
            Some(root) => root.clone(),
            None => loomux_engine::obs::data_root(),
        }
    }
}

/// Classify a `listen:` value without resolving anything.
///
/// **No DNS, deliberately.** A hostname would have to be resolved to know
/// whether it is loopback, the answer could differ between the check and the
/// bind, and a resolver that returns a routable address for a name the
/// operator believed was local would launder §1.2's control into a lookup. So
/// an IP literal is required, and a name is refused with that said out loud.
///
/// **Private — the tightest visibility that compiles**, and not merely tidiness.
/// It is the one function that hands back a bare `ListenTarget::Routable`
/// without consulting `allow_routable_bind`, so exporting it would leave "C2
/// must not re-derive the address from the config text"
/// (`remote-engine-protocol.md` §1.2) enforced by prose where the rest of this
/// module is enforced by the compiler. Callers outside this module get a target
/// only by going through [`ServerConfig::parse`], which is the gate. Making the
/// variant payloads themselves unforgeable is a C2-slice decision, not this
/// one's — but nothing here needs to be reachable for C2 to do that.
fn classify_listen(value: &str) -> Result<ListenTarget, ConfigError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ConfigError::Listen {
            value: value.to_string(),
            msg: "empty".to_string(),
        });
    }

    if let Some(path) = value.strip_prefix(UNIX_PREFIX) {
        if path.trim().is_empty() {
            return Err(ConfigError::Listen {
                value: value.to_string(),
                msg: "`unix:` needs a socket path".to_string(),
            });
        }
        return Ok(ListenTarget::Unix(PathBuf::from(path)));
    }

    let addr: SocketAddr = value.parse().map_err(|_| ConfigError::Listen {
        value: value.to_string(),
        msg: "expected `<ip>:<port>` with an IP LITERAL (`127.0.0.1:8788`, `[::1]:8788`) or \
              `unix:<path>`. Host names are not resolved: whether a name is loopback is a DNS \
              answer that can change between the check and the bind"
            .to_string(),
    })?;

    if addr.ip().is_loopback() {
        Ok(ListenTarget::Loopback(addr))
    } else {
        // Includes the wildcards `0.0.0.0` and `::`, which are the single most
        // likely thing to be typed here and are reachable from every interface
        // the machine has.
        Ok(ListenTarget::Routable(addr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(yaml: &str) -> Result<ServerConfig, ConfigError> {
        ServerConfig::parse(yaml, Path::new("loomux-server.yml"))
    }

    // ---- §1.2 control 1: a routable bind is refused AT CONFIG LOAD ----

    #[test]
    fn a_config_naming_a_routable_address_does_not_load() {
        // The café case from remote-engine-protocol.md §1.2, and the one an
        // operator is most likely to type because every other daemon accepts
        // it. The daemon has NO authentication, so this is the whole boundary
        // — and it is enforced here, with no socket in sight: the config is
        // INVALID, not merely unserved.
        for value in [
            "0.0.0.0:8788",
            "[::]:8788",
            "192.168.1.5:8788",
            "10.0.0.7:22000",
        ] {
            match cfg(&format!("listen: \"{value}\"")) {
                Err(ConfigError::RoutableBindRefused { .. }) => {}
                other => panic!("{value} must be refused without allow_routable_bind, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_refusal_message_names_the_flag_that_lifts_it() {
        let err = cfg("listen: \"0.0.0.0:8788\"").expect_err("refused");
        let msg = err.to_string();
        assert!(
            msg.contains("allow_routable_bind"),
            "the refusal must name the flag, or the operator goes to the source to find it: {msg}"
        );
    }

    #[test]
    fn the_flag_lifts_the_refusal_and_the_target_still_says_it_is_routable() {
        let c = cfg("listen: \"0.0.0.0:8788\"\nallow_routable_bind: true").expect("explicitly allowed");
        assert!(
            c.listen().is_routable(),
            "an allowed routable bind must still be MARKED routable — the startup banner and \
             every later slice's warning key off this, not off the config flag"
        );
        assert!(c.listen().describe().contains("ROUTABLE"));
    }

    #[test]
    fn loopback_and_unix_need_no_flag() {
        for value in [
            "127.0.0.1:8788",
            "127.0.0.2:1",
            "[::1]:8788",
            "unix:/run/loomux/engine.sock",
        ] {
            let c = cfg(&format!("listen: \"{value}\""))
                .unwrap_or_else(|e| panic!("{value} must be allowed by default: {e}"));
            assert!(!c.listen().is_routable(), "{value}");
        }
    }

    #[test]
    fn the_flag_is_inert_for_a_loopback_bind() {
        // Setting the flag must not itself change where the daemon listens —
        // it removes a refusal, it does not select an address.
        let c = cfg("listen: \"127.0.0.1:8788\"\nallow_routable_bind: true").expect("parses");
        assert_eq!(
            c.listen(),
            &ListenTarget::Loopback("127.0.0.1:8788".parse().unwrap())
        );
    }

    #[test]
    fn the_default_config_binds_loopback() {
        assert!(
            !ServerConfig::default().listen().is_routable(),
            "a daemon run with no config at all must not be reachable from the network"
        );
    }

    #[test]
    fn default_config_matches_the_documented_default() {
        // `Default` is built from the parts and `DEFAULT_LISTEN` is the string
        // an operator copies into a file; nothing but this test stops the two
        // from drifting apart.
        assert_eq!(
            ServerConfig::default().listen(),
            &classify_listen(DEFAULT_LISTEN).expect("the documented default must classify")
        );
    }

    // ---- classification edges ----

    #[test]
    fn a_host_name_is_refused_rather_than_resolved() {
        // Whether `localhost` is loopback is a DNS answer, and one that can
        // differ between this check and the bind. Refusing is the fail-closed
        // reading; resolving would put §1.2's control behind a lookup.
        for value in ["localhost:8788", "my-server:8788", "example.com:80"] {
            match cfg(&format!("listen: \"{value}\"")) {
                Err(ConfigError::Listen { .. }) => {}
                other => panic!("{value} must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_malformed_listen_value_is_an_error_not_a_silent_default() {
        for value in ["", "   ", "8788", "127.0.0.1", "unix:", "unix:   "] {
            match classify_listen(value) {
                Err(ConfigError::Listen { .. }) => {}
                other => panic!("{value:?} must be an error, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_unix_target_keeps_its_path_verbatim() {
        assert_eq!(
            classify_listen("unix:/run/loomux/engine.sock").expect("classified"),
            ListenTarget::Unix(PathBuf::from("/run/loomux/engine.sock"))
        );
    }

    // ---- the config file itself ----

    #[test]
    fn an_unknown_key_fails_loudly() {
        // The typo case, and the reason it matters here more than elsewhere:
        // `allow_routable_bnd: true` silently ignored is a setting the
        // operator believes is in force. It happens to fail CLOSED, but the
        // same silence over a mistyped `listen:` would bind the default while
        // the file says otherwise.
        let err = cfg("allow_routable_bnd: true").expect_err("unknown key must be rejected");
        assert!(matches!(err, ConfigError::Parse { .. }), "got {err:?}");
        assert!(
            err.to_string().contains("allow_routable_bnd"),
            "the error must name the offending key: {err}"
        );
    }

    #[test]
    fn an_empty_config_file_is_the_default_config() {
        for text in ["", "\n\n", "# just a comment\n"] {
            assert_eq!(
                cfg(text).unwrap_or_else(|e| panic!("{text:?} must parse: {e}")),
                ServerConfig::default(),
                "an empty or comments-only file is a legitimate all-defaults config"
            );
        }
    }

    #[test]
    fn a_partial_config_keeps_the_defaults_for_what_it_omits() {
        let c = cfg("state_root: /var/lib/loomux").expect("parses");
        assert_eq!(c.listen(), ServerConfig::default().listen());
        assert_eq!(c.state_root(), PathBuf::from("/var/lib/loomux"));
    }

    #[test]
    fn the_state_root_override_wins_over_the_engine_data_root() {
        assert_eq!(
            cfg("state_root: /var/lib/loomux").expect("parses").state_root(),
            PathBuf::from("/var/lib/loomux")
        );
        // And with no override the daemon defers to the engine rather than
        // inventing a second opinion about where state lives.
        assert_eq!(
            ServerConfig::default().state_root(),
            loomux_engine::obs::data_root()
        );
    }
}
