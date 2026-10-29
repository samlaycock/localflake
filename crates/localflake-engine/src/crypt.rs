//! `ENCRYPT`, `DECRYPT`, `ENCRYPT_RAW`, `DECRYPT_RAW` and their TRY_ forms.
//!
//! The RAW functions are standard AES (128, 192 or 256-bit by key length) in
//! GCM (the default), CBC, ECB, CTR, OFB or CFB mode, so they interoperate
//! with Snowflake and other tools. The passphrase functions return the IV,
//! ciphertext and (for GCM) tag concatenated, as Snowflake's do, but derive
//! their AES-256 key with SHA-256 rather than Snowflake's internal PBKDF2 seed,
//! so they only decrypt what localflake encrypted.

use std::error::Error;
use std::sync::Arc;

use aes::cipher::block_padding::{NoPadding, Pkcs7};
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyInit, KeyIvInit, StreamCipher};
use aes::{Aes128, Aes192, Aes256};
use aes_gcm::AesGcm;
use aes_gcm::aead::AeadInOut;
use aes_gcm::aead::consts::U12;
use duckdb::arrow::array::{Array, ArrayRef, AsArray, BinaryArray, BooleanArray, RecordBatch, StructArray};
use duckdb::arrow::datatypes::{DataType, Field, Fields};
use duckdb::vscalar::{ArrowFunctionSignature, VArrowScalar};
use sha2::Digest;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const DECRYPTION_FAILED: &str = "Decryption failed. Check encrypted data, key, AAD, or AEAD tag.";
const GCM_IV: usize = 12;
const BLOCK_IV: usize = 16;
const TAG: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Gcm,
    Cbc,
    Ecb,
    Ctr,
    Ofb,
    Cfb,
}

/// `AES[-<mode>][/pad:<PKCS|NONE>]`, case-insensitive.
fn parse_method(method: &str) -> Result<(Mode, bool)> {
    let method = method.to_uppercase();
    let (cipher, padding) = match method.split_once("/PAD:") {
        Some((cipher, padding)) => (cipher.to_owned(), padding.trim().to_owned()),
        None => (method.clone(), "PKCS".to_owned()),
    };
    let mode = match cipher.trim().strip_prefix("AES").map(|m| m.trim_start_matches('-')) {
        Some("" | "GCM") => Mode::Gcm,
        Some("CBC") => Mode::Cbc,
        Some("ECB") => Mode::Ecb,
        Some("CTR") => Mode::Ctr,
        Some("OFB") => Mode::Ofb,
        Some("CFB") => Mode::Cfb,
        _ => return Err(format!("unsupported encryption method '{method}'").into()),
    };
    let pad = match padding.as_str() {
        "PKCS" => true,
        "NONE" => false,
        _ => return Err(format!("unsupported padding '{padding}'").into()),
    };
    Ok((mode, pad))
}

fn iv_length(mode: Mode) -> usize {
    match mode {
        Mode::Gcm => GCM_IV,
        Mode::Ecb => 0,
        _ => BLOCK_IV,
    }
}

/// Runs `f` with the AES variant for the key's length.
macro_rules! with_aes {
    ($key:expr, $f:ident ( $($arg:expr),* )) => {
        match $key.len() {
            16 => $f::<Aes128>($key, $($arg),*),
            24 => $f::<Aes192>($key, $($arg),*),
            32 => $f::<Aes256>($key, $($arg),*),
            n => Err(format!("invalid key length {n} bytes: AES keys are 16, 24 or 32 bytes").into()),
        }
    };
}

/// The ciphertext and (for GCM) tag.
fn encrypt_raw(data: &[u8], key: &[u8], iv: &[u8], aad: &[u8], mode: Mode, pad: bool) -> Result<(Vec<u8>, Vec<u8>)> {
    if iv.len() != iv_length(mode) {
        return Err(format!("invalid IV length {} bytes: {mode:?} needs {}", iv.len(), iv_length(mode)).into());
    }
    with_aes!(key, encrypt_with(data, iv, aad, mode, pad))
}

