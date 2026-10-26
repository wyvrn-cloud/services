//! A community server: channels that the people let in, and nobody else, read and
//! post in.
//!
//! - [`community`]: community/1.0, the protocol members' apps speak to it.
//! - [`questions`]: answering a question a post asks, as a workflow/1.0 form.
//! - [`voice`]: community-voice/1.0, voice channels: rooms, keys, invitations.
//! - [`store`]: who is let in, the channels, and what was said.
//! - [`server`]: the endpoint, and sending what is posted to everyone who joined.
//!
//! It knows nothing of who its members are or what they are for. Something that
//! posts on a program's behalf (a bot) is a member like any other.

pub mod admin_flows;
pub mod assistant;
pub mod community;
pub mod config;
pub mod questions;
pub mod roles;
pub mod serve;
pub mod server;
pub mod store;
pub mod voice;
