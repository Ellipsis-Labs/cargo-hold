use std::fs::File;
use std::path::Path;

use blake3::Hasher;
use memmap2::Mmap;

use crate::error::HoldError;

/// Computes the BLAKE3 hash of a file using memory mapping and parallel
/// processing.
///
/// This function uses memory-mapped I/O for efficient reading and BLAKE3's
/// built-in parallelism for maximum performance. Symbolic links are rejected
/// for security reasons.
///
/// # Arguments
///
/// * `path` - Path to the file to hash
///
/// # Returns
///
/// A hex-encoded string of the file's BLAKE3 hash.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read
/// - The path points to a symbolic link
/// - Memory mapping fails
pub fn hash_file(path: &Path) -> Result<String, HoldError> {
    // Check for symlinks before opening
    let metadata = std::fs::symlink_metadata(path).map_err(|source| HoldError::IoError {
        path: path.to_path_buf(),
        source,
    })?;

    // Reject symlinks
    if metadata.is_symlink() {
        return Err(HoldError::InvalidFileType {
            path: path.to_path_buf(),
            message: "Symbolic links are not supported".to_string(),
        });
    }

    // Reject directories
    if metadata.is_dir() {
        return Err(HoldError::InvalidFileType {
            path: path.to_path_buf(),
            message: "Directories are not supported".to_string(),
        });
    }

    // Handle empty files without memory mapping
    if metadata.len() == 0 {
        let hasher = Hasher::new();
        return Ok(hasher.finalize().to_hex().to_string());
    }

    // Open the file
    let file = File::open(path).map_err(|source| HoldError::IoError {
        path: path.to_path_buf(),
        source,
    })?;

    // Memory map the file
    let mmap = unsafe { Mmap::map(&file) }.map_err(|source| HoldError::IoError {
        path: path.to_path_buf(),
        source,
    })?;

    // Use BLAKE3's optimized parallel hashing on memory-mapped data
    let mut hasher = Hasher::new();
    hasher.update_rayon(&mmap);

    Ok(hasher.finalize().to_hex().to_string())
}

/// Gets the size of a file in bytes, checking for symbolic links.
///
/// This function uses `symlink_metadata` to detect symbolic links without
/// following them, rejecting them for security reasons.
///
/// # Arguments
///
/// * `path` - Path to the file
///
/// # Returns
///
/// The size of the file in bytes.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be accessed
/// - The path points to a symbolic link
pub fn get_file_size(path: &Path) -> Result<u64, HoldError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| HoldError::IoError {
        path: path.to_path_buf(),
        source,
    })?;

    // Reject symlinks
    if metadata.is_symlink() {
        return Err(HoldError::InvalidFileType {
            path: path.to_path_buf(),
            message: "Symbolic links are not supported".to_string(),
        });
    }

    // Reject directories
    if metadata.is_dir() {
        return Err(HoldError::InvalidFileType {
            path: path.to_path_buf(),
            message: "Directories are not supported".to_string(),
        });
    }

    Ok(metadata.len())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_hash_file() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "hello world").unwrap();

        let hash = hash_file(&test_file).unwrap();
        // BLAKE3 hash of "hello world"
        assert_eq!(
            hash,
            "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24"
        );
    }

    #[test]
    fn test_hash_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("empty.txt");
        fs::write(&test_file, "").unwrap();

        let hash = hash_file(&test_file).unwrap();
        // BLAKE3 hash of empty string
        assert_eq!(
            hash,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn test_get_file_size() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("sized.txt");
        let content = "hello world";
        fs::write(&test_file, content).unwrap();

        let size = get_file_size(&test_file).unwrap();
        assert_eq!(size, content.len() as u64);
    }

    #[test]
    fn test_hash_nonexistent_file() {
        let result = hash_file(Path::new("/nonexistent/file"));
        assert!(matches!(result, Err(HoldError::IoError { .. })));
    }

    #[test]
    #[cfg(unix)]
    fn test_hash_symlink() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("target.txt");
        let link = temp_dir.path().join("link.txt");

        fs::write(&target, "content").unwrap();
        symlink(&target, &link).unwrap();

        let result = hash_file(&link);
        assert!(matches!(result, Err(HoldError::InvalidFileType { .. })));
    }

    #[test]
    #[cfg(unix)]
    fn test_get_file_size_symlink() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("target.txt");
        let link = temp_dir.path().join("link.txt");

        fs::write(&target, "content").unwrap();
        symlink(&target, &link).unwrap();

        let result = get_file_size(&link);
        assert!(matches!(result, Err(HoldError::InvalidFileType { .. })));
    }
}
