//! Cryptography and decompression primitives for Steam depot chunks.
//!
//! A depot chunk downloaded from the Steam CDN is **encrypted** with the depot's 32-byte AES key
//! and then **compressed** (either Valve's "VZip"/LZMA container or a plain deflate/zip stream).
//! Recovering the raw bytes is: [`symmetric_decrypt`] → [`decompress`], after which the chunk's
//! Adler-32 ([`adler32`]) must match the manifest. This mirrors SteamKit2's `CryptoHelper`,
//! `VZipUtil` and `DepotChunk.Process`.

use std::io::Read;

use aes::Aes256;
use aes::cipher::block_padding::Pkcs7;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockDecryptMut, KeyInit, KeyIvInit};
use thiserror::Error;

type Aes256CbcDec = cbc::Decryptor<Aes256>;

const VZIP_HEADER: u16 = 0x5A56; // "VZ"
const VZIP_FOOTER: u16 = 0x767A; // "zv"
/// VSZ (zstd) container framing: 8-byte header ("VSZ" + version + u32 crc) and a 15-byte footer
/// (u32 crc + u64 uncompressed size + "zsv").
const VSZ_HEADER_LEN: usize = 8;
const VSZ_FOOTER_LEN: usize = 15;

#[derive(Debug, Error)]
pub enum ChunkError {
    #[error("encrypted chunk is too short")]
    TooShort,
    #[error("AES decryption failed: {0}")]
    Decrypt(&'static str),
    #[error("unrecognized chunk compression header")]
    UnknownCompression,
    #[error("VZip container is malformed: {0}")]
    Vzip(&'static str),
    #[error("LZMA decode failed: {0}")]
    Lzma(String),
    #[error("deflate/zip decode failed: {0}")]
    Inflate(String),
    #[error("chunk checksum mismatch (expected {expected:08x}, got {actual:08x})")]
    Checksum { expected: u32, actual: u32 },
}

/// Steam's symmetric decrypt (`CryptoHelper.SymmetricDecrypt`): the first 16 bytes are the IV
/// encrypted with AES-256-**ECB**; decrypting that block with the key yields the real IV, which
/// then decrypts the remainder with AES-256-**CBC** (PKCS7-padded).
pub fn symmetric_decrypt(input: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, ChunkError> {
    if input.len() < 32 || !input.len().is_multiple_of(16) {
        return Err(ChunkError::TooShort);
    }
    // Recover the IV: ECB-decrypt the first block (no chaining, no padding).
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut iv = GenericArray::clone_from_slice(&input[..16]);
    cipher.decrypt_block(&mut iv);

    let decryptor = Aes256CbcDec::new(GenericArray::from_slice(key), &iv);
    decryptor
        .decrypt_padded_vec_mut::<Pkcs7>(&input[16..])
        .map_err(|_| ChunkError::Decrypt("CBC/PKCS7 unpad"))
}

/// Decompresses a decrypted chunk by its container magic: `VSZ` (zstd, modern games), `VZ` (VZip /
/// LZMA), a `PK` ZIP archive, or a bare deflate/zlib stream. Returns the raw file bytes.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, ChunkError> {
    if data.len() >= 3 && data[0] == b'V' && data[1] == b'S' && data[2] == b'Z' {
        return vsz_decompress(data);
    }
    if data.len() >= 2 && data[0] == b'V' && data[1] == b'Z' {
        return vzip_decompress(data);
    }
    if data.len() >= 4 && &data[..4] == b"PK\x03\x04" {
        return zip_single_entry(data);
    }
    // Fall back to a raw zlib/deflate stream.
    inflate(data)
}

/// Decodes a Valve "VSZ" container: `VSZ` + version + u32, a **zstd** frame, and a 15-byte footer.
fn vsz_decompress(data: &[u8]) -> Result<Vec<u8>, ChunkError> {
    if data.len() < VSZ_HEADER_LEN + VSZ_FOOTER_LEN {
        return Err(ChunkError::Vzip("VSZ too short"));
    }
    let footer = &data[data.len() - VSZ_FOOTER_LEN..];
    if &footer[12..15] != b"zsv" {
        return Err(ChunkError::Vzip("bad VSZ footer"));
    }
    // The zstd frame sits between the fixed header and footer; it is exactly one self-contained
    // frame, so a plain streaming decode reads it to completion.
    let frame = &data[VSZ_HEADER_LEN..data.len() - VSZ_FOOTER_LEN];
    zstd::stream::decode_all(frame).map_err(|e| ChunkError::Lzma(format!("zstd: {e}")))
}

/// Decodes a Valve "VZip" container: `VZ` + version `a` + u32 timestamp, then 5 LZMA property
/// bytes, the LZMA stream, and a 10-byte footer (`crc:u32`, `size:u32`, `zv`).
fn vzip_decompress(data: &[u8]) -> Result<Vec<u8>, ChunkError> {
    // 7-byte header + 5 property bytes + 10-byte footer are the fixed overhead.
    if data.len() < 22 {
        return Err(ChunkError::Vzip("too short"));
    }
    if u16::from_le_bytes([data[0], data[1]]) != VZIP_HEADER || data[2] != b'a' {
        return Err(ChunkError::Vzip("bad header"));
    }
    let footer = &data[data.len() - 10..];
    if u16::from_le_bytes([footer[8], footer[9]]) != VZIP_FOOTER {
        return Err(ChunkError::Vzip("bad footer"));
    }
    let output_size = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]) as u64;
    let properties = &data[7..12];
    let stream = &data[12..data.len() - 10];