fn encrypt_with<C>(key: &[u8], data: &[u8], iv: &[u8], aad: &[u8], mode: Mode, pad: bool) -> Result<(Vec<u8>, Vec<u8>)>
where
    C: aes::cipher::BlockCipherEncrypt
        + aes::cipher::BlockCipherDecrypt
        + KeyInit
        + aes::cipher::BlockSizeUser<BlockSize = aes::cipher::consts::U16>,
{
    let bad_key = |_| "invalid key";
    Ok(match mode {
        Mode::Gcm => {
            let cipher = AesGcm::<C, U12>::new_from_slice(key).map_err(bad_key)?;
            let mut buffer = data.to_vec();
            let nonce = iv.try_into().map_err(|_| "invalid IV")?;
            let tag = cipher
                .encrypt_inout_detached(nonce, aad, buffer.as_mut_slice().into())
                .map_err(|_| "encryption failed")?;
            (buffer, tag.to_vec())
        }
        Mode::Cbc => {
            let cipher = cbc::Encryptor::<C>::new_from_slices(key, iv).map_err(bad_key)?;
            let out = if pad {
                cipher.encrypt_padded_vec::<Pkcs7>(data)
            } else {
                if !data.len().is_multiple_of(16) {
                    return Err("with no padding, the value must be a multiple of 16 bytes".into());
                }
                cipher.encrypt_padded_vec::<NoPadding>(data)
            };
            (out, vec![])
        }
        Mode::Ecb => {
            let cipher = ecb::Encryptor::<C>::new_from_slice(key).map_err(bad_key)?;
            let out = if pad {
                cipher.encrypt_padded_vec::<Pkcs7>(data)
            } else {
                if !data.len().is_multiple_of(16) {
                    return Err("with no padding, the value must be a multiple of 16 bytes".into());
                }
                cipher.encrypt_padded_vec::<NoPadding>(data)
            };
            (out, vec![])
        }
        Mode::Ctr => {
            let mut buffer = data.to_vec();
            ctr::Ctr128BE::<C>::new_from_slices(key, iv).map_err(bad_key)?.apply_keystream(&mut buffer);
            (buffer, vec![])
        }
        Mode::Ofb => {
            let mut buffer = data.to_vec();
            ofb::Ofb::<C>::new_from_slices(key, iv).map_err(bad_key)?.apply_keystream(&mut buffer);
            (buffer, vec![])
        }
        Mode::Cfb => {
            let mut buffer = data.to_vec();
            cfb_mode::BufEncryptor::<C>::new_from_slices(key, iv).map_err(bad_key)?.encrypt(&mut buffer);
            (buffer, vec![])
        }
    })
}

fn decrypt_raw(data: &[u8], key: &[u8], iv: &[u8], aad: &[u8], tag: &[u8], mode: Mode, pad: bool) -> Result<Vec<u8>> {
    if iv.len() != iv_length(mode) {
        return Err(format!("invalid IV length {} bytes: {mode:?} needs {}", iv.len(), iv_length(mode)).into());
    }
    with_aes!(key, decrypt_with(data, iv, aad, tag, mode, pad))
}

fn decrypt_with<C>(key: &[u8], data: &[u8], iv: &[u8], aad: &[u8], tag: &[u8], mode: Mode, pad: bool) -> Result<Vec<u8>>
where
    C: aes::cipher::BlockCipherEncrypt
        + aes::cipher::BlockCipherDecrypt
        + KeyInit
        + aes::cipher::BlockSizeUser<BlockSize = aes::cipher::consts::U16>,
{
    let failed = |_| DECRYPTION_FAILED;
    Ok(match mode {
        Mode::Gcm => {
            let cipher = AesGcm::<C, U12>::new_from_slice(key).map_err(failed)?;
            let mut buffer = data.to_vec();
            let nonce = iv.try_into().map_err(|_| DECRYPTION_FAILED)?;
            let tag = tag.try_into().map_err(|_| DECRYPTION_FAILED)?;
            cipher
                .decrypt_inout_detached(nonce, aad, buffer.as_mut_slice().into(), tag)
                .map_err(|_| DECRYPTION_FAILED)?;
            buffer
        }
        Mode::Cbc => {
            let cipher = cbc::Decryptor::<C>::new_from_slices(key, iv).map_err(failed)?;
            if pad { cipher.decrypt_padded_vec::<Pkcs7>(data) } else { cipher.decrypt_padded_vec::<NoPadding>(data) }
                .map_err(|_| DECRYPTION_FAILED)?
        }
        Mode::Ecb => {
            let cipher = ecb::Decryptor::<C>::new_from_slice(key).map_err(failed)?;
            if pad { cipher.decrypt_padded_vec::<Pkcs7>(data) } else { cipher.decrypt_padded_vec::<NoPadding>(data) }
                .map_err(|_| DECRYPTION_FAILED)?
        }
        Mode::Ctr => {
            let mut buffer = data.to_vec();
            ctr::Ctr128BE::<C>::new_from_slices(key, iv).map_err(failed)?.apply_keystream(&mut buffer);
            buffer
        }
        Mode::Ofb => {
            let mut buffer = data.to_vec();
            ofb::Ofb::<C>::new_from_slices(key, iv).map_err(failed)?.apply_keystream(&mut buffer);
            buffer
        }
        Mode::Cfb => {
            let mut buffer = data.to_vec();
            cfb_mode::BufDecryptor::<C>::new_from_slices(key, iv).map_err(failed)?.decrypt(&mut buffer);
            buffer
        }
    })
}

