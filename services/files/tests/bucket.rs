//! Files kept in a real S3-compatible bucket: put over files/1.0 as ever, moved
//! to the bucket, fetched by a redirect or passed on, deleted, and older files
//! moved from disk and between buckets.
//!
//! Needs a bucket service to talk to, so it runs only when one is named:
//!
//! ```sh
//! docker run -d --rm --name wyvrn-minio-test -p 127.0.0.1:19000:9000 \
//!   -e MINIO_ROOT_USER=wyvrntest -e MINIO_ROOT_PASSWORD=wyvrntest-secret \
//!   minio/minio server /data
//! WYVRN_TEST_BUCKET=http://127.0.0.1:19000 WYVRN_TEST_BUCKET_KEY=wyvrntest \
//!   WYVRN_TEST_BUCKET_SECRET=wyvrntest-secret cargo test -p wyvrn-files --test bucket
//! ```

use std::sync::Arc;

use didcomm_agent::{Agent, Identity};
use rusty_s3::S3Action;
use serde_json::{json, Value};
use wyvrn_files::{
    bucket::{Bucket, BucketConfig, Downloads},
    hash,
    host::{Host, PIURI},
    server::{self, AppState},
    store::{NewFile, Store},
};

const BUCKET: &str = "wyvrn-files-test";

/// The bucket service named by the environment, with a folder of its own for
/// this test, or `None` (and the test says it was skipped).
async fn bucket_config(downloads: Downloads) -> Option<BucketConfig> {
    let Ok(endpoint) = std::env::var("WYVRN_TEST_BUCKET") else {
        eprintln!("skipped: set WYVRN_TEST_BUCKET (and _KEY, _SECRET) to a bucket service to run this");
        return None;
    };
    let config = BucketConfig {
        endpoint: endpoint.clone(),
        name: BUCKET.into(),
        region: "us-east-1".into(),
        prefix: format!("test-{}", uuid::Uuid::new_v4()),
        path_style: true,
        downloads,
        link_seconds: 120,
        key: std::env::var("WYVRN_TEST_BUCKET_KEY").expect("WYVRN_TEST_BUCKET_KEY"),
        secret: std::env::var("WYVRN_TEST_BUCKET_SECRET").expect("WYVRN_TEST_BUCKET_SECRET"),
        key_env: None,
        secret_env: None,
    };
    // The bucket itself, made if it isn't there yet.
    let s3 = rusty_s3::Bucket::new(endpoint.parse().unwrap(), rusty_s3::UrlStyle::Path, BUCKET, "us-east-1").unwrap();
    let credentials = rusty_s3::Credentials::new(config.key.clone(), config.secret.clone());
    let url = s3.create_bucket(&credentials).sign(std::time::Duration::from_secs(60));
    let made = reqwest::Client::new().put(url).send().await.unwrap();
    assert!(made.status().is_success() || made.status() == 409, "{}", made.text().await.unwrap());
    Some(config)
}

struct Running {
    did: String,
    state: Arc<AppState>,
}

async fn start_service(uploader: &str, bucket: &BucketConfig) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let config: wyvrn_files::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\ndir = \"unused\"\n\
         [service]\nuploaders = [\"{uploader}\"]\nchunk_bytes = 8\nmax_file_bytes = 100\nquota_bytes = 300\n"
    ))
    .unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let did = agent.did();
    let store = Store::scratch().unwrap().with_buckets(Some(Bucket::new(bucket).unwrap()), Vec::new());
    let host = Host::new(store, config.service.clone(), &url);
    let state = Arc::new(AppState::new(agent, host, did_document));
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { did, state }
}

async fn ask(agent: &Agent, service: &Running, kind: &str, body: Value) -> Value {
    let message = json!({ "type": format!("{PIURI}/{kind}"), "body": body, "return_route": "all" });
    agent.send(&service.did, &message).await.expect("sent").expect("an answer").message
}

/// Puts `bytes` as a file, in 8-byte chunks, and completes it: its id and link.
async fn put_file(agent: &Agent, service: &Running, name: &str, bytes: &[u8]) -> (String, String) {
    let granted = ask(agent, service, "upload", json!({ "name": name, "media_type": "text/plain", "byte_count": bytes.len(), "hash": hash::of_bytes(bytes) })).await;
    let id = granted["body"]["file"].as_str().expect("granted").to_string();
    let upload_url = granted["body"]["upload_url"].as_str().unwrap().to_string();
    let http = reqwest::Client::new();
    for (n, chunk) in bytes.chunks(8).enumerate() {
        let put = http.put(&upload_url).header("Upload-Offset", (n * 8).to_string()).body(chunk.to_vec()).send().await.unwrap();
        assert_eq!(put.status(), 204);
    }
    let completed = ask(agent, service, "complete", json!({ "file": id })).await;
    (id, completed["body"]["url"].as_str().expect("completed").to_string())
}

fn not_following() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap()
}

