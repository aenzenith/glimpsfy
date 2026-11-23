//! Glimpsfy's local MCP server: an AI agent on this Mac asks for a window, a display, an area or
//! the text in an area, and the user approves every request before anything is sent.
//!
//! - `rpc`: JSON-RPC 2.0 lines and the protocol versions.
//! - `tools`: the tools, their arguments and their results.
//! - `gate`: the approval gate, the queue, timeouts and timed permissions.
//! - `server`: one connection's session, with the app behind the [`server::Host`] trait.
//! - `peer`: the per-user socket and the checks on whoever connects (macOS).
//! - `bridge`: `glimpsfy mcp`, which joins an agent's stdio to the socket.

pub mod bridge;
pub mod gate;
#[cfg(target_os = "macos")]
pub mod peer;
pub mod rpc;
pub mod server;
pub mod tools;
