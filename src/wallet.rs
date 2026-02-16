use k256::ecdsa::{SigningKey, VerifyingKey, Signature, signature::Signer, signature::Verifier};
use rand::rngs::OsRng;
use std::fs;

#[derive(Clone)]
pub struct Wallet {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl Wallet {
    /// Generate a new random wallet
    pub fn new() -> Self {
        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = VerifyingKey::from(&signing_key);
        Self {
            signing_key,
            verifying_key,
        }
    }

    pub fn new_from_file(path: &str) -> Self {
        match Self::load_from_file(path) {
            Ok(w) => {
                println!("🔑 Wallet loaded from {}", path);
                w
            }
            Err(_) => {
                println!("⚠️  No wallet found at {}. Creating new...", path);
                let w = Self::new();
                if let Err(e) = w.save_to_file(path) {
                    println!("❌ Failed to save wallet: {}", e);
                } else {
                    println!("💾 Wallet saved to {}", path);
                }
                w
            }
        }
    }

    /// Save wallet private key to a file (hex encoded)
    pub fn save_to_file(&self, path: &str) -> std::io::Result<()> {
        let private_key_bytes = self.signing_key.to_bytes();
        let hex_key = hex::encode(private_key_bytes);
        fs::write(path, hex_key)?;
        Ok(())
    }

    /// Load wallet from a file containing hex-encoded private key
    pub fn load_from_file(path: &str) -> std::io::Result<Self> {
        let hex_key = fs::read_to_string(path)?;
        let bytes = hex::decode(hex_key.trim())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        
        let signing_key = SigningKey::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let verifying_key = VerifyingKey::from(&signing_key);
        
        Ok(Self {
            signing_key,
            verifying_key,
        })
    }

    /// Sign a message (byte slice) and return the signature as a hex string
    pub fn sign(&self, message: &[u8]) -> String {
        let signature: Signature = self.signing_key.sign(message);
        hex::encode(signature.to_bytes())
    }

    /// Get the public key as a hex string
    pub fn get_public_key(&self) -> String {
        hex::encode(self.verifying_key.to_encoded_point(true).as_bytes())
    }

    /// Verify a signature for a given message and public key (static helper)
    pub fn verify(message: &[u8], signature_hex: &str, public_key_hex: &str) -> bool {
        // Decode public key
        let pub_bytes = match hex::decode(public_key_hex) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let verifying_key = match VerifyingKey::from_sec1_bytes(&pub_bytes) {
            Ok(k) => k,
            Err(_) => return false,
        };

        // Decode signature
        let sig_bytes = match hex::decode(signature_hex) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let signature = match Signature::try_from(sig_bytes.as_slice()) {
            Ok(s) => s,
            Err(_) => return false,
        };

        // Verify
        verifying_key.verify(message, &signature).is_ok()
    }
}
