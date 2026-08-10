//! The zip guard: everything that happens to an `.xlsx` before it is a
//! workbook.
//!
//! An `.xlsx` is a zip archive a stranger uploaded, so the discipline is the
//! one spec 020 §3 states and the knowledge base already writes down for
//! hostile archives — **bound before you decompress, and bound again after**.
//! Both halves are here, in the order they run, because the order *is* the
//! property:
//!
//! 1. [`admit_declared`] reads the central directory only. The entry count and
//!    every entry's declared uncompressed size are known from it, so an archive
//!    whose claimed expansion is over the ceiling, or whose claimed compression
//!    ratio is over it, is refused **without a byte being decompressed**. A zip
//!    bomb never runs.
//! 2. [`admit_active_content`] refuses external workbook links, remote data
//!    connections, embedded objects, and macro parts — from part names, so this
//!    too costs no decompression and reads no attacker-controlled XML.
//! 3. [`verify_streamed`] then expands every entry through a counting reader
//!    that stops at the entry's *declared* size and at the running total. A
//!    header that understates what it holds is caught here, which is the whole
//!    reason a second pass exists: step 1 believes the archive's own numbers,
//!    and step 3 is what makes believing them safe.
//!
//! Only after all three does a parser see the bytes. The parser expands the
//! archive a second time, and that is fine: by then the real expansion has been
//! measured and is inside the ceiling.

use std::io::{Cursor, Read};

use crate::local::capability::LocalInvocation;
use crate::local::ingest::schema::refuse;
use crate::sdk::errors::ConnectorFailure;

/// What one archive may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub max_entries: u64,
    pub max_uncompressed_bytes: u64,
    /// The largest expansion factor an honest office document has any reason
    /// to claim. XML compresses well; it does not compress a thousandfold.
    pub max_compression_ratio: u64,
}

/// One entry, as the central directory describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub name: String,
    pub declared_uncompressed: u64,
    pub compressed: u64,
}

/// How many bytes at a time the streaming pass pulls out of one entry.
const CHUNK: usize = 16 * 1_024;

/// Step 1: the central directory, before anything is decompressed.
///
/// Reading the directory itself allocates one record per member, which is why
/// bound 1 runs first and is not optional: the stored file's own size is what
/// caps how many members an archive can claim to have before this function is
/// ever called.
pub fn admit_declared(
    bytes: &[u8],
    limits: &ArchiveLimits,
) -> Result<Vec<ArchiveEntry>, ConnectorFailure> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| {
        refuse(
            "ingest_not_an_archive",
            "the stored file is not the archive its schema expects",
        )
    })?;
    if archive.len() as u64 > limits.max_entries {
        return Err(refuse(
            "ingest_archive_entries_exceeded",
            "the stored archive declares more entries than the schema admits",
        ));
    }

    let mut entries = Vec::with_capacity(archive.len());
    let mut declared_total: u64 = 0;
    let mut compressed_total: u64 = 0;
    for index in 0..archive.len() {
        // `by_index_raw` hands back the entry's *metadata* and a reader over
        // the still-compressed bytes: reaching a size here does not expand
        // anything.
        let entry = archive.by_index_raw(index).map_err(|_| {
            refuse(
                "ingest_not_an_archive",
                "the stored archive's directory could not be read",
            )
        })?;
        declared_total = declared_total.saturating_add(entry.size());
        compressed_total = compressed_total.saturating_add(entry.compressed_size());
        entries.push(ArchiveEntry {
            name: entry.name().to_owned(),
            declared_uncompressed: entry.size(),
            compressed: entry.compressed_size(),
        });
    }

    if declared_total > limits.max_uncompressed_bytes {
        return Err(refuse(
            "ingest_archive_expansion_exceeded",
            "the stored archive declares an expansion larger than the schema admits",
        ));
    }
    // A ratio is only meaningful against bytes that were actually stored; an
    // archive claiming expansion out of nothing is the same refusal.
    let ratio = declared_total / compressed_total.max(1);
    if ratio > limits.max_compression_ratio {
        return Err(refuse(
            "ingest_compression_ratio_exceeded",
            "the stored archive declares a compression ratio larger than the schema admits",
        ));
    }
    Ok(entries)
}

/// Step 2: the parts a spreadsheet has no business carrying.
///
/// The check is on part names, which is what makes it cheap *and* what makes it
/// honest: a name is structural, while "does this XML contain a macro" is a
/// question about content an attacker wrote.
pub fn admit_active_content(entries: &[ArchiveEntry]) -> Result<(), ConnectorFailure> {
    for entry in entries {
        let name = entry.name.to_ascii_lowercase();
        let refused = name.starts_with("xl/externallinks/")
            || name == "xl/connections.xml"
            || name.starts_with("xl/embeddings/")
            || name.starts_with("xl/activex/")
            || name.starts_with("xl/macrosheets/")
            || name.contains("vbaproject.bin")
            || name.contains("oleobject")
            || name.contains("activex")
            || name.ends_with(".bin") && !name.starts_with("xl/printersettings/");
        if refused {
            return Err(refuse(
                "ingest_active_content",
                "the stored workbook carries an external link, a data connection, an embedded \
                 object, or a macro part, and is refused before it is parsed",
            ));
        }
        // A traversing member name is refused with them: nothing here writes a
        // file, but a part that names its way out of the package is not a part.
        if name.contains("..") || name.starts_with('/') || name.contains('\\') {
            return Err(refuse(
                "ingest_active_content",
                "the stored archive carries a member whose name leaves the package",
            ));
        }
    }
    Ok(())
}

