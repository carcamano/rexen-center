//! XenAPI classes, records, enums and calls.
//!
//! **Phase F1: hand-written subset.** Phase F2 replaces this whole module with
//! code generated from a pinned `xenapi.json` (see the port plan, §3.8). Keep
//! the shapes here generator-friendly:
//!
//! * one zero-sized marker type per class (`Vm`, `Host`, `Pool`) used in
//!   [`crate::Ref`];
//! * one `*Record` struct per class, `#[serde(default)]`, unknown fields ignored;
//! * enums declared with [`crate::xapi_enum!`] (tolerates unknown values);
//! * one module per class with free `async fn`s taking `&Session`.
//!
//! Nothing outside this module may depend on how these items are produced.

mod enums;
mod records;

pub mod host;
pub mod pool;
pub mod vm;

pub use enums::*;
pub use records::*;

/// Marker type for the `pool` class.
#[derive(Debug)]
pub enum Pool {}

/// Marker type for the `host` class.
#[derive(Debug)]
pub enum Host {}

/// Marker type for the `VM` class.
#[derive(Debug)]
pub enum Vm {}

/// Lenient serde helpers for fields whose wire type varies between servers.
pub(crate) mod de {
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    /// An integer that may arrive as a JSON number or a decimal string.
    pub fn int<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        Ok(match Value::deserialize(d)? {
            Value::Number(n) => n.as_i64().unwrap_or_default(),
            Value::String(s) => s.parse().unwrap_or_default(),
            _ => 0,
        })
    }
}
