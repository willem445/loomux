//! The remote loomux engine daemon — the skeleton (#888, plan-463 slice C1a).
//!
//! # What this crate is for
//!
//! Today the Tauri webview and the orchestration engine are one process. #888
//! splits them: the engine (PTYs, agents, groups, state, timers) runs on a
//! server, and local loomux becomes a display client. `loomux-engine` is the
//! Tauri-free core that split needs; **this crate is the process that will
//! host it**.
//!
//! # What this crate is NOT, yet
//!
//! It has no listener, no protocol, no engine hosting. It starts, reads a
//! config, decides whether a listener would be allowed to bind the address it
//! was given, prints that, and stops. The running order is in
//! `doc/design/remote-engine-protocol.md` §13; the two slices either side of
//! this one are:
//!
//! - **C2** — the WebSocket listener: the actual bind, the `Origin` refusal on
//!   upgrade (§1.2's second control), the hello frame, and RPC dispatch
//!   restricted to the classified command roster (§5).
//! - **A4** — the registry move, which is what gives this crate an engine to
//!   own. Until it lands there is no `OrchRegistry` on this side of the Tauri
//!   boundary to host.
//!
//! # The one thing it does decide
//!
//! [`config::ServerConfig::parse`] **refuses to load a config** that names a
//! routable bind address unless it also says `allow_routable_bind: true`. That
//! is v1 requirement 1 of `remote-engine-protocol.md` §1.2, and it is here
//! rather than in C2 because it is a statement about which config files are
//! valid: it needs no socket to decide, and a `ServerConfig` that has not
//! passed it does not exist. C2 must not re-implement it — see
//! `doc/design/remote-engine-daemon.md` §3.
//!
//! **Read §1.3 of the protocol note before running this anywhere.** The v1
//! daemon has no authentication by deliberate decision (H1), and its entire
//! trust boundary is reachability: a loopback bind reached over SSH from the
//! workstation it runs on.

pub mod cli;
pub mod config;
