//! CrabScheme stdlib module: `(crab crypto)`.
//!
//! Modern, misuse-resistant cryptography beyond the digests in
//! `(crab hash)` — secure randomness, authenticated encryption, and
//! digital signatures. The `(crab …)` answer to Go's `crypto/*`,
//! Python's `secrets` + `cryptography`, and Clojure's `buddy`.
//!
//! All primitives are pure-Rust (RustCrypto + dalek) — no OpenSSL,
//! no C — so the module cross-compiles to `wasm32-wasip1`.
//!
//! Cryptographic material (keys, nonces, signatures) is passed and
//! returned as **bytevectors**; data inputs (plaintext, message,
//! aad) accept a bytevector or a string (hashed as UTF-8 bytes).
//! Pair with `(crab base)` / `(crab hash)` for hex rendering and
//! digests.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `crypto-random-bytes`   | n                       | bytevector | `n` CSPRNG bytes. |
//! | `crypto-random-token`   | n                       | string     | `n` random bytes, URL-safe base64 (no pad). |
//! | `crypto-constant-time=?`| a b                     | boolean    | Timing-safe byte compare. |
//! | `crypto-aead-keygen`    | —                       | bytevector | Fresh 32-byte ChaCha20-Poly1305 key. |
//! | `crypto-aead-nonce`     | —                       | bytevector | Fresh 12-byte nonce. |
//! | `crypto-aead-encrypt`   | key nonce plaintext [aad] | bytevector | Ciphertext ‖ 16-byte tag. |
//! | `crypto-aead-decrypt`   | key nonce ciphertext [aad] | bytevector | Plaintext; raises on auth failure. |
//! | `crypto-ed25519-keypair`| —                       | #(secret public) | 32-byte secret + 32-byte public. |
//! | `crypto-ed25519-sign`   | secret message          | bytevector | 64-byte signature. |
//! | `crypto-ed25519-verify` | public message signature | boolean   | Strict verification. |
//! | `crypto-x25519-keypair` | —                       | #(secret public) | 32-byte X25519 keypair. |
//! | `crypto-x25519-shared`  | secret their-public     | bytevector | 32-byte ECDH shared secret (run through HKDF). |
//! | `crypto-hkdf-sha256`    | ikm salt info length    | bytevector | Derive `length` bytes (≤ 8160). |
//! | `crypto-password-hash`  | password                | string     | Argon2id PHC string (random salt). |
//! | `crypto-password-verify`| password phc            | boolean    | Verify against a PHC string. |
//!
//! ## AEAD usage
//!
//! ChaCha20-Poly1305: a 32-byte key and a 12-byte nonce. **A
//! (key, nonce) pair must never be reused** — generate a fresh nonce
//! per message with `crypto-aead-nonce` and transmit it alongside the
//! ciphertext (the nonce is not secret). Optional associated data
//! (`aad`) is authenticated but not encrypted; the same `aad` must be
//! supplied to decrypt.
//!
//! ```scheme
//! (import (crab crypto))
//! (define key (crypto-aead-keygen))
//! (define nonce (crypto-aead-nonce))
//! (define ct (crypto-aead-encrypt key nonce "secret message"))
//! (utf8->string (crypto-aead-decrypt key nonce ct))   ; => "secret message"
//! ```
//!
//! ## Signatures
//!
//! ```scheme
//! (define kp (crypto-ed25519-keypair))
//! (define sk (vector-ref kp 0))
//! (define pk (vector-ref kp 1))
//! (define sig (crypto-ed25519-sign sk "msg"))
//! (crypto-ed25519-verify pk "msg" sig)        ; => #t
//! (crypto-ed25519-verify pk "tampered" sig)   ; => #f
//! ```
//!
//! ## Key agreement, derivation, and passwords
//!
//! X25519 ECDH: both peers compute the same shared secret; run it through
//! HKDF before using it as a key. Argon2id is for hashing user passwords
//! at rest.
//!
//! ```scheme
//! ;; ECDH + HKDF
//! (define a (crypto-x25519-keypair))
//! (define b (crypto-x25519-keypair))
//! (define shared (crypto-x25519-shared (vector-ref a 0) (vector-ref b 1)))
//! (define key (crypto-hkdf-sha256 shared "" "chat-v1" 32))   ; 32-byte AEAD key
//!
//! ;; Passwords
//! (define h (crypto-password-hash "hunter2"))
//! (crypto-password-verify "hunter2" h)        ; => #t
//! ```

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

use argon2::password_hash::{PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};

