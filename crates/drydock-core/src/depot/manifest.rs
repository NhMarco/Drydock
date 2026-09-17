//! Parsing of a Steam depot **manifest** file (`<depot>_<manifest>.manifest`) served by the proxy.
//!
//! A manifest is SteamKit2's `DepotManifest` serialization: little-endian magic-delimited sections
//! wrapping protobuf messages — a `ContentManifestPayload` (the file/chunk list) and a
//! `ContentManifestMetadata` (depot id, manifest gid, whether filenames are encrypted). We decode
//! only the fields the downloader needs with a tiny hand-rolled protobuf reader, so there is no
//! `protoc`/`prost-build` dependency.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use thiserror::Error;

use super::crypto::{ChunkError, symmetric_decrypt};

const PAYLOAD_MAGIC: u32 = 0x71F6_17D0;
const METADATA_MAGIC: u32 = 0x1F48_12BE;
const SIGNATURE_MAGIC: u32 = 0x1B81_B817;
const END_MAGIC: u32 = 0x32C4_15AB;

/// Steam file flag: the entry is a directory (no content, no chunks).
pub const FLAG_DIRECTORY: u32 = 0x40;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest is truncated")]
    Truncated,
    #[error("unexpected manifest magic {0:#010x}")]
    BadMagic(u32),
    #[error("manifest protobuf is malformed")]
    BadProtobuf,
    #[error("filename could not be decrypted: {0}")]
    Filename(#[from] ChunkError),
    #[error("decrypted filename was not valid UTF-8")]
    FilenameUtf8,
}

/// One chunk of a file: its CDN id (SHA-1), Adler-32 checksum, position and sizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkEntry {
    pub sha: [u8; 20],
    pub crc: u32,
    pub offset: u64,
    pub uncompressed_len: u32,
    pub compressed_len: u32,
}

impl ChunkEntry {
    /// The lowercase hex chunk id used in the CDN URL `/depot/<depot>/chunk/<id>`.
    #[must_use]
    pub fn id_hex(&self) -> String {
        let mut s = String::with_capacity(40);
        for byte in self.sha {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }
}

/// One file (or directory) in the depot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Forward-slash relative path (decrypted when the manifest had encrypted filenames).
    pub path: String,
    pub size: u64,
    pub flags: u32,
    pub chunks: Vec<ChunkEntry>,
}

impl FileEntry {
    #[must_use]
    pub fn is_directory(&self) -> bool {
        self.flags & FLAG_DIRECTORY != 0
    }
}

/// A parsed depot manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepotManifest {
    pub depot_id: u32,
    pub manifest_gid: u64,
    pub filenames_encrypted: bool,
    pub files: Vec<FileEntry>,
}

impl DepotManifest {
    /// Parses a raw `.manifest` file.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        let mut reader = ByteReader::new(bytes);
        let mut payload: Option<&[u8]> = None;
        let mut metadata: Option<&[u8]> = None;

        loop {
            let magic = reader.read_u32()?;
            match magic {
                PAYLOAD_MAGIC => {
                    let len = reader.read_u32()? as usize;
                    payload = Some(reader.read_bytes(len)?);
                }
                METADATA_MAGIC => {
                    let len = reader.read_u32()? as usize;
                    metadata = Some(reader.read_bytes(len)?);
                }
                SIGNATURE_MAGIC => {
                    let len = reader.read_u32()? as usize;
                    reader.read_bytes(len)?; // signature is not verified client-side
                }
                END_MAGIC => break,
                other => return Err(ManifestError::BadMagic(other)),
            }
        }

        let metadata = metadata.ok_or(ManifestError::Truncated)?;
        let (depot_id, manifest_gid, filenames_encrypted) = parse_metadata(metadata)?;
        let mut files = parse_payload(payload.ok_or(ManifestError::Truncated)?)?;

        // Plaintext names just need their back-slashes normalized; encrypted names stay untouched
        // until `decrypt_filenames` runs.
        if !filenames_encrypted {
            for file in &mut files {
                file.path = file.path.trim_end().replace('\\', "/");
            }
        }

