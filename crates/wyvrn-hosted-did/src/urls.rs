//! Where a DID's files are.

/// A `did:web` or `did:webvh`, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// For a `did:webvh`, the hash its log has to match.
    pub scid: Option<String>,
    /// The host, with its port if it names one: `id.example`, `id.example:8443`.
    pub host: String,
    /// The path segments after the host: `["alice"]`, `["official", "news"]`.
    pub path: Vec<String>,
}

impl Location {
    pub fn of(did: &str) -> Option<Self> {
        let (scid, rest) = if let Some(rest) = did.strip_prefix("did:webvh:") {
            let (scid, rest) = rest.split_once(':')?;
            (Some(scid.to_string()), rest)
        } else {
            (None, did.strip_prefix("did:web:")?)
        };
        // Nothing of a DID URL's: no path, query or fragment.
        if rest.is_empty() || rest.contains(['/', '?', '#']) {
            return None;
        }
        let mut parts = rest.split(':');
        // A port is written `%3A` in the host, since `:` separates the parts.
        let host = parts.next()?.replace("%3A", ":").replace("%3a", ":");
        let path: Vec<String> = parts.map(str::to_string).collect();
        (!host.is_empty() && path.iter().all(|segment| !segment.is_empty())).then_some(Self { scid, host, path })
    }

    /// Whether another DID is at the same host and path: a `did:web` and the
    /// `did:webvh` it is the shadow of.
    pub fn same_place(&self, other: &Self) -> bool {
        self.host == other.host && self.path == other.path
    }

    fn base(&self) -> String {
        let mut url = format!("https://{}", self.host);
        for segment in &self.path {
            url.push('/');
            url.push_str(segment);
        }
        url
    }

    /// `did.json`: under `.well-known` for a DID that is only a host.
    pub fn web_document(&self) -> String {
        if self.path.is_empty() {
            format!("https://{}/.well-known/did.json", self.host)
        } else {
            format!("{}/did.json", self.base())
        }
    }

    /// `did.jsonl`: under `.well-known` for a DID that is only a host.
    pub fn log(&self) -> String {
        if self.path.is_empty() {
            format!("https://{}/.well-known/did.jsonl", self.host)
        } else {
            format!("{}/did.jsonl", self.base())
        }
    }

    /// `whois.vp`: beside the log, and never under `.well-known`.
    pub fn whois(&self) -> String {
        format!("{}/whois.vp", self.base())
    }

    /// `https://host`, to tell whether another URL is the same origin.
    pub fn origin(&self) -> String {
        format!("https://{}", self.host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_did_says_where_its_files_are() {
        let alice = Location::of("did:webvh:QmScid:id.example:alice").unwrap();
        assert_eq!(alice.scid.as_deref(), Some("QmScid"));
        assert_eq!(alice.log(), "https://id.example/alice/did.jsonl");
        assert_eq!(alice.whois(), "https://id.example/alice/whois.vp");
        let web = Location::of("did:web:id.example:alice").unwrap();
        assert_eq!(web.web_document(), "https://id.example/alice/did.json");
        assert!(web.same_place(&alice));
        assert!(!web.same_place(&Location::of("did:web:id.example:alicia").unwrap()));
        assert!(!web.same_place(&Location::of("did:web:other.example:alice").unwrap()));

        let news = Location::of("did:web:id.example%3A8443:official:news").unwrap();
        assert_eq!(news.web_document(), "https://id.example:8443/official/news/did.json");

        let root = Location::of("did:web:example.com").unwrap();
        assert_eq!(root.web_document(), "https://example.com/.well-known/did.json");
        assert_eq!(Location::of("did:webvh:QmScid:example.com").unwrap().log(), "https://example.com/.well-known/did.jsonl");
        assert_eq!(root.whois(), "https://example.com/whois.vp");
    }

    #[test]
    fn what_is_not_a_web_did_has_no_place() {
        for not in ["did:peer:4zQm", "did:web:", "did:webvh:QmScid", "did:web:a.example:", "did:web:a.example::b", "did:web:a.example/x", "did:web:a.example#key", "did:web:a.example?x=1"] {
            assert!(Location::of(not).is_none(), "{not}");
        }
    }
}