const AEAD_KEY_LEN: usize = 32;
const AEAD_NONCE_LEN: usize = 12;
const ED25519_KEY_LEN: usize = 32;
const ED25519_SIG_LEN: usize = 64;
const X25519_KEY_LEN: usize = 32;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("crypto-random-bytes", crypto_random_bytes),
        UntypedProc::new("crypto-random-token", crypto_random_token),
        UntypedProc::new("crypto-constant-time=?", crypto_constant_time_eq),
        UntypedProc::new("crypto-aead-keygen", crypto_aead_keygen),
        UntypedProc::new("crypto-aead-nonce", crypto_aead_nonce),
        UntypedProc::new("crypto-aead-encrypt", crypto_aead_encrypt),
        UntypedProc::new("crypto-aead-decrypt", crypto_aead_decrypt),
        UntypedProc::new("crypto-ed25519-keypair", crypto_ed25519_keypair),
        UntypedProc::new("crypto-ed25519-sign", crypto_ed25519_sign),
        UntypedProc::new("crypto-ed25519-verify", crypto_ed25519_verify),
        UntypedProc::new("crypto-x25519-keypair", crypto_x25519_keypair),
        UntypedProc::new("crypto-x25519-shared", crypto_x25519_shared),
        UntypedProc::new("crypto-hkdf-sha256", crypto_hkdf_sha256),
        UntypedProc::new("crypto-password-hash", crypto_password_hash),
        UntypedProc::new("crypto-password-verify", crypto_password_verify),
    ]
}

// ----- helpers -----

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
}

fn fail(msg: String) -> FfiError {
    FfiError::HostFailure(msg)
}

fn bv_value(b: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(b)))
}

/// Fill a buffer of `n` bytes from the OS CSPRNG.
fn random_bytes(name: &str, n: usize) -> Result<Vec<u8>, FfiError> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf)
        .map_err(|e| fail(format!("{}: CSPRNG unavailable: {}", name, e)))?;
    Ok(buf)
}

/// Read a non-negative integer count.
fn expect_count(name: &str, args: &[Value], idx: usize) -> Result<usize, FfiError> {
    match args.get(idx) {
        Some(
            nv @ (Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)),
        ) => {
            let n = nv.as_number().unwrap();
            let f = n.to_f64();
            if !f.is_finite() || f < 0.0 || f.fract() != 0.0 {
                return Err(fail(format!(
                    "{}: expected a non-negative integer count",
                    name
                )));
            }
            Ok(f as usize)
        }
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "non-negative integer",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

/// Strict bytevector argument (cryptographic material — no string
/// coercion, so a wrong-typed key is caught rather than silently
/// hashed as text).
fn expect_bv(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>, FfiError> {
    match args.get(idx) {
        Some(Value::ByteVector(bv)) => Ok(bv.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "bytevector",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

/// Data argument: a bytevector, or a string taken as its UTF-8 bytes.
fn expect_bytes(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().as_bytes().to_vec()),
        Some(Value::ByteVector(bv)) => Ok(bv.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string or bytevector",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

/// Optional data argument (defaults to empty when absent).
fn optional_bytes(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>, FfiError> {
    if idx >= args.len() {
        return Ok(Vec::new());
    }
    expect_bytes(name, args, idx)
}

fn require_len(name: &str, what: &str, bytes: &[u8], want: usize) -> Result<(), FfiError> {
    if bytes.len() != want {
        return Err(fail(format!(
            "{}: {} must be exactly {} bytes, got {}",
            name,
            what,
            want,
            bytes.len()
        )));
    }
    Ok(())
}

// ----- randomness -----

fn crypto_random_bytes(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("crypto-random-bytes", "1", args.len()));
    }
    let n = expect_count("crypto-random-bytes", args, 0)?;
    Ok(bv_value(random_bytes("crypto-random-bytes", n)?))
}

fn crypto_random_token(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("crypto-random-token", "1", args.len()));
    }
    let n = expect_count("crypto-random-token", args, 0)?;
    let bytes = random_bytes("crypto-random-token", n)?;
    Ok(Value::string(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
    ))
}

fn crypto_constant_time_eq(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity("crypto-constant-time=?", "2", args.len()));
    }
    let a = expect_bytes("crypto-constant-time=?", args, 0)?;
    let b = expect_bytes("crypto-constant-time=?", args, 1)?;
    // subtle's slice ConstantTimeEq returns false for unequal lengths
    // without branching on the contents.
    Ok(Value::Boolean(a.ct_eq(&b).into()))
}

// ----- AEAD (ChaCha20-Poly1305) -----

fn crypto_aead_keygen(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("crypto-aead-keygen", "0", args.len()));
    }
    Ok(bv_value(random_bytes("crypto-aead-keygen", AEAD_KEY_LEN)?))
}

