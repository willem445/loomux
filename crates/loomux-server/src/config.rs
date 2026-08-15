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
//! The refusal lives here, one slice ahead of the socket, because the config
//! schema has to name the listen address anyway — and an address field that
//! accepts `0.0.0.0` and leaves the refusal to a later slice is a half
//! contract whose unsafe value is representable, spellable and silently
//! honoured in the meantime. Deciding it at parse time also means C2 cannot
//! forget: there is no second place where a `ListenTarget` can be produced.
//!
//! Everything here is pure. No socket is opened, no name is resolved, no
//! directory is created — [`ServerConfig::resolve_listen`] answers "would this
//! be allowed?", and the answer is a value the caller has to look at.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The default listen address when the config file names none.
///
/// The port is unregistered and arbitrary; what is deliberate is the *host*.
/// A default of `0.0.0.0` is the café failure remote-engine-protocol.md §1.2
/// describes: it works exactly as well as loopback right up until it is
/// catastrophic, and nothing about the daemon's behaviour tells the operator
/// which one they got.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8788";

/// The scheme prefix that names a unix-domain socket in a `listen:` value.
const UNIX_PREFIX: &str = "unix:";

/// Where a listener would bind, once [`ServerConfig::resolve_listen`] has
/// allowed it.
///
/// `Routable` is a variant rather than an error case because the operator can
/// legitimately ask for it (§1.2's "explicit, loudly-named flag") and the
/// daemon then has to be able to say so loudly. A refusal is
/// [`ConfigError::RoutableBindRefused`]; this type is the allowed set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenTarget {
    /// A loopback IP literal — the v1 default and the shape §1.2 assumes.
    Loopback(SocketAddr),
    /// A unix-domain socket path. Accepted by this layer on every platform:
    /// classification is pure string work, and whether the host can actually
    /// bind one is the listener's problem to report, not a reason for the
    /// config parser to behave differently per OS.
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

/// Everything that can go wrong between "the operator ran the daemon" and "a
/// listener would be allowed to start".
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

/// The config file, as parsed.
///
/// `deny_unknown_fields` is the same choice `workflow.rs` makes for
/// `.loomux/workflow.yml`, and for the same reason: this is a hand-edited file
/// on the machine, so a key nobody recognises is a typo, and a typo that is
/// silently ignored is a setting the operator believes is in force. That is
/// the OPPOSITE of the wire rule in remote-engine-protocol.md §4.4 ("both
/// sides ignore what they do not know"), which is right for two independently
/// updated peers and wrong for one local file — see
/// doc/design/remote-engine-daemon.md for why the two rules do not conflict.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// `127.0.0.1:8788`, `[::1]:8788`, or `unix:/run/loomux/engine.sock`.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// §1.2's "explicit, loudly-named flag". Default false, and the default is
    /// the security property — see [`ServerConfig::resolve_listen`].
    #[serde(default)]
    pub allow_routable_bind: bool,
    /// Overrides the engine's `obs::data_root()` for every piece of persisted
    /// orchestration state. `None` means "wherever the desktop app would put
    /// it on this machine", which is what makes a daemon on a workstation see
    /// the groups that are already there.
    #[serde(default)]
    pub state_root: Option<PathBuf>,
}

fn default_listen() -> String {
    DEFAULT_LISTEN.to_string()
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: default_listen(),
            allow_routable_bind: false,
            state_root: None,
        }
    }
}

