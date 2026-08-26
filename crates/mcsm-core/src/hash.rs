//! File digests. SHA-1 is what the Mojang manifest publishes for the server jar;
//! SHA-512 is what Modrinth's file-lookup endpoints key on.

use std::io::Read as _;
use std::path::Path;

// Both `sha1` and `sha2` re-export the same `digest::Digest` trait.
use sha2::Digest;

use crate::error::{Error, Result};

fn hash_file<D: Digest>(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let mut hasher = D::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| Error::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Lowercase hex SHA-1 of a file.
pub fn sha1_hex(path: &Path) -> Result<String> {
    hash_file::<sha1::Sha1>(path)
}

/// Lowercase hex SHA-512 of a file.
pub fn sha512_hex(path: &Path) -> Result<String> {
    hash_file::<sha2::Sha512>(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        let path = std::env::temp_dir().join(format!("mcsm-hash-{}.bin", std::process::id()));
        std::fs::write(&path, b"hello world").unwrap();
        assert_eq!(
            sha1_hex(&path).unwrap(),
            "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"
        );
        assert_eq!(
            sha512_hex(&path).unwrap(),
            "309ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f"
        );
        std::fs::remove_file(&path).ok();
    }
}
