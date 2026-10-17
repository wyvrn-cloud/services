//! wyvrn-stickers: hosts sticker and custom-emoji packs. Packs are made by
//! talking to it over DIDComm, as `https://didcomm.org/workflow/1.0` workflows
//! (it is that protocol's Processor), and fetched from it over HTTPS by anyone.
//! What a pack is, and how a sticker is sent, is `https://wyvrn.app/stickers/1.0`.
//!
//! - [`images`]: what an uploaded picture is, from its bytes.
//! - [`store`]: packs, their stickers, and workflows under way.
//! - [`templates`]: the workflows it offers, as `workflow/1.0` templates.
//! - [`render`]: a template's display hints, filled in for one instance.
//! - [`processor`]: the workflow protocol -- what each message does.
//! - [`server`]: the HTTP side: the endpoint messages come to, and serving packs.

// The templates are written out as JSON, several levels deep.
#![recursion_limit = "512"]

pub mod admin;
pub mod config;
pub mod images;
pub mod processor;
pub mod render;
pub mod server;
pub mod store;
pub mod templates;
