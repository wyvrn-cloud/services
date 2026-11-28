//! The bucket's commands, for whichever program keeps a store: wyvrn-files,
//! wyvrn-community, and wyvrn-community-host for each community it holds.

use crate::store::Store;

/// How many files, and bytes, are on disk and in each bucket.
pub fn places(store: &Store) -> anyhow::Result<()> {
    for (place, count, bytes) in store.places()? {
        println!("{place}\t{count} files\t{bytes} bytes");
    }
    if let Some(bucket) = store.bucket() {
        println!("new files go to bucket:{} ({bucket:?})", bucket.tag());
    }
    Ok(())
}

/// Writes, reads back and removes a test file, and says whether a page at
/// `origin` may fetch from the bucket.
pub async fn bucket_check(store: &Store, origin: &str) -> anyhow::Result<()> {
    let Some(bucket) = store.bucket() else { anyhow::bail!("no bucket is configured") };
    let checked = bucket.check(origin).await?;
    println!("the bucket takes, gives back and removes a file");
    if checked.cors {
        println!("a page at {origin} may fetch from it");
    } else {
        println!(
            "a page at {origin} may NOT fetch from it (CORS): apps following a download link to it will fail.\n\
             Allow GET and HEAD from any origin, the Range header, and expose Content-Range, Content-Length and ETag,\n\
             or have downloads passed on (downloads = \"proxy\")."
        );
    }
    Ok(())
}

/// Moves every stored file not in the bucket there, saying each as it goes.
pub async fn move_to_bucket(store: &Store) -> anyhow::Result<()> {
    let moved = store.move_to_bucket(|file| println!("{}\t{}\t{} bytes", file.id, file.place, file.byte_count)).await?;
    println!("{} moved, {} bytes", moved.moved, moved.bytes);
    for id in &moved.mismatched {
        println!("{id}: not what its record says; left where it is");
    }
    for id in &moved.unreadable {
        println!("{id}: couldn't be read; left where it is");
    }
    Ok(())
}
