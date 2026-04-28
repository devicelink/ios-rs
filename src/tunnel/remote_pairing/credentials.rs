/// RemotePairing credentials generated and persisted by this tool.
///
/// Stored per-device in `~/.local/share/devicelink/remote-pairing/<udid>.json`.
/// Created fresh on first pairing; reused on subsequent connections via pair-verify.
use std::path::PathBuf;

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use crate::tunnel::error::Error;

#[derive(Serialize, Deserialize, Clone)]
pub struct OurIdentity {
    /// UUID string identifying this host in the pairing protocol
    pub identifier:  String,
    /// Ed25519 signing key (32-byte seed, hex-encoded)
    pub signing_key: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PeerCredentials {
    /// The peer's RemotePairing identifier (UUID)
    pub peer_identifier: String,
    /// Peer's Ed25519 verifying key (32 bytes, hex-encoded) — not always known
    #[serde(default)]
    pub peer_public_key: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RemotePairingRecord {
    pub our:  OurIdentity,
    pub peer: PeerCredentials,
}

impl RemotePairingRecord {
    /// Load from disk or return None if the device has not been paired yet.
    pub fn load(udid: &str) -> Option<Self> {
        let path = record_path(udid)?;
        let data = std::fs::read(&path).ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// Persist to disk.
    pub fn save(&self, udid: &str) -> Result<(), Error> {
        let path = record_path(udid)
            .ok_or_else(|| Error::Protocol("cannot determine home directory".into()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self).unwrap();
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Delete — forces re-pairing next time.
    pub fn delete(udid: &str) {
        if let Some(p) = record_path(udid) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Deserialise the Ed25519 signing key from the stored hex seed.
    pub fn signing_key(&self) -> Result<SigningKey, Error> {
        let bytes = hex_decode(&self.our.signing_key)
            .map_err(|e| Error::Protocol(format!("bad signing key hex: {e}")))?;
        let arr: [u8; 32] = bytes.try_into()
            .map_err(|_| Error::Protocol("signing key must be 32 bytes".into()))?;
        Ok(SigningKey::from_bytes(&arr))
    }

    /// Build a fresh identity (no prior pairing for this device).
    pub fn new_identity(_udid: &str) -> Self {
        let sk    = SigningKey::generate(&mut OsRng);
        let ident = uuid::Uuid::new_v4().to_string().to_uppercase();
        RemotePairingRecord {
            our: OurIdentity {
                identifier:  ident,
                signing_key: hex_encode(sk.as_bytes()),
            },
            peer: PeerCredentials {
                peer_identifier: String::new(),
                peer_public_key: String::new(),
            },
        }
    }
}

fn record_path(udid: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home)
        .join(".local/share/devicelink/remote-pairing")
        .join(format!("{udid}.json")))
}

pub fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

// ── tiny UUID v4 without the uuid crate ──────────────────────────────────────

mod uuid {
    use rand::Rng;
    pub struct Uuid;
    impl Uuid {
        pub fn new_v4() -> UuidV4 {
            let mut bytes = [0u8; 16];
            rand::thread_rng().fill(&mut bytes);
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            UuidV4(bytes)
        }
    }
    pub struct UuidV4([u8; 16]);
    impl std::fmt::Display for UuidV4 {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let b = &self.0;
            write!(f,
                "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                b[0],b[1],b[2],b[3], b[4],b[5], b[6],b[7], b[8],b[9],
                b[10],b[11],b[12],b[13],b[14],b[15]
            )
        }
    }
}