fn random_iv(len: usize) -> Vec<u8> {
    let mut iv = uuid::Uuid::new_v4().into_bytes().to_vec();
    iv.truncate(len);
    iv
}

/// The AES-256 key for a passphrase.
fn passphrase_key(passphrase: &str) -> Vec<u8> {
    sha2::Sha256::digest(format!("localflake:{passphrase}").as_bytes()).to_vec()
}

/// `ENCRYPT`: IV, ciphertext and tag, concatenated.
fn encrypt(data: &[u8], passphrase: &str, aad: &[u8], method: &str) -> Result<Vec<u8>> {
    let (mode, pad) = parse_method(method)?;
    let iv = random_iv(iv_length(mode));
    let (ciphertext, tag) = encrypt_raw(data, &passphrase_key(passphrase), &iv, aad, mode, pad)?;
    Ok([iv, ciphertext, tag].concat())
}

fn decrypt(data: &[u8], passphrase: &str, aad: &[u8], method: &str) -> Result<Vec<u8>> {
    let (mode, pad) = parse_method(method)?;
    let (iv_len, tag_len) = (iv_length(mode), if mode == Mode::Gcm { TAG } else { 0 });
    if data.len() < iv_len + tag_len {
        return Err(DECRYPTION_FAILED.into());
    }
    let (iv, rest) = data.split_at(iv_len);
    let (ciphertext, tag) = rest.split_at(rest.len() - tag_len);
    decrypt_raw(ciphertext, &passphrase_key(passphrase), iv, aad, tag, mode, pad)
}

fn binary_arg(input: &RecordBatch, i: usize) -> &BinaryArray {
    input.column(i).as_binary::<i32>()
}

fn optional(array: &BinaryArray, row: usize) -> &[u8] {
    if array.is_null(row) { &[] } else { array.value(row) }
}

/// `sf_encrypt(value, passphrase, aad, method) -> BLOB`.
pub struct Encrypt;

impl VArrowScalar for Encrypt {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let (values, aads) = (binary_arg(&input, 0), binary_arg(&input, 2));
        let (passphrases, methods) = (input.column(1).as_string::<i32>(), input.column(3).as_string::<i32>());
        let out = (0..values.len())
            .map(|i| -> Result<Option<Vec<u8>>> {
                if values.is_null(i) || passphrases.is_null(i) || methods.is_null(i) {
                    return Ok(None);
                }
                encrypt(values.value(i), passphrases.value(i), optional(aads, i), methods.value(i)).map(Some)
            })
            .collect::<Result<BinaryArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Binary, DataType::Utf8, DataType::Binary, DataType::Utf8],
            DataType::Binary,
        )]
    }
}

/// `sf_decrypt(value, passphrase, aad, method, try) -> BLOB`: NULL on failure if `try`.
pub struct Decrypt;

impl VArrowScalar for Decrypt {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let (values, aads) = (binary_arg(&input, 0), binary_arg(&input, 2));
        let (passphrases, methods) = (input.column(1).as_string::<i32>(), input.column(3).as_string::<i32>());
        let tries = input.column(4).as_boolean();
        let out = (0..values.len())
            .map(|i| -> Result<Option<Vec<u8>>> {
                if values.is_null(i) || passphrases.is_null(i) || methods.is_null(i) {
                    return Ok(None);
                }
                match decrypt(values.value(i), passphrases.value(i), optional(aads, i), methods.value(i)) {
                    Ok(plain) => Ok(Some(plain)),
                    Err(_) if tries.value(i) => Ok(None),
                    Err(e) => Err(e),
                }
            })
            .collect::<Result<BinaryArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Binary, DataType::Utf8, DataType::Binary, DataType::Utf8, DataType::Boolean],
            DataType::Binary,
        )]
    }
}

fn raw_fields() -> Fields {
    Fields::from(vec![
        Field::new("ciphertext", DataType::Binary, true),
        Field::new("iv", DataType::Binary, true),
        Field::new("tag", DataType::Binary, true),
    ])
}

/// `sf_encrypt_raw(value, key, iv, aad, method) -> STRUCT(ciphertext, iv, tag)`.
/// An empty IV (NULL in SQL) gets a random one.
pub struct EncryptRaw;

