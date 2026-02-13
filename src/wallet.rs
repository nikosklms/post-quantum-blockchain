use k256::ecdsa::{SigningKey, VerifyingKey, Signature, signature::Signer, signature::Verifier};
use rand::rngs::OsRng;

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