    // Reassemble the LZMA "alone" layout lzma-rs expects: 5 property bytes, an 8-byte little-endian
    // uncompressed size, then the raw stream.
    let mut alone = Vec::with_capacity(13 + stream.len());
    alone.extend_from_slice(properties);
    alone.extend_from_slice(&output_size.to_le_bytes());
    alone.extend_from_slice(stream);

    let mut out = Vec::with_capacity(output_size as usize);
    lzma_rs::lzma_decompress(&mut &alone[..], &mut out).map_err(|e| ChunkError::Lzma(e.to_string()))?;
    Ok(out)
}

/// Unzips the single stored/deflated entry of a chunk that Steam delivered as a ZIP archive.
fn zip_single_entry(data: &[u8]) -> Result<Vec<u8>, ChunkError> {
    let reader = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| ChunkError::Inflate(e.to_string()))?;
    if archive.is_empty() {
        return Err(ChunkError::Inflate("empty zip".into()));
    }
    let mut file = archive
        .by_index(0)
        .map_err(|e| ChunkError::Inflate(e.to_string()))?;
    let mut out = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut out)
        .map_err(|e| ChunkError::Inflate(e.to_string()))?;
    Ok(out)
}

/// Inflates a raw zlib/deflate stream.
fn inflate(data: &[u8]) -> Result<Vec<u8>, ChunkError> {
    let mut out = Vec::new();
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    if decoder.read_to_end(&mut out).is_ok() {
        return Ok(out);
    }
    // Some streams are raw deflate without the zlib wrapper.
    out.clear();
    let mut raw = flate2::read::DeflateDecoder::new(data);
    raw.read_to_end(&mut out)
        .map_err(|e| ChunkError::Inflate(e.to_string()))?;
    Ok(out)
}

/// The chunk checksum Steam stores in the manifest (`ChunkData.crc`). This is SteamKit2's
/// `Utils.AdlerHash`, which is an Adler-32 **variant that starts `a = 0, b = 0`** (not the standard
/// `a = 1`), returning `a | (b << 16)`.
#[must_use]
pub fn steam_adler_hash(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a: u32 = 0;
    let mut b: u32 = 0;
    // Reduce every 5552 bytes so the running sums never overflow u32.
    for block in data.chunks(5552) {
        for &byte in block {
            a += u32::from(byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    a | (b << 16)
}

/// The inverse of [`symmetric_decrypt`], used only to build round-trip test vectors (also consumed
/// by the manifest module's filename-decryption test).
#[cfg(test)]
pub(crate) fn symmetric_encrypt(plaintext: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Vec<u8> {
    use aes::cipher::block_padding::Pkcs7;
    use aes::cipher::{BlockEncrypt, BlockEncryptMut};
    type Aes256CbcEnc = cbc::Encryptor<Aes256>;

    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut iv_block = GenericArray::clone_from_slice(iv);
    cipher.encrypt_block(&mut iv_block); // ECB-encrypt the IV to form the header block
    let body = Aes256CbcEnc::new(GenericArray::from_slice(key), iv.into())
        .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
    let mut out = iv_block.to_vec();
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_adler_known_answer() {
        // SteamKit's a=0-seeded variant: empty -> 0, "A" -> 65 | (65<<16).
        assert_eq!(steam_adler_hash(b""), 0);
        assert_eq!(steam_adler_hash(b"A"), 65 | (65 << 16));
        // "AB": a = 65+66 = 131, b = 65 + 131 = 196.
        assert_eq!(steam_adler_hash(b"AB"), 131 | (196 << 16));
    }

    #[test]
    fn symmetric_decrypt_round_trips() {
        let key = [7u8; 32];
        let iv = [3u8; 16];
        let plaintext = b"The quick brown fox jumps over 13 depots.".to_vec();
        let encrypted = symmetric_encrypt(&plaintext, &key, &iv);
        let decrypted = symmetric_decrypt(&encrypted, &key).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn symmetric_decrypt_rejects_short_input() {
        assert!(matches!(
            symmetric_decrypt(&[0u8; 16], &[0u8; 32]),
            Err(ChunkError::TooShort)
        ));
    }

    #[test]
    fn decompress_handles_vsz_zstd() {
        let raw = b"modern depots use zstd for chunks; ".repeat(40);
        let frame = zstd::stream::encode_all(&raw[..], 3).unwrap();
        let mut container = Vec::new();
        container.extend_from_slice(b"VSZa"); // magic + version
        container.extend_from_slice(&0u32.to_le_bytes()); // header crc
        container.extend_from_slice(&frame);
        container.extend_from_slice(&0u32.to_le_bytes()); // footer crc
        container.extend_from_slice(&(raw.len() as u64).to_le_bytes()); // uncompressed size
        container.extend_from_slice(b"zsv"); // footer magic
        assert_eq!(decompress(&container).unwrap(), raw);
    }

    #[test]
    fn decompress_handles_deflate() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;
        let raw = b"depot chunk payload that compresses".repeat(4);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(decompress(&compressed).unwrap(), raw);
    }
}
