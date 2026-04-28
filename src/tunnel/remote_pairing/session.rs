/// RemotePairing session over a CoreDeviceProxy service socket.
///
/// Messages are raw plist dicts (4-byte BE length-prefix framing).
/// Binary data (TLV8, ciphertext) is sent as plist `<data>` elements.
use std::io::{Read, Write};

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use ed25519_dalek::Signer;
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::Sha512;
use srp::{client::SrpClient, groups::G_3072};
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PubKey};

use crate::tunnel::error::Error;
use super::credentials::{hex_encode, RemotePairingRecord};
use super::framing;
use super::tlv8;

const WIRE_PROTOCOL_VERSION: i64 = 19;
const SRP_USERNAME: &str = "Pair-Setup";
const SRP_PIN: &str = "000000";

// ── combined Read+Write trait ────────────────────────────────────────────────

pub trait ReadWrite: Read + Write + Send {}
impl<T: Read + Write + Send> ReadWrite for T {}

// ── session ───────────────────────────────────────────────────────────────────

pub struct RemotePairingSession {
    stream:     Box<dyn ReadWrite>,
    record:     RemotePairingRecord,
    seq:        i64,
    client_key: Option<[u8; 32]>,
    server_key: Option<[u8; 32]>,
    enc_seq:    u64,
}

impl RemotePairingSession {
    pub fn open(
        stream:  impl ReadWrite + 'static,
        record:  RemotePairingRecord,
        udid:    &str,
    ) -> Result<(Self, RemotePairingRecord), Error> {
        let mut s = RemotePairingSession {
            stream:     Box::new(stream),
            record:     record.clone(),
            seq:        0,
            client_key: None,
            server_key: None,
            enc_seq:    0,
        };

        let has_creds = !record.our.signing_key.is_empty()
                     && !record.peer.peer_identifier.is_empty();

        // Initial handshake — always send attemptPairVerify:true regardless of
        // whether we have credentials.  Sending false causes the device to
        // silently discard the message and never respond.
        s.send(plist_dict! {
            "hostOptions" => plist_dict! {
                "attemptPairVerify" => plist::Value::Boolean(true)
            },
            "wireProtocolVersion" => plist::Value::Integer(WIRE_PROTOCOL_VERSION.into()),
        })?;
        let caps = s.recv()?;
        // Device capabilities received — log key names for diagnostics
        if let Some(d) = caps.as_dictionary() {
            eprintln!("[RemotePairing] device responded with keys: {:?}", d.keys().collect::<Vec<_>>());
        }

        // Pair-verify or SRP
        let updated = if has_creds {
            match s.pair_verify() {
                Ok(()) => s.record.clone(),
                Err(e) => {
                    eprintln!("pair-verify failed ({e}), falling back to SRP");
                    s.srp_pair(udid)?
                }
            }
        } else {
            s.srp_pair(udid)?
        };

        Ok((s, updated))
    }

    // ── pair verify ──────────────────────────────────────────────────────────

