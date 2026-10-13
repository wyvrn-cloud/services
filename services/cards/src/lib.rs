//! Link cards: what a link to someone looks like where it is posted.
//!
//! A link the app hands out -- to a public address (`/dl/<did>`), or carrying a
//! contact invite (`/dl?_oob=…`) -- opens a page of the web app, which a person's
//! browser runs. A chat or a social network that is shown the link runs nothing:
//! it fetches the address once and reads the page's OpenGraph tags. This service
//! answers that fetch: the tags, and the picture they name, drawn from who the
//! link is to.
//!
//! - [`who`]: who a link is to -- an address looked up and checked as an app
//!   would, or an invite read for what it carries.
//! - [`render`]: the picture.
//! - [`server`]: the two pages and the picture, over HTTP.
//!
//! It holds nothing and signs nothing: it is not a DIDComm service, and has no
//! identity of its own. What it says of an address is what anyone who looked it
//! up would find.

pub mod config;
pub mod render;
pub mod server;
pub mod who;
