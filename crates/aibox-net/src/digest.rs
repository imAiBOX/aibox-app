//! Shared digest policy for downloads, cache hits and post-download verification.
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::Path;

// Source archives only: bound both work/input and expansion (including all members).
pub const MAX_GZIP_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_CONTENT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DigestMode {
    #[default]
    Bytes,
    Content,
}

impl DigestMode {
    pub fn parse(mode: Option<&str>) -> Result<Self, &'static str> {
        match mode {
            None => Ok(Self::Bytes),
            Some(s) if s.eq_ignore_ascii_case("bytes") => Ok(Self::Bytes),
            Some(s) if s.eq_ignore_ascii_case("content") => Ok(Self::Content),
            _ => Err("Unknown download hash mode"),
        }
    }

    pub fn digest(self, path: &Path) -> io::Result<String> {
        match self {
            Self::Bytes => crate::download::sha256_file(path),
            Self::Content => gzip_content_sha256(path),
        }
    }

    pub fn expected_size(self, size: Option<u64>) -> Option<u64> {
        if self == Self::Content {
            None
        } else {
            size
        }
    }
}

pub fn gzip_content_sha256(path: &Path) -> io::Result<String> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_GZIP_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gzip input limit exceeded",
        ));
    }
    // MultiGzDecoder checks CRC/ISIZE and consumes every member, rejecting trailing junk.
    let decoder = flate2::read::MultiGzDecoder::new(file.take(MAX_GZIP_BYTES + 1));
    hash_content(decoder, MAX_CONTENT_BYTES)
}

fn hash_content(reader: impl Read, limit: u64) -> io::Result<String> {
    let mut reader = reader.take(limit + 1);
    let mut hasher = Sha256::new();
    let mut total = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "gzip expansion limit exceeded",
            ));
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expansion_is_bounded() {
        assert!(hash_content(&b"12345"[..], 4).is_err());
        assert_eq!(
            hash_content(&b"12345"[..], 5).unwrap(),
            crate::sha256_hex(b"12345")
        );
    }
    #[test]
    fn modes_fail_closed() {
        assert!(DigestMode::parse(Some("typo")).is_err());
        assert_eq!(DigestMode::parse(None).unwrap(), DigestMode::Bytes);
        assert_eq!(DigestMode::Content.expected_size(Some(123)), None);
    }
}