#[tokio::test]
async fn a_file_put_as_ever_goes_to_the_bucket_and_is_fetched_from_it_by_a_link() {
    let Some(config) = bucket_config(Downloads::Redirect).await else { return };
    let alice = Agent::new(Identity::generate().unwrap()).unwrap();
    let service = start_service(&alice.did(), &config).await;
    let bytes = b"The quick brown fox jumps over the lazy dog.";
    let (id, url) = put_file(&alice, &service, "fox.txt", bytes).await;
    let store = &service.state.host.store;

    // Stored, on disk, waiting for the bucket: served from disk meanwhile.
    assert_eq!(store.file(&id).unwrap().unwrap().place, "spool");
    assert_eq!(reqwest::get(&url).await.unwrap().bytes().await.unwrap().as_ref(), bytes);

    // Moved, and gone from disk.
    wyvrn_files::serve::tend(store).await;
    let file = store.file(&id).unwrap().unwrap();
    let bucket = store.bucket().unwrap().clone();
    assert_eq!(file.place, format!("bucket:{}", bucket.tag()));
    assert!(!store.path(&id).exists());
    assert_eq!(store.read(&file).await.unwrap(), bytes);
    assert_eq!(store.read_now(&file).unwrap(), bytes);

    // Fetched: a redirect, not kept past the link, to a link that answers whole,
    // by range, and as the file is named.
    let redirected = not_following().get(&url).send().await.unwrap();
    assert_eq!(redirected.status(), 302);
    assert_eq!(redirected.headers()["cache-control"].to_str().unwrap(), "private, max-age=60");
    let link = redirected.headers()["location"].to_str().unwrap().to_string();
    assert!(link.starts_with(&format!("{}/{BUCKET}/", config.endpoint)), "{link}");
    let whole = reqwest::get(&link).await.unwrap();
    assert_eq!(whole.status(), 200);
    assert_eq!(whole.headers()["content-type"].to_str().unwrap(), "text/plain");
    assert!(whole.headers()["content-disposition"].to_str().unwrap().starts_with("attachment; filename=\"fox.txt\""));
    assert_eq!(whole.bytes().await.unwrap().as_ref(), bytes);
    let part = reqwest::Client::new().get(&link).header("range", "bytes=4-8").send().await.unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(part.bytes().await.unwrap().as_ref(), b"quick");
    // Followed, as an app's fetch does.
    assert_eq!(reqwest::get(&url).await.unwrap().bytes().await.unwrap().as_ref(), bytes);
    // Asked with what is held already: not sent again.
    let held = not_following().get(&url).header("if-none-match", format!("\"{}\"", file.hash)).send().await.unwrap();
    assert_eq!(held.status(), 304);

    // Deleted: gone here at once, and from the bucket once it is told.
    let key = bucket.key(&id);
    assert_eq!(bucket.size(&key).await.unwrap(), Some(bytes.len() as u64));
    let deleted = ask(&alice, &service, "delete", json!({ "file": id })).await;
    assert!(deleted["type"].as_str().unwrap().ends_with("/deleted"), "{deleted}");
    assert_eq!(reqwest::get(&url).await.unwrap().status(), 404);
    wyvrn_files::serve::tend(store).await;
    assert_eq!(bucket.size(&key).await.unwrap(), None);
    bucket.delete_all().await.unwrap();
}

#[tokio::test]
async fn a_bucket_kept_out_of_sight_is_passed_on_range_and_all() {
    let Some(config) = bucket_config(Downloads::Proxy).await else { return };
    let alice = Agent::new(Identity::generate().unwrap()).unwrap();
    let service = start_service(&alice.did(), &config).await;
    let bytes = b"The quick brown fox jumps over the lazy dog.";
    let (id, url) = put_file(&alice, &service, "fox.txt", bytes).await;
    wyvrn_files::serve::tend(&service.state.host.store).await;
    assert!(service.state.host.store.file(&id).unwrap().unwrap().place.starts_with("bucket:"));

    let whole = not_following().get(&url).send().await.unwrap();
    assert_eq!(whole.status(), 200);
    assert_eq!(whole.headers()["content-type"].to_str().unwrap(), "text/plain");
    assert!(whole.headers()["content-security-policy"].to_str().unwrap().starts_with("sandbox"));
    assert_eq!(whole.headers()["etag"].to_str().unwrap(), format!("\"{}\"", hash::of_bytes(bytes)));
    assert_eq!(whole.bytes().await.unwrap().as_ref(), bytes);
    let part = reqwest::Client::new().get(&url).header("range", "bytes=10-14").send().await.unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(part.headers()["content-range"].to_str().unwrap(), format!("bytes 10-14/{}", bytes.len()));
    assert_eq!(part.bytes().await.unwrap().as_ref(), b"brown");
    let head = reqwest::Client::new().head(&url).send().await.unwrap();
    assert_eq!((head.status().as_u16(), head.headers()["content-length"].to_str().unwrap()), (200, bytes.len().to_string().as_str()));
    service.state.host.store.bucket().unwrap().delete_all().await.unwrap();
}

