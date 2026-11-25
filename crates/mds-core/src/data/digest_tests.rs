use super::digest::{hex, hmac_sha256, sha256};

fn sha_hex(parts: &[&[u8]]) -> String {
    hex(&sha256(parts))
}

fn hmac_hex(key: &[u8], message: &[u8]) -> String {
    hex(&hmac_sha256(key, &[message]))
}

// FIPS 180-4 examples (NIST CSRC example values for SHA-256).
#[test]
fn sha256_matches_the_fips_180_4_examples() {
    assert_eq!(
        sha_hex(&[b"abc"]),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha_hex(&[b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"]),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    let million = vec![b'a'; 1_000_000];
    assert_eq!(
        sha_hex(&[&million]),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn sha256_of_the_empty_message() {
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(sha_hex(&[]), empty);
    assert_eq!(sha_hex(&[b""]), empty);
    assert_eq!(sha_hex(&[b"", b""]), empty);
}

/// The digest of the parts is the digest of their concatenation, for each split point of
/// a two-block message and for parts that cross a block end.
#[test]
fn sha256_of_parts_is_the_digest_of_their_concatenation() {
    let message = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    let whole = sha256(&[message]);
    for split in 0..=message.len() {
        let (head, tail) = message.split_at(split);
        assert_eq!(sha256(&[head, tail]), whole, "split at {split}");
    }
    let long = vec![b'a'; 1_000_000];
    let pieces: Vec<&[u8]> = long.chunks(63).collect();
    assert_eq!(
        hex(&sha256(&pieces)),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

// RFC 4231 test cases 1, 2, 3, 4, 6 and 7 (case 5 tests output truncation).
#[test]
fn hmac_sha256_matches_rfc_4231() {
    assert_eq!(
        hmac_hex(&[0x0b; 20], b"Hi There"),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    assert_eq!(
        hmac_hex(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    assert_eq!(
        hmac_hex(&[0xaa; 20], &[0xdd; 50]),
        "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
    );
    let key: Vec<u8> = (0x01..=0x19).collect();
    assert_eq!(
        hmac_hex(&key, &[0xcd; 50]),
        "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b"
    );
    let long_key = [0xaa; 131];
    assert_eq!(
        hmac_hex(
            &long_key,
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        ),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
    assert_eq!(
        hmac_hex(
            &long_key,
            b"This is a test using a larger than block-size key and a larger than \
              block-size data. The key needs to be hashed before being used by the \
              HMAC algorithm."
        ),
        "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2"
    );
}

#[test]
fn hmac_of_parts_is_the_hmac_of_their_concatenation() {
    let key = [7u8; 32];
    assert_eq!(
        hmac_sha256(&key, &[b"Hi ", b"", b"There"]),
        hmac_sha256(&key, &[b"Hi There"])
    );
    assert_ne!(
        hmac_sha256(&key, &[b"Hi There"]),
        hmac_sha256(&[8u8; 32], &[b"Hi There"])
    );
}
