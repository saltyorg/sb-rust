//! OpenSSL's `enc -aes-256-cbc -pbkdf2` format (ops spec, "Decryption"),
//! as Saltbox's backup role writes its uploads with OpenSSL 3's defaults:
//! `Salted__`, an 8-byte salt, then the ciphertext; the key (32 bytes)
//! and IV (16) from PBKDF2-HMAC-SHA256 over the password and the salt,
//! 10000 iterations; AES-256-CBC with PKCS#7 padding. The checks and
//! their texts are sb-go's (cmd/restore.go, `decryptFile`). Why the
//! header and length are checked here: no crate reads OpenSSL's `enc`
//! container (the `openssl` crate's `symm::decrypt` takes the key and IV
//! too), and cipher's own length error is the padding error, while sb-go
//! keeps "invalid ciphertext length" apart from a wrong password.

use aes::Aes256;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeDecrypt as _, KeyIvInit as _};
use sha2::Sha256;
use zeroize::Zeroizing;

/// OpenSSL's header of a salted file.
const MAGIC: &[u8] = b"Salted__";

/// The salt's length after [`MAGIC`].
const SALT: usize = 8;

/// OpenSSL 3's `enc -pbkdf2` iteration count when `-iter` isn't given.
const ITERATIONS: u32 = 10_000;

/// AES's block size.
const BLOCK: usize = 16;

/// Why a file didn't decrypt, in sb-go's words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecryptError {
    /// No `Salted__` header, or too short to hold the salt.
    #[error("invalid ciphertext format (missing 'Salted__' prefix)")]
    Format,
    /// No ciphertext, or not a whole number of blocks.
    #[error("invalid ciphertext length: {0}")]
    Length(usize),
    /// The padding didn't check out: the likely sign of a wrong password
    /// (CBC has no other; the caller says so, as sb-go).
    #[error("invalid padding")]
    Padding,
}

/// `data`, a file in OpenSSL's salted format, decrypted with `password`.
///
/// The key and IV are derived into a buffer cleared on drop; the
/// ciphertext is copied into another and decrypted there in place,
/// cbc's `decrypt_padded` with PKCS#7
/// (<https://docs.rs/cbc/0.2.1/cbc/>, the crate's in-place example), so
/// a wrong password's garbage is cleared too, not only a good plaintext.
/// Why not `decrypt_padded_vec`: its buffer would be a plain `Vec` on
/// the error path. PBKDF2 is `pbkdf2_hmac::<Sha256>`
/// (<https://docs.rs/pbkdf2/0.13.0/pbkdf2/fn.pbkdf2_hmac.html>).
///
/// Clearing is best effort: sb's buffers (the key and IV, the plaintext
/// or the garbage), AES's key schedule, CBC's IV and the SHA-256 state
/// are cleared on drop; hmac 0.13's padded key block and PBKDF2's
/// intermediate blocks are stack temporaries the crates don't clear (ops
/// spec, Known limits).
///
/// # Errors
///
/// [`DecryptError`]: no header, a length that isn't whole blocks, or
/// padding that doesn't check out (a wrong password, most likely).
///
/// # Panics
///
/// Never: the key and IV slices are the 32 and 16 bytes AES-256-CBC takes.
pub fn decrypt(data: &[u8], password: &[u8]) -> Result<Zeroizing<Vec<u8>>, DecryptError> {
    let rest = data.strip_prefix(MAGIC).ok_or(DecryptError::Format)?;
    let (salt, ciphertext) = rest.split_at_checked(SALT).ok_or(DecryptError::Format)?;
    if ciphertext.is_empty() || ciphertext.len() % BLOCK != 0 {
        return Err(DecryptError::Length(ciphertext.len()));
    }
    let mut key_iv = Zeroizing::new([0u8; 48]);
    pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, ITERATIONS, key_iv.as_mut());
    let (key, iv) = key_iv.split_at(32);
    let decryptor = cbc::Decryptor::<Aes256>::new_from_slices(key, iv)
        .expect("PBKDF2 gave AES-256-CBC its 32 key and 16 IV bytes");
    let mut buffer = Zeroizing::new(ciphertext.to_vec());
    let length = decryptor
        .decrypt_padded::<Pkcs7>(buffer.as_mut_slice())
        .map_err(|_| DecryptError::Padding)?
        .len();
    buffer.truncate(length);
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The password the fixtures were made with
    /// (tests/fixtures/restore/made-with.sh).
    const PASSWORD: &[u8] = b"synthetic-restore-pass";

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/restore/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(&path).unwrap_or_else(|err| panic!("{path}: {err}"))
    }

    /// Each openssl-made file decrypts to its plaintext: one block, an
    /// empty plaintext (a block of padding), and many blocks.
    #[test]
    fn openssl_made_files_decrypt_to_their_plaintext() {
        for name in ["settings.yml", "empty.yml", "long.conf"] {
            let plaintext: Zeroizing<Vec<u8>> =
                decrypt(&fixture(&format!("{name}.enc")), PASSWORD).expect(name);
            assert_eq!(*plaintext, fixture(name), "{name}");
        }
    }

    /// A wrong password gives the padding error, the caller's "wrong
    /// password" sign (these passwords' garbage happens to fail the check
    /// on every fixture; any one could pass with odds of about 1 in 256).
    #[test]
    fn a_wrong_password_is_the_padding_error() {
        for name in ["settings.yml.enc", "empty.yml.enc", "long.conf.enc"] {
            for password in [&b"wrong"[..], b"", b"synthetic-restore-pas"] {
                assert_eq!(
                    decrypt(&fixture(name), password),
                    Err(DecryptError::Padding),
                    "{name} with {password:?}"
                );
            }
        }
    }

    /// No header, or too short for the salt: the format error; no
    /// ciphertext, or a partial block: the length error, with its length.
    #[test]
    fn a_malformed_file_is_refused_before_decrypting() {
        let good = fixture("settings.yml.enc");
        assert_eq!(decrypt(&good[8..], PASSWORD), Err(DecryptError::Format));
        assert_eq!(decrypt(b"", PASSWORD), Err(DecryptError::Format));
        assert_eq!(
            decrypt(b"Salted__1234567", PASSWORD),
            Err(DecryptError::Format)
        );
        assert_eq!(decrypt(&good[..16], PASSWORD), Err(DecryptError::Length(0)));
        assert_eq!(
            decrypt(&good[..good.len() - 1], PASSWORD),
            Err(DecryptError::Length(good.len() - 17))
        );
        assert_eq!(
            decrypt(&good[..33], PASSWORD),
            Err(DecryptError::Length(17))
        );
    }

    /// The errors read as sb-go's.
    #[test]
    fn the_errors_are_sb_gos_words() {
        assert_eq!(
            DecryptError::Format.to_string(),
            "invalid ciphertext format (missing 'Salted__' prefix)"
        );
        assert_eq!(
            DecryptError::Length(17).to_string(),
            "invalid ciphertext length: 17"
        );
        assert_eq!(DecryptError::Padding.to_string(), "invalid padding");
    }
}
