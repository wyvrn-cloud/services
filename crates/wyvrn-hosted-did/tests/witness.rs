//! A log that names a witness, written as a holder writes it and read as anyone
//! reads it: only as far as the witness has signed for it.

use std::collections::HashMap;

use base64::Engine;
use serde_json::{json, Value};
use wyvrn_hosted_did::read::{self, Files, Step};
use wyvrn_hosted_did::{verify, write, Fetch};

/// An Ed25519 key as an app keeps it.
fn jwk(seed: u8) -> String {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    json!({ "kty": "OKP", "crv": "Ed25519", "d": b64.encode(key.to_bytes()), "x": b64.encode(key.verifying_key().to_bytes()) }).to_string()
}

fn document(did: &str, auth: &str, device: &str) -> Value {
    json!({
        "@context": ["https://www.w3.org/ns/did/v1", "https://w3id.org/security/multikey/v1"],
        "id": did,
        "alsoKnownAs": ["did:web:id.example:alice"],
        "verificationMethod": [{ "id": format!("{did}#{auth}"), "type": "Multikey", "controller": did, "publicKeyMultibase": auth },
                               { "id": format!("{did}#{device}"), "type": "Multikey", "controller": did, "publicKeyMultibase": device }],
        "authentication": [format!("{did}#{auth}")],
        "keyAgreement": [format!("{did}#{device}")],
    })
}

struct Files2(HashMap<String, Vec<u8>>);

impl Fetch for Files2 {
    async fn get(&self, url: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.get(url).cloned())
    }
}

#[tokio::test]
async fn a_log_naming_a_witness_is_read_only_as_far_as_the_witness_has_signed() {
    let (update, witness) = (jwk(1), jwk(2));
    let witness_did = write::witness_did(&witness).unwrap();
    let auth = write::public_multikey(&update).unwrap();
    let options = write::Options { witness: Some(witness_did.clone()), ttl: Some(600) };
    let first = write::create_with(&document("did:webvh:{SCID}:id.example:alice", &auth, "z6LSfirst"), &update, std::slice::from_ref(&auth), &options).await.unwrap();
    let entry: Value = serde_json::from_str(&first.entry).unwrap();
    assert_eq!(entry["parameters"]["witness"], json!({ "threshold": 1, "witnesses": [{ "id": witness_did }] }));
    assert_eq!(entry["parameters"]["ttl"], 600);

    // The holder continues its own log without the witness's proofs to hand.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let second = write::append(&first.did, &first.log, &document(&first.did, &auth, "z6LSsecond"), &update, None).await.unwrap();
    assert!(verify::needs_witness_proofs(&second.log));

    let log_url = "https://id.example/alice/did.jsonl";
    let witness_url = "https://id.example/alice/did-witness.json";
    let with = |witness_file: Option<String>| {
        let mut files = HashMap::from([(log_url.to_string(), second.log.clone().into_bytes())]);
        if let Some(file) = witness_file {
            files.insert(witness_url.to_string(), file.into_bytes());
        }
        Files2(files)
    };

    // No proofs at all: it doesn't verify.
    assert!(wyvrn_hosted_did::resolve(&first.did, &with(None)).await.is_err());
    // A proof for the first entry only: the second isn't the DID's yet.
    let proof_one = write::witness_proof(&first.version_id, &witness).await.unwrap();
    assert!(wyvrn_hosted_did::resolve(&first.did, &with(Some(write::witness_file(&[(first.version_id.clone(), proof_one.clone())])))).await.is_err());
    // A proof for the second, by the witness: it is.
    let proof_two = write::witness_proof(&second.version_id, &witness).await.unwrap();
    let file = write::witness_file(&[(first.version_id.clone(), proof_one), (second.version_id.clone(), proof_two)]);
    let resolved = wyvrn_hosted_did::resolve(&first.did, &with(Some(file.clone()))).await.unwrap();
    assert_eq!(resolved.document["keyAgreement"], json!([format!("{}#z6LSsecond", first.did)]));
    // By someone else's key: not.
    let forged = write::witness_proof(&second.version_id, &jwk(3)).await.unwrap();
    assert!(wyvrn_hosted_did::resolve(&first.did, &with(Some(write::witness_file(&[(second.version_id.clone(), forged)])))).await.is_err());

    // Read a file at a time, as an app does: the witnesses' file is asked for after the log.
    let mut files: Files = HashMap::new();
    let Step::Need(url) = read::read(&first.did, &files).await else { panic!("the log first") };
    assert_eq!(url, log_url);
    files.insert(url, Some(second.log.clone()));
    let Step::Need(url) = read::read(&first.did, &files).await else { panic!("the witnesses' file next") };
    assert_eq!(url, witness_url);
    files.insert(url, Some(file));
    let mut step = read::read(&first.did, &files).await;
    while let Step::Need(url) = step {
        files.insert(url, None);
        step = read::read(&first.did, &files).await;
    }
    let Step::Done(done) = step else { panic!("read") };
    assert_eq!(done.resolved.did, first.did);
    // The holder turns witnessing on for a log written before: from the next entry.
    let plain = write::create(&document("did:webvh:{SCID}:id.example:alice", &auth, "z6LSfirst"), &update, std::slice::from_ref(&auth)).await.unwrap();
    assert!(!verify::needs_witness_proofs(&plain.log));
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let on = write::append_with(&plain.did, &plain.log, &document(&plain.did, &auth, "z6LSfirst"), &update, None, &options).await.unwrap();
    let entry: Value = serde_json::from_str(&on.entry).unwrap();
    assert_eq!(entry["parameters"]["witness"]["witnesses"][0]["id"], witness_did);
}