    fn pair_verify(&mut self) -> Result<(), Error> {
        let sk = EphemeralSecret::random_from_rng(OsRng);
        let pk = X25519PubKey::from(&sk);

        // STATE=0x01
        self.send_pairing_data("verifyManualPairing", true,
            &tlv8::encode(&[(tlv8::STATE, &[0x01]), (tlv8::PUBLIC_KEY, pk.as_bytes())])
        )?;

        // Device responds with its X25519 public key
        let resp    = self.recv_pairing_data()?;
        let tlv     = tlv8::decode(&resp);
        let dev_pk  = {
            let b = tlv.get(&tlv8::PUBLIC_KEY)
                .ok_or_else(|| Error::Protocol("pair-verify: no PUBLIC_KEY".into()))?;
            if b.len() != 32 { return Err(Error::Protocol("pair-verify: bad key len".into())); }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(b);
            X25519PubKey::from(arr)
        };
        let shared   = sk.diffie_hellman(&dev_pk);
        let enc_key  = hkdf32(shared.as_bytes(), b"Pair-Verify-Encrypt-Salt", b"Pair-Verify-Encrypt-Info")?;
        let signing  = self.record.signing_key()?;
        let sig = {
            let mut buf = Vec::new();
            buf.extend_from_slice(pk.as_bytes());
            buf.extend_from_slice(self.record.our.identifier.as_bytes());
            buf.extend_from_slice(dev_pk.as_bytes());
            signing.sign(&buf).to_bytes()
        };
        let inner   = tlv8::encode(&[
            (tlv8::IDENTIFIER, self.record.our.identifier.as_bytes()),
            (tlv8::SIGNATURE, &sig),
        ]);
        let enc     = chacha_encrypt(&enc_key, b"PV-Msg03\x00\x00\x00\x00", &inner)?;

        // STATE=0x03
        self.send_pairing_data("verifyManualPairing", false,
            &tlv8::encode(&[(tlv8::STATE, &[0x03]), (tlv8::ENCRYPTED_DATA, &enc)])
        )?;

        // Expect STATE=0x04
        let resp  = self.recv_pairing_data()?;
        let tlv   = tlv8::decode(&resp);
        if let Some(e) = tlv.get(&tlv8::ERROR) {
            return Err(Error::Protocol(format!("pair-verify: device error {e:02x?}")));
        }
        match tlv8::get_u8(&tlv, tlv8::STATE) {
            Some(0x04) => {}
            s => return Err(Error::Protocol(format!("pair-verify: bad state {s:02x?}"))),
        }

        self.client_key = Some(hkdf32(shared.as_bytes(), b"", b"ClientEncrypt-main")?);
        self.server_key = Some(hkdf32(shared.as_bytes(), b"", b"ServerEncrypt-main")?);
        Ok(())
    }

    // ── SRP pairing ──────────────────────────────────────────────────────────

