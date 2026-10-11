//! What a name is, and which ones are set aside.

use std::collections::HashSet;

/// A name: one or more labels. `alice`, `official:announcements`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Name(Vec<String>);

/// 3 to 32 of `a`-`z`, `0`-`9` and `-`; no hyphen first, last or doubled. Nothing
/// else, so that two names that look alike are alike.
pub fn is_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    (3..=32).contains(&bytes.len())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && bytes[0] != b'-'
        && bytes[bytes.len() - 1] != b'-'
        && !label.contains("--")
}

impl Name {
    /// `labels` joined by `:`, of which there may be at most `max_labels`.
    pub fn parse(name: &str, max_labels: usize) -> Option<Self> {
        let labels: Vec<String> = name.split(':').map(str::to_string).collect();
        (!labels.is_empty() && labels.len() <= max_labels && labels.iter().all(|l| is_label(l))).then_some(Self(labels))
    }

    /// As it is in a DID, and in a message: `official:announcements`.
    pub fn as_did_path(&self) -> String {
        self.0.join(":")
    }

    /// As it is in a URL: `official/announcements`.
    pub fn as_url_path(&self) -> String {
        self.0.join("/")
    }

    pub fn labels(&self) -> &[String] {
        &self.0
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_did_path())
    }
}

/// The names a host sets aside by configuration: claimable only by whoever an
/// admin assigns them to. (An admin can set aside more, one at a time; those are
/// in the store.)
#[derive(Debug, Clone, Default)]
pub struct Reserved {
    names: HashSet<String>,
    namespaces: Vec<Vec<String>>,
}

impl Reserved {
    /// `names` are whole names; `namespaces` are leading labels (`official`,
    /// `teams:core`) under which every name is reserved.
    pub fn new(names: &[String], namespaces: &[String]) -> Self {
        Self {
            names: names.iter().cloned().collect(),
            namespaces: namespaces.iter().map(|n| n.split(':').map(str::to_string).collect()).collect(),
        }
    }

    pub fn covers(&self, name: &Name) -> bool {
        self.names.contains(&name.as_did_path())
            || self.namespaces.iter().any(|namespace| {
                // The namespace itself is reserved too: `official` as well as `official:x`.
                name.labels().len() >= namespace.len() && name.labels()[..namespace.len()] == namespace[..]
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_plain_and_unambiguous() {
        for good in ["alice", "a-b", "abc", "x1-2y", &"a".repeat(32)] {
            assert!(is_label(good), "{good}");
        }
        for bad in ["", "ab", "-ab", "ab-", "a--b", "Alice", "al.ice", "al ice", "alíce", "аlice", &"a".repeat(33)] {
            assert!(!is_label(bad), "{bad}");
        }
    }

    #[test]
    fn a_name_is_labels_and_only_so_many() {
        assert_eq!(Name::parse("alice", 2).unwrap().as_url_path(), "alice");
        assert_eq!(Name::parse("official:news", 2).unwrap().as_url_path(), "official/news");
        assert!(Name::parse("a:b:c", 2).is_none());
        assert!(Name::parse("official:", 2).is_none());
        assert!(Name::parse("official:*", 2).is_none());
        assert!(Name::parse("", 2).is_none());
    }

    #[test]
    fn a_namespace_reserves_everything_under_it() {
        let reserved = Reserved::new(&["admin".to_string()], &["official".to_string()]);
        let name = |n: &str| Name::parse(n, 3).unwrap();
        assert!(reserved.covers(&name("admin")));
        assert!(reserved.covers(&name("official")));
        assert!(reserved.covers(&name("official:news")));
        assert!(!reserved.covers(&name("officially")));
        assert!(!reserved.covers(&name("alice")));
        assert!(!reserved.covers(&name("alice:official")));
    }
}
