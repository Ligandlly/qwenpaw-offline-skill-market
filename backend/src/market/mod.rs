//! The market is one machine with two object types.
//!
//! A skill and an agent are stored in their own tables and gated by their own
//! rules, but everything between "read a list of them" and "hand a version
//! back" is identical: the same 15 fields, the same visibility/withdrawal/
//! takedown precedence, the same `latest` resolution, the same download
//! counter. This module owns that middle so a change lands once instead of
//! twice — the two planes differ only in the data described by [`Entity`].
//!
//! Layered so each file answers one question:
//!
//! - [`entity`]: what differs between the planes (tables, columns, links).
//! - [`read`]: what the market shows (lists, status, bundles).
//! - [`write`]: what changes an entry (publish, withdraw, restore, takedown).
//! - [`validate`]: what a write would be refused for.

mod entity;
mod read;
mod validate;
mod write;

pub use entity::{Entity, Entry, Labels, AGENT, SKILL};
pub(crate) use read::{bundle, list_admin, list_owned, list_page, status};
pub(crate) use validate::{
    check_fields, inspect, lookup, require_fields, required_slug, resolve_slug, Answer, Problem,
};
pub(crate) use write::{change_state, listing, record_tenant, store, Change, Publish};