    fn srp_pair(&mut self, udid: &str) -> Result<RemotePairingRecord, Error> {
        eprintln!("SRP pairing — approve 'Trust this computer?' on the device if prompted");

        self.send_pairing_data("setupManualPairing", true,
            &tlv8::encode(&[(tlv8::METHOD, &[0x00]), (tlv8::STATE, &[0x01])])
        )?;

        let resp     = self.recv_pairing_data()?;
        let tlv      = tlv8::decode(&resp);
        if let Some(e) = tlv.get(&tlv8::ERROR) {
            return Err(Error::Protocol(format!("SRP start error: {e:02x?}")));
        }
        let salt   = tlv.get(&tlv8::SALT).cloned()
            .ok_or_else(|| Error::Protocol("SRP: no SALT".into()))?;
        let b_pub  = tlv.get(&tlv8::PUBLIC_KEY).cloned()
            .ok_or_else(|| Error::Protocol("SRP: no PUBLIC_KEY".into()))?;

        let client   = SrpClient::<Sha512>::new(&G_3072);
        let a_priv   = rand_bytes(64);
        let a_pub    = client.compute_public_ephemeral(&a_priv);
        let verifier = client.process_reply(
            &a_priv, SRP_USERNAME.as_bytes(), SRP_PIN.as_bytes(), &salt, &b_pub,
        ).map_err(|e| Error::Protocol(format!("SRP compute: {e}")))?;

        self.send_pairing_data("setupManualPairing", false,
            &tlv8::encode(&[
                (tlv8::STATE, &[0x03]),
                (tlv8::PUBLIC_KEY, &a_pub),
                (tlv8::PROOF, verifier.proof()),
            ])
        )?;

        let resp  = self.recv_pairing_data()?;
        let tlv   = tlv8::decode(&resp);
        if let Some(e) = tlv.get(&tlv8::ERROR) {
            return Err(Error::Protocol(format!("SRP proof error: {e:02x?}")));
        }
        let m2 = tlv.get(&tlv8::PROOF).cloned()
            .ok_or_else(|| Error::Protocol("SRP: no server PROOF".into()))?;
        verifier.verify_server(&m2)
            .map_err(|e| Error::Protocol(format!("SRP server proof: {e}")))?;

        let session_key  = verifier.key();
        let setup_enc    = hkdf32(session_key, b"Pair-Setup-Encrypt-Salt", b"Pair-Setup-Encrypt-Info")?;

        let mut record = if self.record.our.signing_key.is_empty() {
            RemotePairingRecord::new_identity(udid)
        } else {
            self.record.clone()
        };
        let signing  = record.signing_key()?;
        let vk_bytes = signing.verifying_key().to_bytes();
        let ctrl_key = hkdf32(session_key, b"Pair-Setup-Controller-Sign-Salt", b"Pair-Setup-Controller-Sign-Info")?;
        let sig = {
            let mut buf = Vec::new();
            buf.extend_from_slice(&ctrl_key);
            buf.extend_from_slice(record.our.identifier.as_bytes());
            buf.extend_from_slice(&vk_bytes);
            signing.sign(&buf).to_bytes()
        };
        let inner = tlv8::encode(&[
            (tlv8::IDENTIFIER, record.our.identifier.as_bytes()),
            (tlv8::PUBLIC_KEY, &vk_bytes),
            (tlv8::SIGNATURE,  &sig),
        ]);
        let enc   = chacha_encrypt(&setup_enc, b"PS-Msg05\x00\x00\x00\x00", &inner)?;

        self.send_pairing_data("setupManualPairing", false,
            &tlv8::encode(&[(tlv8::STATE, &[0x05]), (tlv8::ENCRYPTED_DATA, &enc)])
        )?;

        let resp = self.recv_pairing_data()?;
        let tlv  = tlv8::decode(&resp);
        if let Some(e) = tlv.get(&tlv8::ERROR) {
            return Err(Error::Protocol(format!("SRP identity error: {e:02x?}")));
        }
        if let Some(enc) = tlv.get(&tlv8::ENCRYPTED_DATA) {
            if let Ok(plain) = chacha_decrypt(&setup_enc, b"PS-Msg06\x00\x00\x00\x00", enc) {
                let dev_tlv = tlv8::decode(&plain);
                if let Some(id) = dev_tlv.get(&tlv8::IDENTIFIER) {
                    record.peer.peer_identifier = String::from_utf8_lossy(id).into();
                }
                if let Some(pk) = dev_tlv.get(&tlv8::PUBLIC_KEY) {
                    record.peer.peer_public_key = hex_encode(pk);
                }
            }
        }

        self.client_key = Some(hkdf32(session_key, b"", b"ClientEncrypt-main")?);
        self.server_key = Some(hkdf32(session_key, b"", b"ServerEncrypt-main")?);
        self.record = record.clone();
        record.save(udid)?;
        eprintln!("Pairing successful — credentials saved for {udid}");
        Ok(record)
    }

    // ── encrypted request/response ────────────────────────────────────────────

