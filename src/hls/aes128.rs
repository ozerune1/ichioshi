//! Decrypting HLS segments encrypted with `METHOD=AES-128`: the whole
//! segment, AES-128 in CBC mode with PKCS#7 padding, under the key at the
//! playlist's key URI and its IV (see `m3u8::Key::iv_for`).
//!
//! CBC is done here, on the `aes` crate's block cipher: each block is
//! decrypted, then XORed with the ciphertext block before it (the IV for
//! the first). The blocks are decrypted all at once, which lets the cipher
//! use its parallel (AES-NI) path.

use aes::Aes128;
use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use anyhow::{Result, ensure};

const BLOCK: usize = 16;

/// `data` decrypted, its padding taken off. Fails if it isn't whole blocks,
/// or the padding is wrong (which is what a wrong key or IV looks like).
pub fn decrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    ensure!(!data.is_empty(), "nothing to decrypt");
    ensure!(
        data.len().is_multiple_of(BLOCK),
        "encrypted data isn't whole 16-byte blocks ({} bytes)",
        data.len()
    );
    let mut out = decrypt_blocks(key, iv, data);
    let pad = out.last().copied().map_or(0, usize::from);
    let tail = out.len().checked_sub(pad).map(|at| &out[at..]);
    ensure!(
        (1..=BLOCK).contains(&pad)
            && tail.is_some_and(|t| t.iter().all(|&b| usize::from(b) == pad)),
        "bad padding after decrypting (wrong key or IV?)"
    );
    out.truncate(out.len() - pad);
    Ok(out)
}

/// CBC decryption of whole blocks, padding left on.
fn decrypt_blocks(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let cipher = Aes128::new(&GenericArray::from(*key));
    let (whole, _) = data.as_chunks::<BLOCK>();
    let mut blocks: Vec<_> = whole.iter().map(|b| GenericArray::from(*b)).collect();
    cipher.decrypt_blocks(&mut blocks);
    let previous = std::iter::once(iv).chain(whole);
    let mut out = Vec::with_capacity(data.len());
    for (block, prev) in blocks.iter().zip(previous) {
        out.extend(block.iter().zip(prev).map(|(b, p)| b ^ p));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncrypt;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// AES-128-CBC with PKCS#7, the other way, to check against.
    fn encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Vec<u8> {
        let cipher = Aes128::new(&GenericArray::from(*key));
        let pad = BLOCK - plain.len() % BLOCK;
        let mut data = plain.to_vec();
        data.extend(std::iter::repeat_n(pad as u8, pad));
        let mut prev = *iv;
        for chunk in data.as_chunks_mut::<BLOCK>().0 {
            for (b, p) in chunk.iter_mut().zip(prev) {
                *b ^= p;
            }
            cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
            prev = *chunk;
        }
        data
    }

    #[test]
    fn round_trips() {
        let key: [u8; 16] = std::array::from_fn(|i| (i * 7 + 3) as u8);
        let iv: [u8; 16] = std::array::from_fn(|i| (200 - i) as u8);
        for len in [0, 1, 15, 16, 17, 31, 32, 188, 1000, 65_536] {
            let plain: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let enc = encrypt(&key, &iv, &plain);
            assert_eq!(enc.len() % 16, 0);
            if len > 0 {
                assert_ne!(&enc[..len.min(16)], &plain[..len.min(16)]);
            }
            assert_eq!(decrypt(&key, &iv, &enc).unwrap(), plain, "length {len}");
        }
    }

    /// NIST SP 800-38A, F.2.2 (CBC-AES128.Decrypt).
    #[test]
    fn matches_the_nist_vectors() {
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let cipher = hex(concat!(
            "7649abac8119b246cee98e9b12e9197d",
            "5086cb9b507219ee95db113a917678b2",
            "73bed6b8e3c1743b7116e69e22229516",
            "3ff1caa1681fac09120eca307586e1a7",
        ));
        let plain = hex(concat!(
            "6bc1bee22e409f96e93d7e117393172a",
            "ae2d8a571e03ac9c9eb76fac45af8e51",
            "30c81c46a35ce411e5fbc1191a0a52ef",
            "f69f2445df4f9b17ad2b417be66c3710",
        ));
        assert_eq!(decrypt_blocks(&key, &iv, &cipher), plain);
    }

    #[test]
    fn rejects_bad_input() {
        let key = [1u8; 16];
        let iv = [2u8; 16];
        assert!(decrypt(&key, &iv, &[]).is_err());
        assert!(decrypt(&key, &iv, &[0; 15]).is_err());
        assert!(decrypt(&key, &iv, &[0; 33]).is_err());
        let enc = encrypt(&key, &iv, b"seventeen bytes!!");
        assert_eq!(enc.len(), 32);
        // The wrong key breaks the padding (almost always; this one does).
        assert!(decrypt(&[9; 16], &iv, &enc).is_err());
        let mut flipped = enc.clone();
        flipped[31] ^= 1;
        assert!(decrypt(&key, &iv, &flipped).is_err());
        // Each kind of bad padding: 0, over 16, unequal bytes.
        for last in [
            [0u8; 16].to_vec(),
            {
                let mut b = vec![5u8; 16];
                b[15] = 17;
                b
            },
            {
                let mut b = vec![3u8; 16];
                b[14] = 4;
                b
            },
        ] {
            let mut plain = vec![0u8; 16];
            plain.extend(&last);
            // Encrypt without padding: the last block is the "padding".
            let enc = encrypt(&key, &iv, &plain);
            let raw = &enc[..32];
            assert!(decrypt(&key, &iv, raw).is_err(), "{last:?}");
        }
        // A full block of padding is fine.
        let enc = encrypt(&key, &iv, &[7u8; 16]);
        assert_eq!(enc.len(), 32);
        assert_eq!(decrypt(&key, &iv, &enc).unwrap(), [7u8; 16]);
    }
}