        Ok(Self {
            depot_id,
            manifest_gid,
            filenames_encrypted,
            files,
        })
    }

    /// When the manifest stored encrypted filenames, decrypts every path in place with the depot
    /// key (base64 → AES symmetric decrypt → UTF-8, back-slashes normalized to `/`).
    pub fn decrypt_filenames(&mut self, depot_key: &[u8; 32]) -> Result<(), ManifestError> {
        if !self.filenames_encrypted {
            return Ok(());
        }
        for file in &mut self.files {
            // The base64 field can carry embedded/trailing whitespace (e.g. a trailing newline), so
            // strip all ASCII whitespace before decoding.
            let cleaned: String = file.path.chars().filter(|c| !c.is_ascii_whitespace()).collect();
            let encrypted = BASE64.decode(&cleaned).map_err(|_| ManifestError::BadProtobuf)?;
            let decrypted = symmetric_decrypt(&encrypted, depot_key)?;
            let end = decrypted.iter().position(|&b| b == 0).unwrap_or(decrypted.len());
            let name = std::str::from_utf8(&decrypted[..end]).map_err(|_| ManifestError::FilenameUtf8)?;
            file.path = name.replace('\\', "/");
        }
        self.filenames_encrypted = false;
        Ok(())
    }

    /// Total download size (sum of compressed chunk lengths across real files).
    #[must_use]
    pub fn total_compressed(&self) -> u64 {
        self.files
            .iter()
            .flat_map(|file| &file.chunks)
            .map(|chunk| u64::from(chunk.compressed_len))
            .sum()
    }
}

/// Decodes the `ContentManifestMetadata` fields we use: depot_id(1), gid_manifest(2),
/// filenames_encrypted(4).
fn parse_metadata(bytes: &[u8]) -> Result<(u32, u64, bool), ManifestError> {
    let mut pb = ProtoReader::new(bytes);
    let mut depot_id = 0u32;
    let mut gid = 0u64;
    let mut encrypted = false;
    while let Some((field, wire)) = pb.next_field()? {
        match (field, wire) {
            (1, 0) => depot_id = pb.read_varint()? as u32,
            (2, 0) => gid = pb.read_varint()?,
            (4, 0) => encrypted = pb.read_varint()? != 0,
            _ => pb.skip(wire)?,
        }
    }
    Ok((depot_id, gid, encrypted))
}

/// Decodes `ContentManifestPayload` → repeated `FileMapping` (field 1).
fn parse_payload(bytes: &[u8]) -> Result<Vec<FileEntry>, ManifestError> {
    let mut pb = ProtoReader::new(bytes);
    let mut files = Vec::new();
    while let Some((field, wire)) = pb.next_field()? {
        if field == 1 && wire == 2 {
            let message = pb.read_len_delimited()?;
            files.push(parse_file_mapping(message)?);
        } else {
            pb.skip(wire)?;
        }
    }
    Ok(files)
}

/// Decodes one `FileMapping`: filename(1), size(2), flags(3), chunks(6, repeated).
fn parse_file_mapping(bytes: &[u8]) -> Result<FileEntry, ManifestError> {
    let mut pb = ProtoReader::new(bytes);
    let mut path = String::new();
    let mut size = 0u64;
    let mut flags = 0u32;
    let mut chunks = Vec::new();
    while let Some((field, wire)) = pb.next_field()? {
        match (field, wire) {
            (1, 2) => {
                // Plaintext when unencrypted; a base64 string of the AES ciphertext (sometimes with a
                // trailing newline) when encrypted — see `decrypt_filenames`.
                path = String::from_utf8_lossy(pb.read_len_delimited()?).into_owned();
            }
            (2, 0) => size = pb.read_varint()?,
            (3, 0) => flags = pb.read_varint()? as u32,
            (6, 2) => {
                let message = pb.read_len_delimited()?;
                chunks.push(parse_chunk(message)?);
            }
            _ => pb.skip(wire)?,
        }
    }
    Ok(FileEntry {
        path,
        size,
        flags,
        chunks,
    })
}

