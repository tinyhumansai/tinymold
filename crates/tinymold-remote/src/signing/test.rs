//! Unit tests for signing and verifying remote entries, with the encodings
//! pinned against fixed vectors.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const SEED: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn digest(hash: &str, size: i64) -> proto::Digest {
    proto::Digest {
        hash: hash.to_owned(),
        size_bytes: size,
    }
}

fn key() -> proto::Digest {
    digest(&"ab".repeat(32), 32)
}

fn record() -> proto::Digest {
    digest(&"cd".repeat(32), 120)
}

fn signer() -> SigningKey {
    SigningKey::from_hex(SEED).unwrap()
}

fn trusting(signer: &SigningKey) -> TrustedKeys {
    TrustedKeys::from_list(&signer.public_hex()).unwrap()
}

#[test]
fn the_signed_message_is_pinned() {
    assert_eq!(
        String::from_utf8(message(Kind::Action, &key(), &record())).unwrap(),
        format!(
            "tinymold/remote/entry/v1\naction\n{}\n{}/120\n",
            "ab".repeat(32),
            "cd".repeat(32)
        )
    );
    assert!(
        String::from_utf8(message(Kind::Prekey, &key(), &record()))
            .unwrap()
            .contains("\nprekey\n")
    );
}

#[test]
fn a_fixed_seed_gives_a_pinned_key_and_signature() {
    let signer = signer();
    // Cross-checked against the Python `cryptography` package (OpenSSL).
    assert_eq!(
        signer.public_hex(),
        "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8"
    );
    let blob = String::from_utf8(signer.sign(Kind::Action, &key(), &record())).unwrap();
    assert_eq!(
        blob,
        format!(
            "tinymold-signature 1\nkey {}\nsig {}\n",
            signer.public_hex(),
            "b86345bad079ce4f8c28e11f8535982acabc1916b5b1da9b2498804d30524d0e72b0d419ac639e7afeee06b07a2831df9f751efb9bdae2b1b8d35067ff2e1006"
        )
    );
}

#[test]
fn a_signature_by_a_trusted_key_verifies() {
    let signer = signer();
    let blob = signer.sign(Kind::Action, &key(), &record());
    trusting(&signer)
        .verify(Kind::Action, &key(), &record(), &blob)
        .unwrap();
}

#[test]
fn a_signature_is_bound_to_its_kind_key_and_record() {
    let signer = signer();
    let trusted = trusting(&signer);
    let blob = signer.sign(Kind::Action, &key(), &record());
    let other = digest(&"ef".repeat(32), 120);
    for (kind, key, record) in [
        (Kind::Prekey, key(), record()),
        (Kind::Action, other.clone(), record()),
        (Kind::Action, key(), other.clone()),
        (Kind::Action, key(), digest(&"cd".repeat(32), 121)),
    ] {
        let error = trusted.verify(kind, &key, &record, &blob).unwrap_err();
        assert!(
            matches!(error, Error::Untrusted { reason, .. } if reason == "its signature does not match it"),
            "{kind:?} {key:?} {record:?}"
        );
    }
}

#[test]
fn a_signature_by_another_key_is_untrusted() {
    let stranger = SigningKey::generate();
    let blob = stranger.sign(Kind::Action, &key(), &record());
    let error = trusting(&signer())
        .verify(Kind::Action, &key(), &record(), &blob)
        .unwrap_err();
    assert!(
        matches!(error, Error::Untrusted { reason, .. } if reason == "it is signed by a key that is not trusted")
    );
}

#[test]
fn a_malformed_signature_is_untrusted() {
    let trusted = trusting(&signer());
    for blob in [
        &b"\xff\xfe"[..],
        b"",
        b"tinymold-signature 2\nkey 00\nsig 00\n",
        b"tinymold-signature 1\nkey zz\nsig 00\n",
        b"tinymold-signature 1\nsig 00\nkey 00\n",
    ] {
        let error = trusted
            .verify(Kind::Action, &key(), &record(), blob)
            .unwrap_err();
        assert!(matches!(error, Error::Untrusted { .. }), "{blob:?}");
    }
    let mut good = signer().sign(Kind::Action, &key(), &record());
    good.extend_from_slice(b"extra\n");
    assert!(
        trusted
            .verify(Kind::Action, &key(), &record(), &good)
            .is_err()
    );
}

#[test]
fn keys_round_trip_through_hex() {
    let key = SigningKey::generate();
    let again = SigningKey::from_hex(&key.to_hex().to_uppercase()).unwrap();
    assert_eq!(again, key);
    assert_ne!(SigningKey::generate(), key);
    assert!(!format!("{key:?}").contains(&key.to_hex()));
}

#[test]
fn bad_keys_are_refused() {
    for text in ["", "00", &"g".repeat(64), &"0".repeat(66)] {
        assert!(matches!(
            SigningKey::from_hex(text),
            Err(Error::InvalidSigningKey)
        ));
    }
    assert!(matches!(
        TrustedKeys::from_list(" , "),
        Err(Error::InvalidTrustedKey(_))
    ));
    assert!(matches!(
        TrustedKeys::from_list(&format!("{},xyz", signer().public_hex())),
        Err(Error::InvalidTrustedKey(item)) if item == "xyz"
    ));
}

#[test]
fn a_list_of_trusted_keys_accepts_any_of_them() {
    let first = SigningKey::generate();
    let second = SigningKey::generate();
    let trusted =
        TrustedKeys::from_list(&format!("{}, {}", first.public_hex(), second.public_hex()))
            .unwrap();
    for signer in [&first, &second] {
        let blob = signer.sign(Kind::Prekey, &key(), &record());
        trusted
            .verify(Kind::Prekey, &key(), &record(), &blob)
            .unwrap();
    }
}
