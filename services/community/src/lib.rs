//! A community server: channels that the people let in, and nobody else, read and
//! post in.
//!
//! - [`community`]: community/1.0, the protocol members' apps speak to it.
//! - [`questions`]: answering a question a post asks, as a workflow/1.0 form.
//! - [`voice`]: community-voice/1.0, voice channels: rooms, keys, invitations.
//! - [`store`]: who is let in, the channels, and what was said.
//! - [`server`]: the endpoint, and sending what is posted to everyone who joined.
//! - [`web`]: the community on the web, signing in to it, and the community as
//!   an identity provider.
//!
//! It knows nothing of who its members are or what they are for. Something that
//! posts on a program's behalf (a bot) is a member like any other.

pub mod address_banner;
pub mod appeals;
pub mod admin_flows;
pub mod announcements;
pub mod assistant;
pub mod automod;
pub mod board;
pub mod bots;
pub mod claims;
pub mod discord;
pub mod discord_history;
pub mod events;
pub mod filters;
pub mod forms;
pub mod community;
pub mod clients;
pub mod config;
pub mod expressions;
pub mod files;
pub mod gates;
pub mod member_ids;
pub mod moderation;
pub mod outgoing;
pub mod posting;
pub mod protection;
pub mod questions;
pub mod roles;
pub mod serve;
pub mod server;
pub mod store;
pub mod structure;
pub mod voice;
pub mod webhook_formats;
pub mod webhooks;
pub mod welcome;
pub mod wiki;
pub mod web;
