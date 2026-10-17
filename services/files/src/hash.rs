//! A file's hash as `files/1.0` writes it: a multihash, base58btc-encoded, of
//! sha2-256 -- the same form a DIDComm attachment by reference carries in
//! `data.hash` (chat-message/1.0).

use sha2::{Digest, Sha256};

/// multihash's code for sha2-256, and the digest's length.
const SHA2_256: [u8; 2] = [0x12, 0x20];

/// A sha2-256 digest as a base58btc multihash.
pub fn multihash(digest: &[u8; 32]) -> String {
    let mut bytes = Vec::with_capacity(34);
    bytes.extend_from_slice(&SHA2_256);
    bytes.extend_from_slice(digest);
    bs58::encode(bytes).into_string()
}

/// The sha2-256 digest a base58btc multihash carries, if it is one of those.
pub fn digest_of(multihash: &str) -> Option<[u8; 32]> {
    let bytes = bs58::decode(multihash).into_vec().ok()?;
    if bytes.len() != 34 || bytes[..2] != SHA2_256 {
        return None;
    }
    bytes[2..].try_into().ok()
}

/// The multihash of a file on disk, read through once.
pub fn of_file(path: &std::path::Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(multihash(&hasher.finalize().into()))
}

/// The multihash of bytes in memory.
pub fn of_bytes(bytes: &[u8]) -> String {
    multihash(&Sha256::digest(bytes).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_round_trips_and_is_told_from_anything_else() {
        let hash = of_bytes(b"hello");
        // What `echo -n hello | sha256sum` says, as a multihash: starts with Qm, as sha2-256 ones do.
        assert!(hash.starts_with("Qm"), "{hash}");
        assert_eq!(digest_of(&hash).unwrap().to_vec(), Sha256::digest(b"hello").to_vec());
        assert_eq!(digest_of("QmNotAHash"), None);
        assert_eq!(digest_of("0OIl"), None);
        assert_eq!(digest_of(&bs58::encode([0x11, 0x20]).into_string()), None);
    }
}