fn crypto_aead_nonce(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("crypto-aead-nonce", "0", args.len()));
    }
    Ok(bv_value(random_bytes("crypto-aead-nonce", AEAD_NONCE_LEN)?))
}

fn aead_cipher(name: &str, key: &[u8]) -> Result<ChaCha20Poly1305, FfiError> {
    require_len(name, "key", key, AEAD_KEY_LEN)?;
    Ok(ChaCha20Poly1305::new(Key::from_slice(key)))
}

fn crypto_aead_encrypt(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(arity("crypto-aead-encrypt", "3 or 4", args.len()));
    }
    let key = expect_bv("crypto-aead-encrypt", args, 0)?;
    let nonce = expect_bv("crypto-aead-encrypt", args, 1)?;
    let plaintext = expect_bytes("crypto-aead-encrypt", args, 2)?;
    let aad = optional_bytes("crypto-aead-encrypt", args, 3)?;
    require_len("crypto-aead-encrypt", "nonce", &nonce, AEAD_NONCE_LEN)?;
    let cipher = aead_cipher("crypto-aead-encrypt", &key)?;
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| fail("crypto-aead-encrypt: encryption failed".into()))?;
    Ok(bv_value(ct))
}

fn crypto_aead_decrypt(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(arity("crypto-aead-decrypt", "3 or 4", args.len()));
    }
    let key = expect_bv("crypto-aead-decrypt", args, 0)?;
    let nonce = expect_bv("crypto-aead-decrypt", args, 1)?;
    let ciphertext = expect_bv("crypto-aead-decrypt", args, 2)?;
    let aad = optional_bytes("crypto-aead-decrypt", args, 3)?;
    require_len("crypto-aead-decrypt", "nonce", &nonce, AEAD_NONCE_LEN)?;
    let cipher = aead_cipher("crypto-aead-decrypt", &key)?;
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        // A decryption error is the expected signal for tampering or a
        // wrong key/nonce/aad — surface it without leaking which.
        .map_err(|_| fail("crypto-aead-decrypt: authentication failed".into()))?;
    Ok(bv_value(pt))
}

// ----- Ed25519 signatures -----

fn crypto_ed25519_keypair(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("crypto-ed25519-keypair", "0", args.len()));
    }
    let mut seed = [0u8; ED25519_KEY_LEN];
    getrandom::getrandom(&mut seed)
        .map_err(|e| fail(format!("crypto-ed25519-keypair: CSPRNG unavailable: {}", e)))?;
    let sk = SigningKey::from_bytes(&seed);
    let pk = sk.verifying_key();
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
        vec![
            bv_value(sk.to_bytes().to_vec()),
            bv_value(pk.to_bytes().to_vec()),
        ],
    ))))
}

fn crypto_ed25519_sign(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity("crypto-ed25519-sign", "2", args.len()));
    }
    let secret = expect_bv("crypto-ed25519-sign", args, 0)?;
    let message = expect_bytes("crypto-ed25519-sign", args, 1)?;
    require_len(
        "crypto-ed25519-sign",
        "secret key",
        &secret,
        ED25519_KEY_LEN,
    )?;
    let mut seed = [0u8; ED25519_KEY_LEN];
    seed.copy_from_slice(&secret);
    let sk = SigningKey::from_bytes(&seed);
    let sig: Signature = sk.sign(&message);
    Ok(bv_value(sig.to_bytes().to_vec()))
}

fn crypto_ed25519_verify(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 3 {
        return Err(arity("crypto-ed25519-verify", "3", args.len()));
    }
    let public = expect_bv("crypto-ed25519-verify", args, 0)?;
    let message = expect_bytes("crypto-ed25519-verify", args, 1)?;
    let sig_bytes = expect_bv("crypto-ed25519-verify", args, 2)?;
    require_len(
        "crypto-ed25519-verify",
        "public key",
        &public,
        ED25519_KEY_LEN,
    )?;
    require_len(
        "crypto-ed25519-verify",
        "signature",
        &sig_bytes,
        ED25519_SIG_LEN,
    )?;
    let mut pk_arr = [0u8; ED25519_KEY_LEN];
    pk_arr.copy_from_slice(&public);
    // An invalid public-key encoding is a verification failure, not an
    // error: return #f rather than raising.
    let vk = match VerifyingKey::from_bytes(&pk_arr) {
        Ok(vk) => vk,
        Err(_) => return Ok(Value::Boolean(false)),
    };
    let mut sig_arr = [0u8; ED25519_SIG_LEN];
    sig_arr.copy_from_slice(&sig_bytes);
    let sig = Signature::from_bytes(&sig_arr);
    Ok(Value::Boolean(vk.verify(&message, &sig).is_ok()))
}