/// A file made and stored in `store` as an upload would be.
fn stored(store: &Store, bytes: &[u8]) -> String {
    let file = store
        .create(NewFile { owner: "did:example:alice", name: "a.bin", media_type: "application/octet-stream", byte_count: bytes.len() as u64, hash: &hash::of_bytes(bytes), created: 1, upload_expires: 100, expires: i64::MAX })
        .unwrap();
    store.append(&file.id, bytes).unwrap();
    store.mark_stored(&file.id).unwrap();
    file.id
}

#[tokio::test]
async fn files_kept_before_are_moved_from_disk_and_between_buckets_with_their_hashes_checked() {
    let Some(first) = bucket_config(Downloads::Redirect).await else { return };
    // Kept on disk, before there was a bucket.
    let store = Store::scratch().unwrap();
    let kept = stored(&store, b"kept on disk");
    let spoiled = stored(&store, b"spoiled later");
    assert_eq!(store.file(&kept).unwrap().unwrap().place, "disk");
    std::fs::write(store.path(&spoiled), b"not what was stored").unwrap();

    // A bucket configured: new files wait for it, old ones stay where they are
    // and are still read there, until moved.
    let store = store.with_buckets(Some(Bucket::new(&first).unwrap()), Vec::new());
    let new = stored(&store, b"new since");
    assert_eq!(store.file(&new).unwrap().unwrap().place, "spool");
    assert_eq!(store.read(&store.file(&kept).unwrap().unwrap()).await.unwrap(), b"kept on disk");
    let moved = store.move_to_bucket(|_| {}).await.unwrap();
    assert_eq!((moved.moved, moved.mismatched.clone(), moved.unreadable.len()), (2, vec![spoiled.clone()], 0), "{moved:?}");
    let tag = store.bucket().unwrap().tag().to_string();
    for id in [&kept, &new] {
        let file = store.file(id).unwrap().unwrap();
        assert_eq!(file.place, format!("bucket:{tag}"));
        assert!(!store.path(id).exists());
    }
    // What didn't match is left on disk, as it was.
    assert_eq!(store.file(&spoiled).unwrap().unwrap().place, "disk");
    assert!(store.path(&spoiled).exists());
    assert_eq!(store.places().unwrap().iter().map(|(place, count, _)| (place.clone(), *count)).collect::<Vec<_>>(), vec![(format!("bucket:{tag}"), 2), ("disk".into(), 1)]);

    // Another bucket from now on: the first is still read from, until moved from.
    let second = BucketConfig { prefix: format!("{}-second", first.prefix), ..first.clone() };
    let store = store.with_buckets(Some(Bucket::new(&second).unwrap()), vec![Bucket::new(&first).unwrap()]);
    assert_eq!(store.read(&store.file(&kept).unwrap().unwrap()).await.unwrap(), b"kept on disk");
    let moved = store.move_to_bucket(|_| {}).await.unwrap();
    assert_eq!(moved.moved, 2, "{moved:?}");
    let old = Bucket::new(&first).unwrap();
    assert_eq!(old.size(&old.key(&kept)).await.unwrap(), None);
    assert_eq!(store.read(&store.file(&kept).unwrap().unwrap()).await.unwrap(), b"kept on disk");
    store.bucket().unwrap().delete_all().await.unwrap();
    old.delete_all().await.unwrap();
}

#[tokio::test]
async fn a_bucket_is_checked_by_writing_reading_and_removing_a_file() {
    let Some(config) = bucket_config(Downloads::Redirect).await else { return };
    let bucket = Bucket::new(&config).unwrap();
    let checked = bucket.check("http://tauri.localhost").await.unwrap();
    // MinIO answers every origin.
    assert!(checked.cors, "{checked:?}");
    assert!(bucket.list("").await.unwrap().is_empty());
    let wrong = Bucket::new(&BucketConfig { secret: "not it".into(), ..config }).unwrap();
    assert!(wrong.check("http://tauri.localhost").await.is_err());
}

#[tokio::test]
async fn a_file_too_big_for_one_put_goes_up_in_parts() {
    let Some(config) = bucket_config(Downloads::Redirect).await else { return };
    let bucket = Bucket::new(&config).unwrap();
    // Over the 64 MiB put in one go: five parts of 16 MiB, the last shorter.
    let size: usize = 70 * 1024 * 1024 + 123;
    let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let path = std::env::temp_dir().join(format!("wyvrn-bucket-parts-{}", uuid::Uuid::new_v4()));
    std::fs::write(&path, &bytes).unwrap();
    let key = bucket.key("big");
    bucket.put_file(&key, &path, size as u64).await.unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(bucket.size(&key).await.unwrap(), Some(size as u64));
    let (digest, read) = bucket.hash(&key).await.unwrap();
    assert_eq!(read, size as u64);
    assert_eq!(hash::multihash(&digest.as_slice().try_into().unwrap()), hash::of_bytes(&bytes));
    bucket.delete_all().await.unwrap();
}
