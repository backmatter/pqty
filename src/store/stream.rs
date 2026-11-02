//! Bounded content copying shared by package ingestion and exports.
use crate::PqtyError;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
};

/// The standard library owns transfer buffering; this adapter only observes
/// accepted bytes and enforces the artifact limit before forwarding them.
struct ObservedWriter<'a, F> {
    writer: &'a mut dyn Write,
    observe: F,
    limit: u64,
    written: u64,
    exceeded: bool,
}

impl<F: FnMut(&[u8], u64)> Write for ObservedWriter<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.written) {
            self.exceeded = true;
            return Err(std::io::Error::other("stream byte limit exceeded"));
        }
        let count = self.writer.write(bytes)?;
        self.written += count as u64;
        (self.observe)(&bytes[..count], self.written);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

pub(super) fn copy_observed(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    limit: u64,
    description: &Path,
    observe: impl FnMut(&[u8], u64),
) -> Result<u64, PqtyError> {
    let mut output = ObservedWriter {
        writer,
        observe,
        limit,
        written: 0,
        exceeded: false,
    };
    // Read at most one byte beyond the permitted content to detect overflow.
    let result = std::io::copy(&mut reader.take(limit.saturating_add(1)), &mut output);
    if output.exceeded {
        return Err(PqtyError::Usage(format!(
            "{} exceeded the {limit}-byte limit",
            description.display()
        )));
    }
    result.map_err(|source| PqtyError::Io {
        path: description.to_owned(),
        source,
    })
}

pub(super) fn copy_hashed(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    limit: u64,
    description: &Path,
) -> Result<(String, u64), PqtyError> {
    let mut hash = Sha256::new();
    let size = copy_observed(reader, writer, limit, description, |bytes, _| {
        hash.update(bytes);
    })?;
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
    }
    impl Read for Generated {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = buffer.len().min(self.remaining);
            buffer[..count].fill(b'x');
            self.remaining -= count;
            Ok(count)
        }
    }
    struct ShortReader<'a> {
        bytes: &'a [u8],
        interrupted: bool,
    }
    impl Read for ShortReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.interrupted) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = output.len().min(3);
            self.bytes.read(&mut output[..count])
        }
    }
    struct ShortWriter {
        bytes: Vec<u8>,
        interrupted: bool,
    }
    impl io::Write for ShortWriter {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.interrupted) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = input.len().min(2);
            self.bytes.extend_from_slice(&input[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn standard_copy_handles_short_and_interrupted_io_without_changing_hashes() {
        use sha2::{Digest, Sha256};
        let bytes = b"streamed content with interrupted and partial IO";
        let mut reader = ShortReader {
            bytes,
            interrupted: true,
        };
        let mut writer = ShortWriter {
            bytes: Vec::new(),
            interrupted: true,
        };
        let (digest, size) = super::copy_hashed(
            &mut reader,
            &mut writer,
            bytes.len() as u64,
            std::path::Path::new("fixture"),
        )
        .unwrap();
        assert_eq!(writer.bytes, bytes);
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(digest, hex::encode(Sha256::digest(bytes)));
    }

    #[test]
    fn standard_copy_preserves_empty_exact_and_exceeded_resource_limits() {
        for bytes in [b"".as_slice(), b"exact limit".as_slice()] {
            let mut output = Vec::new();
            let (_, size) = super::copy_hashed(
                &mut &*bytes,
                &mut output,
                bytes.len() as u64,
                std::path::Path::new("fixture"),
            )
            .unwrap();
            assert_eq!(size, bytes.len() as u64);
            assert_eq!(output, bytes);
        }
        let mut output = Vec::new();
        assert!(
            super::copy_hashed(
                &mut &b"too long"[..],
                &mut output,
                3,
                std::path::Path::new("fixture")
            )
            .unwrap_err()
            .to_string()
            .contains("exceeded")
        );
        assert!(output.len() <= 3);
    }

    #[test]
    fn object_ingestion_streams_and_cleans_failed_staging() {
        let root = temporary_test_root("bounded-ingestion");
        let size = 3 * 1024 * 1024 + 17;
        let mut input = Generated { remaining: size };
        let object = super::super::object::store_reader(&root, &mut input, None).unwrap();
        assert_eq!(object.size, size as u64);
        assert_eq!(
            fs::metadata(root.join(&object.digest[..2]).join(&object.digest))
                .unwrap()
                .len(),
            size as u64
        );
        let mut input = Generated { remaining: size };
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
