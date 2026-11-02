//! Bounded content copying shared by package ingestion and exports.
use crate::PqtyError;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
};

pub(super) fn copy_hashed(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    limit: u64,
    description: &Path,
) -> Result<(String, u64), PqtyError> {
    let mut buffer = vec![0; 64 * 1024];
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    loop {
        let count = reader.read(&mut buffer).map_err(|source| PqtyError::Io {
            path: description.to_owned(),
            source,
        })?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > limit {
            return Err(PqtyError::Usage(format!(
                "{} exceeded the {limit}-byte limit",
                description.display()
            )));
        }
        hash.update(&buffer[..count]);
        writer
            .write_all(&buffer[..count])
            .map_err(|source| PqtyError::Io {
                path: description.to_owned(),
                source,
            })?;
    }
    Ok((hex::encode(hash.finalize()), size))
}

pub(super) fn digest_file(path: &Path, limit: u64) -> Result<(String, u64), PqtyError> {
    let mut file = std::fs::File::open(path).map_err(|source| PqtyError::Io {
        path: path.to_owned(),
        source,
    })?;
    copy_hashed(&mut file, &mut std::io::sink(), limit, path)
}

#[cfg(test)]
mod tests {
    use crate::tests::{make_test_tree_writable, temporary_test_root};
    use std::{
        fs,
        io::{self, Read},
    };
    struct Generated {
        remaining: usize,
        max_read: usize,
    }
    impl Read for Generated {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            assert!(
                buffer.len() <= self.max_read,
                "ingestion requested an unbounded read"
            );
            let count = buffer.len().min(self.remaining);
            buffer[..count].fill(b'x');
            self.remaining -= count;
            Ok(count)
        }
    }
    #[test]
    fn object_ingestion_uses_bounded_reads_and_cleans_failed_staging() {
        let root = temporary_test_root("bounded-ingestion");
        let size = 3 * 1024 * 1024 + 17;
        let mut input = Generated {
            remaining: size,
            max_read: 64 * 1024,
        };
        let object = super::super::object::store_reader(&root, &mut input, None).unwrap();
        assert_eq!(object.size, size as u64);
        assert_eq!(
            fs::metadata(root.join(&object.digest[..2]).join(&object.digest))
                .unwrap()
                .len(),
            size as u64
        );
        let mut input = Generated {
            remaining: size,
            max_read: 64 * 1024,
        };
        assert!(
            !super::super::object::store_reader(&root, &mut input, Some(&object.digest))
                .unwrap()
                .created
        );
        let before = fs::read_dir(&root).unwrap().count();
        assert!(
            super::super::object::store_reader(&root, &mut &b"wrong"[..], Some(&object.digest))
                .is_err()
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), before);
        make_test_tree_writable(&root);
        fs::remove_dir_all(root).unwrap();
    }
}