impl VArrowScalar for EncryptRaw {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let (values, keys, ivs, aads) =
            (binary_arg(&input, 0), binary_arg(&input, 1), binary_arg(&input, 2), binary_arg(&input, 3));
        let methods = input.column(4).as_string::<i32>();
        let (mut ciphertexts, mut out_ivs, mut tags, mut valid) = (vec![], vec![], vec![], vec![]);
        for i in 0..values.len() {
            if values.is_null(i) || keys.is_null(i) || methods.is_null(i) {
                ciphertexts.push(None);
                out_ivs.push(None);
                tags.push(None);
                valid.push(false);
                continue;
            }
            let (mode, pad) = parse_method(methods.value(i))?;
            let iv = match optional(ivs, i) {
                [] => random_iv(iv_length(mode)),
                iv => iv.to_vec(),
            };
            let (ciphertext, tag) = encrypt_raw(values.value(i), keys.value(i), &iv, optional(aads, i), mode, pad)?;
            ciphertexts.push(Some(ciphertext));
            out_ivs.push(Some(iv));
            tags.push((mode == Mode::Gcm).then_some(tag));
            valid.push(true);
        }
        let columns: Vec<ArrayRef> = [ciphertexts, out_ivs, tags]
            .into_iter()
            .map(|c| Arc::new(c.into_iter().collect::<BinaryArray>()) as ArrayRef)
            .collect();
        Ok(Arc::new(StructArray::new(raw_fields(), columns, Some(BooleanArray::from(valid).values().clone().into()))))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Binary, DataType::Binary, DataType::Binary, DataType::Binary, DataType::Utf8],
            DataType::Struct(raw_fields()),
        )]
    }
}

/// `sf_decrypt_raw(value, key, iv, aad, method, tag, try) -> BLOB`.
pub struct DecryptRaw;

impl VArrowScalar for DecryptRaw {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let (values, keys, ivs, aads) =
            (binary_arg(&input, 0), binary_arg(&input, 1), binary_arg(&input, 2), binary_arg(&input, 3));
        let (methods, tags, tries) =
            (input.column(4).as_string::<i32>(), binary_arg(&input, 5), input.column(6).as_boolean());
        let out = (0..values.len())
            .map(|i| -> Result<Option<Vec<u8>>> {
                if values.is_null(i) || keys.is_null(i) || ivs.is_null(i) || methods.is_null(i) {
                    return Ok(None);
                }
                let decrypted = parse_method(methods.value(i)).and_then(|(mode, pad)| {
                    decrypt_raw(
                        values.value(i),
                        keys.value(i),
                        ivs.value(i),
                        optional(aads, i),
                        optional(tags, i),
                        mode,
                        pad,
                    )
                });
                match decrypted {
                    Ok(plain) => Ok(Some(plain)),
                    Err(_) if tries.value(i) => Ok(None),
                    Err(e) => Err(e),
                }
            })
            .collect::<Result<BinaryArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![
                DataType::Binary,
                DataType::Binary,
                DataType::Binary,
                DataType::Binary,
                DataType::Utf8,
                DataType::Binary,
                DataType::Boolean,
            ],
            DataType::Binary,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02X}")).collect()
    }

    /// Snowflake's ENCRYPT_RAW documentation example.
    #[test]
    fn matches_snowflakes_gcm_example() {
        let key = sha2::Sha256::digest(b"NotSecretEnough");
        let (ciphertext, tag) =
            encrypt_raw(b"Bonjour", &key, b"AlsoNotSecre", b"additional data", Mode::Gcm, true).unwrap();
        assert_eq!(hex(&ciphertext), "CA2F4A383F6F55");
        assert_eq!(hex(&tag), "91F28FBC6A2FE9B213D1C44B8D75D147");
    }

    #[test]
    fn round_trips_every_mode() {
        for method in ["AES-GCM", "aes-cbc/pad:pkcs", "AES-ECB", "AES-CTR", "AES-OFB", "AES-CFB"] {
            let sealed = encrypt(b"Patient tested positive", "passphrase", b"aad", method).unwrap();
            assert_eq!(decrypt(&sealed, "passphrase", b"aad", method).unwrap(), b"Patient tested positive", "{method}");
        }
        let sealed = encrypt(b"penicillin", "p", b"right", "AES-GCM").unwrap();
        assert_eq!(decrypt(&sealed, "p", b"wrong", "AES-GCM").unwrap_err().to_string(), DECRYPTION_FAILED);
    }
}
