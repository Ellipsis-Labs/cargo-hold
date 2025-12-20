use std::path::PathBuf;

use crate::state::{FileState, StateMetadata};

#[test]
fn test_state_metadata_operations() {
    let mut metadata = StateMetadata::new();
    assert!(metadata.is_empty());

    let state = FileState {
        path: PathBuf::from("src/main.rs"),
        size: 1234,
        hash: "abcdef".to_string(),
        mtime_nanos: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    };

    metadata.upsert(state.clone()).unwrap();
    assert_eq!(metadata.len(), 1);
    assert!(metadata.contains(&state.path).unwrap());

    let retrieved = metadata.get(&state.path).unwrap().unwrap();
    assert_eq!(retrieved.size, 1234);
    assert_eq!(retrieved.hash, "abcdef");

    metadata.remove(&state.path).unwrap();
    assert!(metadata.is_empty());
}

#[test]
fn test_max_mtime_nanos() {
    let mut metadata = StateMetadata::new();
    assert!(metadata.max_mtime_nanos().is_none());

    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let earlier_nanos = now_nanos - 10_000_000_000; // 10 seconds earlier

    metadata
        .upsert(FileState {
            path: PathBuf::from("file1.rs"),
            size: 100,
            hash: "hash1".to_string(),
            mtime_nanos: earlier_nanos,
        })
        .unwrap();

    metadata
        .upsert(FileState {
            path: PathBuf::from("file2.rs"),
            size: 200,
            hash: "hash2".to_string(),
            mtime_nanos: now_nanos,
        })
        .unwrap();

    assert_eq!(metadata.max_mtime_nanos(), Some(now_nanos));
}