/// Decodes one `ChunkData`: sha(1), crc(2, fixed32), offset(3), cb_original(4), cb_compressed(5).
fn parse_chunk(bytes: &[u8]) -> Result<ChunkEntry, ManifestError> {
    let mut pb = ProtoReader::new(bytes);
    let mut sha = [0u8; 20];
    let mut crc = 0u32;
    let mut offset = 0u64;
    let mut uncompressed_len = 0u32;
    let mut compressed_len = 0u32;
    while let Some((field, wire)) = pb.next_field()? {
        match (field, wire) {
            (1, 2) => {
                let raw = pb.read_len_delimited()?;
                if raw.len() == 20 {
                    sha.copy_from_slice(raw);
                }
            }
            (2, 5) => crc = pb.read_fixed32()?,
            (3, 0) => offset = pb.read_varint()?,
            (4, 0) => uncompressed_len = pb.read_varint()? as u32,
            (5, 0) => compressed_len = pb.read_varint()? as u32,
            _ => pb.skip(wire)?,
        }
    }
    Ok(ChunkEntry {
        sha,
        crc,
        offset,
        uncompressed_len,
        compressed_len,
    })
}

/// A minimal little-endian byte cursor for the manifest container framing.
struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn read_u32(&mut self) -> Result<u32, ManifestError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], ManifestError> {
        let end = self.pos.checked_add(len).ok_or(ManifestError::Truncated)?;
        let slice = self.data.get(self.pos..end).ok_or(ManifestError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }
}