impl ServerConfig {
    /// Parse config text. `path` is carried only so the error can name the
    /// file the operator has to go and edit.
    pub fn parse(text: &str, path: &Path) -> Result<ServerConfig, ConfigError> {
        // An empty (or comments-only) file is a legitimate "everything
        // default" config, and YAML deserializes it as null rather than as an
        // empty mapping — so serde would reject it with an "invalid type: unit
        // value" that reads like a syntax error. Answering it here keeps the
        // obvious way to start a config file from looking broken.
        if text.trim().is_empty() {
            return Ok(ServerConfig::default());
        }
        serde_norway::from_str(text).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            msg: e.to_string(),
        })
    }

    /// Read and parse a config file.
    pub fn load(path: &Path) -> Result<ServerConfig, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
            path: path.to_path_buf(),
            msg: e.to_string(),
        })?;
        ServerConfig::parse(&text, path)
    }

    /// The listen target this config asks for, or the reason it is refused.
    ///
    /// This is §1.2's first control. Note which way it fails: a value that
    /// cannot be classified is an error, never a fallback to the default —
    /// silently binding loopback when the operator asked for something else
    /// would be safe here but is the habit that makes the opposite mistake
    /// somewhere else, and it would hide a typo in the one field where a typo
    /// matters most.
    pub fn resolve_listen(&self) -> Result<ListenTarget, ConfigError> {
        let target = classify_listen(&self.listen)?;
        match target {
            ListenTarget::Routable(addr) if !self.allow_routable_bind => {
                Err(ConfigError::RoutableBindRefused { addr })
            }
            other => Ok(other),
        }
    }

    /// Where persisted orchestration state lives for this daemon.
    ///
    /// Not `Result`, and it touches no disk: creating or validating the root
    /// belongs to whoever first writes under it, and a config check that
    /// created directories as a side effect would be a surprising thing for
    /// `--check-config` to do.
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
/// operator believed was local would launder §1.2's control into a lookup.
/// So an IP literal is required, and a name is refused with that said out loud.
pub fn classify_listen(value: &str) -> Result<ListenTarget, ConfigError> {
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

    // ---- §1.2 control 1: a routable bind is refused unless asked for ----

    #[test]
    fn a_wildcard_bind_is_refused_without_the_flag() {
        // The café case from remote-engine-protocol.md §1.2, and the one an
        // operator is most likely to type because every other daemon accepts
        // it: the daemon has NO authentication, so this is the whole boundary.
        for value in ["0.0.0.0:8788", "[::]:8788", "192.168.1.5:8788", "10.0.0.7:22000"] {
            let c = cfg(&format!("listen: \"{value}\"")).expect("parses");
            match c.resolve_listen() {
                Err(ConfigError::RoutableBindRefused { .. }) => {}
                other => panic!("{value} must be refused without allow_routable_bind, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_refusal_message_names_the_flag_that_lifts_it() {
        let c = cfg("listen: \"0.0.0.0:8788\"").expect("parses");
        let err = c.resolve_listen().expect_err("refused");
        let msg = err.to_string();
        assert!(
            msg.contains("allow_routable_bind"),
            "the refusal must name the flag, or the operator goes to the source to find it: {msg}"
        );
    }

    #[test]
    fn the_flag_lifts_the_refusal_and_the_target_still_says_it_is_routable() {
        let c = cfg("listen: \"0.0.0.0:8788\"\nallow_routable_bind: true").expect("parses");
        let target = c.resolve_listen().expect("explicitly allowed");
        assert!(
            target.is_routable(),
            "an allowed routable bind must still be MARKED routable — the startup banner and \
             every later slice's warning key off this, not off the config flag"
        );
        assert!(target.describe().contains("ROUTABLE"));
    }

    #[test]
    fn loopback_and_unix_need_no_flag() {
        for (value, expect_routable) in [
            ("127.0.0.1:8788", false),
            ("127.0.0.2:1", false),
            ("[::1]:8788", false),
            ("unix:/run/loomux/engine.sock", false),
        ] {
            let target = cfg(&format!("listen: \"{value}\""))
                .expect("parses")
                .resolve_listen()
                .unwrap_or_else(|e| panic!("{value} must be allowed by default: {e}"));
            assert_eq!(target.is_routable(), expect_routable, "{value}");
        }
    }

    #[test]
    fn the_flag_is_inert_for_a_loopback_bind() {
        // Setting the flag must not itself change where the daemon listens —
        // it removes a refusal, it does not select an address.
        let c = cfg("listen: \"127.0.0.1:8788\"\nallow_routable_bind: true").expect("parses");
        assert_eq!(
            c.resolve_listen().expect("allowed"),
            ListenTarget::Loopback("127.0.0.1:8788".parse().unwrap())
        );
    }

    #[test]
    fn the_default_config_binds_loopback() {
        let target = ServerConfig::default().resolve_listen().expect("allowed");
        assert!(
            !target.is_routable(),
            "a daemon run with no config at all must not be reachable from the network"
        );
    }

    // ---- classification edges ----

    #[test]
    fn a_host_name_is_refused_rather_than_resolved() {
        // Whether `localhost` is loopback is a DNS answer, and one that can
        // differ between this check and the bind. Refusing is the fail-closed
        // reading; resolving would put §1.2's control behind a lookup.
        for value in ["localhost:8788", "my-server:8788", "example.com:80"] {
            match classify_listen(value) {
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
        assert_eq!(c.listen, DEFAULT_LISTEN);
        assert!(!c.allow_routable_bind);
        assert_eq!(c.state_root, Some(PathBuf::from("/var/lib/loomux")));
    }

    #[test]
    fn the_state_root_override_wins_over_the_engine_data_root() {
        let c = cfg("state_root: /var/lib/loomux").expect("parses");
        assert_eq!(c.state_root(), PathBuf::from("/var/lib/loomux"));
        // And with no override the daemon defers to the engine rather than
        // inventing a second opinion about where state lives.
        assert_eq!(
            ServerConfig::default().state_root(),
            loomux_engine::obs::data_root()
        );
    }
}
