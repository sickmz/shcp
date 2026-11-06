//! Compatibility for traditional encrypted PEM, including EC keys.
//! russh's traditional PEM decoder only decrypts AES-128 RSA keys.
use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use russh::keys::{self, PrivateKey};
use zeroize::Zeroizing;

pub(super) fn encrypted(text: &str) -> bool {
    text.contains("Proc-Type: 4,ENCRYPTED") || text.contains("BEGIN ENCRYPTED PRIVATE KEY")
}
pub(super) fn load(text: &str, password: Option<&str>) -> Result<PrivateKey, keys::Error> {
    let Some(header) = text
        .lines()
        .find_map(|line| line.strip_prefix("DEK-Info: "))
    else {
        return keys::decode_secret_key(text, password);
    };
    let password = password.ok_or(keys::Error::KeyIsEncrypted)?;
    let (cipher, iv_hex) = header
        .trim()
        .split_once(',')
        .ok_or(keys::Error::CouldNotReadKey)?;
    let key_len = match cipher {
        "AES-128-CBC" => 16,
        "AES-192-CBC" => 24,
        "AES-256-CBC" => 32,
        _ => return Err(keys::Error::CouldNotReadKey),
    };
    if iv_hex.len() != 32 || !iv_hex.is_ascii() {
        return Err(keys::Error::KeyIsCorrupt);
    }
    let mut iv = [0u8; 16];
    for (i, byte) in iv.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&iv_hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| keys::Error::KeyIsCorrupt)?;
    }
    // OpenSSL's traditional PEM encryption uses EVP_BytesToKey with MD5 and one iteration.
    let mut key = Zeroizing::new(Vec::new());
    let mut previous = Zeroizing::new(Vec::new());
    while key.len() < key_len {
        let mut hash = md5::Context::new();
        hash.consume(previous.as_slice());
        hash.consume(password.as_bytes());
        hash.consume(&iv[..8]);
        previous.clear();
        previous.extend_from_slice(&hash.finalize().0);
        key.extend_from_slice(&previous);
    }
    let encoded = text
        .lines()
        .filter(|line| !line.starts_with("-----") && !line.contains(':') && !line.is_empty())
        .collect::<String>();
    let mut bytes = Zeroizing::new(
        STANDARD
            .decode(encoded)
            .map_err(|_| keys::Error::KeyIsCorrupt)?,
    );
    macro_rules! decrypt {
        ($aes:ty) => {
            cbc::Decryptor::<$aes>::new_from_slices(&key[..key_len], &iv)
                .map_err(|_| keys::Error::KeyIsCorrupt)?
                .decrypt_padded::<Pkcs7>(&mut bytes)
                .map_err(|_| keys::Error::KeyIsCorrupt)?
        };
    }
    let clear = match cipher {
        "AES-128-CBC" => decrypt!(aes::Aes128),
        "AES-192-CBC" => decrypt!(aes::Aes192),
        _ => decrypt!(aes::Aes256),
    };
    let label = text
        .lines()
        .find(|line| line.starts_with("-----BEGIN "))
        .ok_or(keys::Error::CouldNotReadKey)?;
    let end = label.replace("BEGIN", "END");
    let encoded = Zeroizing::new(STANDARD.encode(clear));
    let body = Zeroizing::new(format!("{label}\n{}\n{end}\n", encoded.as_str()));
    keys::decode_secret_key(&body, None)
}