/// A minimal protobuf wire reader (varint / length-delimited / fixed32 / fixed64) — just enough to
/// walk the manifest messages and skip unknown fields.
struct ProtoReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ProtoReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Returns the next `(field_number, wire_type)` tag, or `None` at the end of the message.
    fn next_field(&mut self) -> Result<Option<(u64, u8)>, ManifestError> {
        if self.pos >= self.data.len() {
            return Ok(None);
        }
        let tag = self.read_varint()?;
        Ok(Some((tag >> 3, (tag & 0x7) as u8)))
    }

    fn read_varint(&mut self) -> Result<u64, ManifestError> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *self.data.get(self.pos).ok_or(ManifestError::BadProtobuf)?;
            self.pos += 1;
            result |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift >= 64 {
                return Err(ManifestError::BadProtobuf);
            }
        }
    }

    fn read_len_delimited(&mut self) -> Result<&'a [u8], ManifestError> {
        let len = self.read_varint()? as usize;
        let end = self.pos.checked_add(len).ok_or(ManifestError::BadProtobuf)?;
        let slice = self.data.get(self.pos..end).ok_or(ManifestError::BadProtobuf)?;
        self.pos = end;
        Ok(slice)
    }

    fn read_fixed32(&mut self) -> Result<u32, ManifestError> {
        let end = self.pos.checked_add(4).ok_or(ManifestError::BadProtobuf)?;
        let slice = self.data.get(self.pos..end).ok_or(ManifestError::BadProtobuf)?;
        self.pos = end;
        Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    fn skip(&mut self, wire: u8) -> Result<(), ManifestError> {
        match wire {
            0 => {
                self.read_varint()?;
            }
            1 => {
                let end = self.pos.checked_add(8).ok_or(ManifestError::BadProtobuf)?;
                self.data.get(self.pos..end).ok_or(ManifestError::BadProtobuf)?;
                self.pos = end;
            }
            2 => {
                self.read_len_delimited()?;
            }
            5 => {
                self.read_fixed32()?;
            }
            _ => return Err(ManifestError::BadProtobuf),
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // --- tiny protobuf/manifest writers, used only to build test fixtures ---

    fn varint(value: u64, out: &mut Vec<u8>) {
        let mut v = value;
        loop {
            let mut byte = (v & 0x7F) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if v == 0 {
                break;
            }
        }
    }

    fn tag(field: u64, wire: u8, out: &mut Vec<u8>) {
        varint((field << 3) | u64::from(wire), out);
    }

    fn field_varint(field: u64, value: u64, out: &mut Vec<u8>) {
        tag(field, 0, out);
        varint(value, out);
    }

    fn field_bytes(field: u64, value: &[u8], out: &mut Vec<u8>) {
        tag(field, 2, out);
        varint(value.len() as u64, out);
        out.extend_from_slice(value);
    }

    fn field_fixed32(field: u64, value: u32, out: &mut Vec<u8>) {
        tag(field, 5, out);
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn section(magic: u32, body: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&magic.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
    }

    /// Also used by the download module's tests, to assemble a whole depot package.
    pub(crate) fn build_manifest(depot_id: u32, gid: u64, encrypted: bool, files: &[FileEntry]) -> Vec<u8> {
        let mut payload = Vec::new();
        for file in files {
            let mut mapping = Vec::new();
            field_bytes(1, file.path.as_bytes(), &mut mapping);
            field_varint(2, file.size, &mut mapping);
            field_varint(3, u64::from(file.flags), &mut mapping);
            for chunk in &file.chunks {
                let mut c = Vec::new();
                field_bytes(1, &chunk.sha, &mut c);
                field_fixed32(2, chunk.crc, &mut c);
                field_varint(3, chunk.offset, &mut c);
                field_varint(4, u64::from(chunk.uncompressed_len), &mut c);
                field_varint(5, u64::from(chunk.compressed_len), &mut c);
                field_bytes(6, &c, &mut mapping);
            }
            field_bytes(1, &mapping, &mut payload);
        }
        let mut metadata = Vec::new();
        field_varint(1, u64::from(depot_id), &mut metadata);
        field_varint(2, gid, &mut metadata);
        if encrypted {
            field_varint(4, 1, &mut metadata);
        }

        let mut out = Vec::new();
        section(PAYLOAD_MAGIC, &payload, &mut out);
        section(METADATA_MAGIC, &metadata, &mut out);
        section(SIGNATURE_MAGIC, &[0xAA, 0xBB], &mut out);
        out.extend_from_slice(&END_MAGIC.to_le_bytes());
        out
    }

    fn sample_file() -> FileEntry {
        FileEntry {
            path: "bin/game.exe".into(),
            size: 2048,
            flags: 0,
            chunks: vec![ChunkEntry {
                sha: [0x11; 20],
                crc: 0xDEAD_BEEF,
                offset: 0,
                uncompressed_len: 1024,
                compressed_len: 512,
            }],
        }
    }

    #[test]
    fn parses_plain_manifest() {
        let file = sample_file();
        let bytes = build_manifest(228_990, 987_654_321, false, std::slice::from_ref(&file));
        let manifest = DepotManifest::parse(&bytes).expect("parse");
        assert_eq!(manifest.depot_id, 228_990);
        assert_eq!(manifest.manifest_gid, 987_654_321);
        assert!(!manifest.filenames_encrypted);
        assert_eq!(manifest.files, vec![file.clone()]);
        assert_eq!(manifest.files[0].chunks[0].id_hex(), "11".repeat(20));
        assert_eq!(manifest.total_compressed(), 512);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = build_manifest(1, 1, false, &[sample_file()]);
        bytes[0] ^= 0xFF;
        assert!(DepotManifest::parse(&bytes).is_err());
    }

    #[test]
    fn decrypts_encrypted_filenames() {
        let key = [9u8; 32];
        let iv = [4u8; 16];
        // Reality: the filename field holds a base64 string of the AES ciphertext, sometimes with a
        // trailing newline — exercise both here.
        let encrypted_name = super::super::crypto::symmetric_encrypt(b"data/level.pak", &key, &iv);
        let field = format!("{}\n", BASE64.encode(&encrypted_name));
        let mut mapping = Vec::new();
        field_bytes(1, field.as_bytes(), &mut mapping);
        field_varint(2, 10, &mut mapping);
        field_varint(3, 0, &mut mapping);
        let mut payload = Vec::new();
        field_bytes(1, &mapping, &mut payload);
        let mut metadata = Vec::new();
        field_varint(1, 1, &mut metadata);
        field_varint(2, 1, &mut metadata);
        field_varint(4, 1, &mut metadata); // filenames_encrypted
        let mut bytes = Vec::new();
        section(PAYLOAD_MAGIC, &payload, &mut bytes);
        section(METADATA_MAGIC, &metadata, &mut bytes);
        bytes.extend_from_slice(&END_MAGIC.to_le_bytes());

        let mut manifest = DepotManifest::parse(&bytes).expect("parse");
        assert!(manifest.filenames_encrypted);
        manifest.decrypt_filenames(&key).expect("decrypt names");
        assert!(!manifest.filenames_encrypted);
        assert_eq!(manifest.files[0].path, "data/level.pak");
    }
}
