//! From a DID someone was given to the party it names, in the order the protocol
//! sets out, with the fetching left to the caller.

use std::future::Future;

use serde_json::Value;

use crate::urls::Location;
use crate::verify;
use crate::whois::{self, Profile};

/// Getting a file. The only part of reading a hosted DID that touches a network.
pub trait Fetch: Send + Sync {
    /// The bytes at `url`, or `None` if there is nothing there (404). An error is
    /// for not having been able to find out.
    fn get(&self, url: &str) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;
}

/// How far a DID can be trusted to be one party over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// A `did:webvh` whose log verified: its history is its holder's, whatever the
    /// host does.
    Verified,
    /// A `did:web` with no `did:webvh` behind it (or none that checked out): it is
    /// whatever its host says today.
    HostsWord,
}

/// The party a DID names.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The DID to know the party by from now on: the `did:webvh` where there is a
    /// verified one, whichever DID was asked for.
    pub did: String,
    /// The short name it also goes by, if it has one that checks out.
    pub did_web: Option<String>,
    /// The document of [`did`](Self::did).
    pub document: Value,
    pub standing: Standing,
    pub deactivated: bool,
}

async fn fetch_text(fetch: &impl Fetch, url: &str) -> Result<String, String> {
    let bytes = fetch.get(url).await?.ok_or_else(|| format!("nothing at {url}"))?;
    String::from_utf8(bytes).map_err(|_| format!("{url} is not text"))
}

async fn resolve_webvh(did: &str, fetch: &impl Fetch) -> Result<Resolved, String> {
    let place = Location::of(did).ok_or_else(|| format!("{did} is not a did:webvh"))?;
    let verified = verify::log(did, &fetch_text(fetch, &place.log()).await?).await?;
    // Its short name, if it names one in its own place.
    let did_web = verify::also_known_as(&verified.document)
        .into_iter()
        .find(|aka| aka.starts_with("did:web:") && Location::of(aka).is_some_and(|there| there.same_place(&place)))
        .map(str::to_string);
    Ok(Resolved { did: did.to_string(), did_web, document: verified.document, standing: Standing::Verified, deactivated: verified.deactivated })
}

/// Resolves a `did:web` or `did:webvh` to the party it names.
///
/// A `did:web` that names a `did:webvh` in its own place is followed to it: the
/// log is verified, and the pair accepted only if the `did:webvh` names the
/// `did:web` back. Then the party is known by the `did:webvh`. Otherwise the
/// `did:web` stands alone, on its host's word.
pub async fn resolve(did: &str, fetch: &impl Fetch) -> Result<Resolved, String> {
    if did.starts_with("did:webvh:") {
        return resolve_webvh(did, fetch).await;
    }
    let place = Location::of(did).ok_or_else(|| format!("{did} is not a did:web or did:webvh"))?;
    let web_document: Value = serde_json::from_str(&fetch_text(fetch, &place.web_document()).await?).map_err(|_| format!("{did}'s document is not JSON"))?;
    if web_document["id"].as_str() != Some(did) {
        return Err(format!("the document fetched for {did} is not its own"));
    }
    if let Some(behind) = verify::webvh_behind(did, &web_document) {
        match resolve_webvh(behind, fetch).await {
            Ok(mut resolved) if verify::names_back(&resolved.document, did) => {
                resolved.did_web = Some(did.to_string());
                return Ok(resolved);
            }
            // A did:webvh that doesn't verify, or doesn't own up to this did:web,
            // adds nothing: the did:web is on its own.
            Ok(_) | Err(_) => {}
        }
    }
    Ok(Resolved { did: did.to_string(), did_web: Some(did.to_string()), document: web_document, standing: Standing::HostsWord, deactivated: false })
}

/// Where a resolved DID's whois is: its `#whois` service if it declares one, else
/// (for a `did:webvh`, which has it implicitly) beside its log.
fn whois_url(resolved: &Resolved) -> Option<String> {
    let declared = resolved.document["service"].as_array().into_iter().flatten().find(|service| {
        service["type"] == "LinkedVerifiablePresentation" && service["id"].as_str().is_some_and(|id| id.ends_with("#whois"))
    });
    match declared {
        Some(service) => match &service["serviceEndpoint"] {
            Value::String(url) => Some(url.clone()),
            Value::Array(urls) => urls.first().and_then(Value::as_str).map(str::to_string),
            _ => None,
        },
        None if resolved.standing == Standing::Verified => Location::of(&resolved.did).map(|place| place.whois()),
        None => None,
    }
}

/// The profile a resolved DID publishes, checked against it. `None` if it
/// publishes none, or publishes it somewhere other than its own host: fetching it
/// there would tell a third party who is looking, which is the caller's to decide
/// and not done here.
pub async fn whois_of(resolved: &Resolved, fetch: &impl Fetch) -> Result<Option<Profile>, String> {
    let Some(url) = whois_url(resolved) else { return Ok(None) };
    let own_origin = Location::of(&resolved.did).map(|place| place.origin());
    if !own_origin.is_some_and(|origin| url.starts_with(&format!("{origin}/"))) {
        return Ok(None);
    }
    let Some(bytes) = fetch.get(&url).await? else { return Ok(None) };
    let presentation = whois::check(&bytes, &resolved.did, &resolved.document)?;
    Ok(whois::profile(&presentation, &resolved.did))
}
