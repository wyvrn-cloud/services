//! Reading a hosted DID: what `did-hosting/1.0` calls "Resolving a hosted DID" and
//! "The whois profile".
//!
//! Getting the files and trusting them are kept apart. Everything that decides
//! whether a DID is what it claims works on bytes it is handed:
//!
//! - [`urls`]: where a DID's files are.
//! - [`verify`]: a `did:webvh` log against its DID; a `did:web` and a `did:webvh`
//!   as one party; a `did:peer:4` as that party too.
//! - [`whois`]: a whois presentation against the DID that published it, and the
//!   profile in it.
//! - [`keys`]: the keys in a document, and a signature against them.
//!
//! [`resolve`] puts them in order around a [`Fetch`], which is the only part that
//! touches a network: an HTTPS client in an app, the host's own store in the host,
//! a map of prepared answers in a test.

pub mod keys;
pub mod resolve;
pub mod urls;
pub mod verify;
pub mod whois;

pub use resolve::{resolve, whois_of, Fetch, Resolved, Standing};
pub use whois::{Image, Profile};