// ----- X25519 key agreement (ECDH) -----

fn crypto_x25519_keypair(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(arity("crypto-x25519-keypair", "0", args.len()));
    }
    let mut seed = [0u8; X25519_KEY_LEN];
    getrandom::getrandom(&mut seed)
        .map_err(|e| fail(format!("crypto-x25519-keypair: CSPRNG unavailable: {}", e)))?;
    let secret = StaticSecret::from(seed);
    let public = X25519Public::from(&secret);
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
        vec![
            bv_value(secret.to_bytes().to_vec()),
            bv_value(public.to_bytes().to_vec()),
        ],
    ))))
}

/// `(crypto-x25519-shared secret their-public)` — Diffie-Hellman shared
/// secret. Both peers derive the same 32 bytes from their own secret and
/// the other's public key. Run the result through `crypto-hkdf-sha256`
/// before using it as a symmetric key (the raw DH output is not uniform).
fn crypto_x25519_shared(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity("crypto-x25519-shared", "2", args.len()));
    }
    let secret = expect_bv("crypto-x25519-shared", args, 0)?;
    let their = expect_bv("crypto-x25519-shared", args, 1)?;
    require_len(
        "crypto-x25519-shared",
        "secret key",
        &secret,
        X25519_KEY_LEN,
    )?;
    require_len("crypto-x25519-shared", "public key", &their, X25519_KEY_LEN)?;
    let mut sk = [0u8; X25519_KEY_LEN];
    sk.copy_from_slice(&secret);
    let mut pk = [0u8; X25519_KEY_LEN];
    pk.copy_from_slice(&their);
    let shared = StaticSecret::from(sk).diffie_hellman(&X25519Public::from(pk));
    Ok(bv_value(shared.to_bytes().to_vec()))
}

// ----- HKDF-SHA256 key derivation -----

/// `(crypto-hkdf-sha256 ikm salt info length)` — derive `length` bytes of
/// key material from input keying material `ikm`, an optional `salt`, and
/// context `info`. `length` is at most 255*32 = 8160.
fn crypto_hkdf_sha256(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 4 {
        return Err(arity("crypto-hkdf-sha256", "4", args.len()));
    }
    let ikm = expect_bytes("crypto-hkdf-sha256", args, 0)?;
    let salt = expect_bytes("crypto-hkdf-sha256", args, 1)?;
    let info = expect_bytes("crypto-hkdf-sha256", args, 2)?;
    let length = expect_count("crypto-hkdf-sha256", args, 3)?;
    let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut okm = vec![0u8; length];
    hk.expand(&info, &mut okm).map_err(|_| {
        fail("crypto-hkdf-sha256: output length too large (max 255*32 = 8160 bytes)".to_string())
    })?;
    Ok(bv_value(okm))
}

// ----- Argon2 password hashing -----

/// `(crypto-password-hash password)` — hash a password with Argon2id
/// (random salt), returning a self-describing PHC string suitable for
/// storage. Verify it later with `crypto-password-verify`.
fn crypto_password_hash(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("crypto-password-hash", "1", args.len()));
    }
    let password = expect_bytes("crypto-password-hash", args, 0)?;
    let mut salt_bytes = [0u8; 16];
    getrandom::getrandom(&mut salt_bytes)
        .map_err(|e| fail(format!("crypto-password-hash: CSPRNG unavailable: {}", e)))?;
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|e| fail(format!("crypto-password-hash: {}", e)))?;
    let hash = Argon2::default()
        .hash_password(&password, &salt)
        .map_err(|e| fail(format!("crypto-password-hash: {}", e)))?;
    Ok(Value::string(hash.to_string()))
}

/// `(crypto-password-verify password phc)` — check `password` against a
/// PHC string from `crypto-password-hash`. A malformed hash returns `#f`
/// rather than raising.
fn crypto_password_verify(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity("crypto-password-verify", "2", args.len()));
    }
    let password = expect_bytes("crypto-password-verify", args, 0)?;
    let phc = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        other => {
            return Err(FfiError::TypeMismatch {
                expected: "string (PHC hash)",
                got: other.type_name().to_string(),
            })
        }
    };
    let parsed = match PasswordHash::new(&phc) {
        Ok(h) => h,
        Err(_) => return Ok(Value::Boolean(false)),
    };
    Ok(Value::Boolean(
        Argon2::default()
            .verify_password(&password, &parsed)
            .is_ok(),
    ))
}