    pub fn encrypted_request(&mut self, req: &serde_json::Value) -> Result<serde_json::Value, Error> {
        let client_key = self.client_key
            .ok_or_else(|| Error::Protocol("no session key".into()))?;
        let server_key = self.server_key.unwrap();

        let plaintext = serde_json::to_vec(req)?;
        let seq       = self.enc_seq;
        self.enc_seq += 1;

        let nonce     = enc_nonce(seq);
        let cipher    = chacha_encrypt(&client_key, &nonce, &plaintext)?;

        // Send: {message: {streamEncrypted: {_0: <data>}}, originatedBy, sequenceNumber}
        let mut inner_dict = plist::Dictionary::new();
        inner_dict.insert("_0".into(), plist::Value::Data(cipher));
        let mut enc_dict = plist::Dictionary::new();
        enc_dict.insert("streamEncrypted".into(), plist::Value::Dictionary(inner_dict));
        let mut msg = plist::Dictionary::new();
        msg.insert("message".into(), plist::Value::Dictionary(enc_dict));
        msg.insert("originatedBy".into(), plist::Value::String("host".into()));
        msg.insert("sequenceNumber".into(), plist::Value::Integer((seq as i64).into()));

        self.send(plist::Value::Dictionary(msg))?;

        // Receive encrypted response
        let resp = self.recv()?;
        let resp_dict = resp.as_dictionary()
            .ok_or_else(|| Error::Protocol("encrypted response: not a dict".into()))?;
        let cipher_bytes = resp_dict
            .get("message").and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("streamEncrypted")).and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("_0")).and_then(|v| v.as_data())
            .ok_or_else(|| Error::Protocol("encrypted response: no ciphertext data".into()))?;

        let resp_seq  = resp_dict.get("sequenceNumber")
            .and_then(|v| v.as_signed_integer()).unwrap_or(seq as i64) as u64;
        let resp_nonce = enc_nonce(resp_seq);
        let plain     = chacha_decrypt(&server_key, &resp_nonce, cipher_bytes)?;
        serde_json::from_slice(&plain).map_err(|e| Error::Protocol(format!("decrypt JSON: {e}")))
    }

    // ── raw send/recv ────────────────────────────────────────────────────────

    fn send(&mut self, msg: plist::Value) -> Result<(), Error> {
        self.seq += 1;
        framing::send(&mut *self.stream, &msg)
    }

    fn recv(&mut self) -> Result<plist::Value, Error> {
        framing::recv(&mut *self.stream)
    }

    fn send_pairing_data(&mut self, kind: &str, new_session: bool, tlv: &[u8]) -> Result<(), Error> {
        let mut data_dict = plist::Dictionary::new();
        data_dict.insert("data".into(), plist::Value::Data(tlv.to_vec()));
        let mut inner = plist::Dictionary::new();
        inner.insert("_0".into(), plist::Value::Dictionary(data_dict));
        let mut msg = plist::Dictionary::new();
        msg.insert("pairingData".into(), plist::Value::Dictionary(inner));
        msg.insert("kind".into(), plist::Value::String(kind.into()));
        msg.insert("startNewSession".into(), plist::Value::Boolean(new_session));
        self.send(plist::Value::Dictionary(msg))
    }

    fn recv_pairing_data(&mut self) -> Result<Vec<u8>, Error> {
        let val  = self.recv()?;
        let dict = val.as_dictionary()
            .ok_or_else(|| Error::Protocol("pairing response: not a dict".into()))?;

        // Check for error responses
        if let Some(e) = dict.get("error") {
            let msg = match e {
                plist::Value::String(s) => s.clone(),
                other => format!("{other:?}"),
            };
            return Err(Error::Protocol(format!("RemotePairing error: {msg}")));
        }

        // Extract pairingData._0.data
        dict.get("pairingData").and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("_0")).and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("data")).and_then(|v| v.as_data())
            .map(|b| b.to_vec())
            .ok_or_else(|| Error::Protocol("pairing response: no pairingData".into()))
    }
}

// ── plist helpers ─────────────────────────────────────────────────────────────

macro_rules! plist_dict {
    ($($k:expr => $v:expr),* $(,)?) => {{
        let mut d = plist::Dictionary::new();
        $(d.insert($k.into(), $v);)*
        plist::Value::Dictionary(d)
    }};
}
pub(crate) use plist_dict;

// ── crypto helpers ────────────────────────────────────────────────────────────

fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<[u8; 32], Error> {
    let salt_opt = if salt.is_empty() { None } else { Some(salt) };
    let hk = Hkdf::<Sha512>::new(salt_opt, ikm);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out)
        .map_err(|e| Error::Protocol(format!("HKDF: {e}")))?;
    Ok(out)
}

fn chacha_encrypt(key: &[u8; 32], nonce12: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce  = Nonce::from_slice(nonce12);
    cipher.encrypt(nonce, plaintext)
        .map_err(|e| Error::Protocol(format!("ChaCha encrypt: {e}")))
}

fn chacha_decrypt(key: &[u8; 32], nonce12: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce  = Nonce::from_slice(nonce12);
    cipher.decrypt(nonce, ciphertext)
        .map_err(|e| Error::Protocol(format!("ChaCha decrypt: {e}")))
}

fn enc_nonce(seq: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0..8].copy_from_slice(&seq.to_le_bytes());
    n
}

fn rand_bytes(n: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut b = vec![0u8; n];
    OsRng.fill_bytes(&mut b);
    b
}