/// Step 3: expand everything, counting the bytes that actually come out.
///
/// Returns the real total. The per-entry stop is what catches a header that
/// understates its own size; the running total is what catches an archive whose
/// entries are each honest and whose sum is not.
pub fn verify_streamed(
    bytes: &[u8],
    entries: &[ArchiveEntry],
    limits: &ArchiveLimits,
    working_ceiling: u64,
    invocation: &LocalInvocation<'_>,
) -> Result<u64, ConnectorFailure> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| {
        refuse(
            "ingest_not_an_archive",
            "the stored file is not the archive its schema expects",
        )
    })?;
    let overflow = || {
        refuse(
            "ingest_uncompressed_overflow",
            "an archive member expanded past the size it declared, or past the total the schema \
             admits",
        )
    };

    let mut buffer = vec![0_u8; CHUNK];
    crate::local::ingest::charge(invocation, working_ceiling, CHUNK)?;
    let mut total: u64 = 0;
    for (index, entry) in entries.iter().enumerate() {
        let mut reader = archive.by_index(index).map_err(|_| overflow())?;
        let mut expanded: u64 = 0;
        loop {
            invocation.checkpoint()?;
            let read = reader.read(&mut buffer).map_err(|_| overflow())?;
            if read == 0 {
                break;
            }
            expanded = expanded.saturating_add(read as u64);
            total = total.saturating_add(read as u64);
            if expanded > entry.declared_uncompressed || total > limits.max_uncompressed_bytes {
                return Err(overflow());
            }
        }
        // The working-memory charge is the archive's real expansion, so a file
        // whose contents fit the archive ceiling but not the operation's own
        // memory is refused as memory rather than as an archive.
        crate::local::ingest::charge(invocation, working_ceiling, expanded as usize)?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_entries: 8,
            max_uncompressed_bytes: 64 * 1_024,
            max_compression_ratio: 20,
        }
    }

    fn archive(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in parts {
            writer.start_file(*name, options).expect("a part starts");
            writer.write_all(body).expect("a part is written");
        }
        writer.finish().expect("the archive finishes").into_inner()
    }

    /// The declared pass is answered from the directory alone, and it answers
    /// all three of its questions.
    #[test]
    fn the_declared_pass_refuses_before_it_expands_anything() {
        let honest = archive(&[("xl/workbook.xml", b"<workbook/>".to_vec())]);
        assert!(admit_declared(&honest, &limits()).is_ok());

        let bomb = archive(&[("xl/workbook.xml", vec![b'0'; 32 * 1_024])]);
        assert_eq!(
            admit_declared(&bomb, &limits()).unwrap_err().code(),
            "ingest_compression_ratio_exceeded"
        );

        let wide = archive(&[(
            "xl/workbook.xml",
            (0..96_u32 * 1_024).map(|byte| byte as u8).collect(),
        )]);
        assert_eq!(
            admit_declared(&wide, &limits()).unwrap_err().code(),
            "ingest_archive_expansion_exceeded"
        );

        let many: Vec<(&str, Vec<u8>)> = ["a", "b", "c", "d", "e", "f", "g", "h", "i"]
            .into_iter()
            .map(|name| (name, b"x".to_vec()))
            .collect();
        assert_eq!(
            admit_declared(&archive(&many), &limits())
                .unwrap_err()
                .code(),
            "ingest_archive_entries_exceeded"
        );

        assert_eq!(
            admit_declared(b"not a zip at all", &limits())
                .unwrap_err()
                .code(),
            "ingest_not_an_archive"
        );
    }

    /// Every part name a workbook has no business carrying.
    #[test]
    fn active_content_is_refused_by_part_name() {
        for part in [
            "xl/externalLinks/externalLink1.xml",
            "xl/connections.xml",
            "xl/embeddings/oleObject1.bin",
            "xl/vbaProject.bin",
            "xl/macrosheets/sheet1.xml",
            "../escape.xml",
        ] {
            let entries = vec![ArchiveEntry {
                name: part.to_owned(),
                declared_uncompressed: 1,
                compressed: 1,
            }];
            assert_eq!(
                admit_active_content(&entries).unwrap_err().code(),
                "ingest_active_content",
                "{part}"
            );
        }
        let entries = vec![ArchiveEntry {
            name: "xl/worksheets/sheet1.xml".to_owned(),
            declared_uncompressed: 1,
            compressed: 1,
        }];
        assert!(admit_active_content(&entries).is_ok());
    }
}
