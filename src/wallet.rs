use fn_dsa::{
    KeyPairGenerator, KeyPairGeneratorStandard, 
    SigningKey, SigningKeyStandard, 
    VerifyingKey, VerifyingKeyStandard, 
    sign_key_size, vrfy_key_size, signature_size, 
    FN_DSA_LOGN_512, DOMAIN_NONE, HASH_ID_RAW
};
use rand::rngs::OsRng;
use std::fs;
use std::io::Write;

#[derive(Clone)]
pub struct Wallet {
    // Store raw bytes for Falcon keys
    secret_key: Vec<u8>,
    public_key: Vec<u8>,
}

impl Wallet {
    /// Generate a new random wallet with Falcon-512 keys
    pub fn new() -> Self {
        // Falcon (fn-dsa-512)
        let mut kg = KeyPairGeneratorStandard::default();
        let mut secret_key = vec![0u8; sign_key_size(FN_DSA_LOGN_512)];
        let mut public_key = vec![0u8; vrfy_key_size(FN_DSA_LOGN_512)];
        
        kg.keygen(FN_DSA_LOGN_512, &mut OsRng, &mut secret_key, &mut public_key);

        Self {
            secret_key,
            public_key,
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

    /// Save wallet private keys to a file (hex encoded concatenation)
    pub fn save_to_file(&self, path: &str) -> std::io::Result<()> {
        let mut f = fs::File::create(path)?;
        f.write_all(&self.secret_key)?;
        f.write_all(&self.public_key)?;
        Ok(())
    }

    /// Load wallet from a file
    pub fn load_from_file(path: &str) -> std::io::Result<Self> {
        let data = fs::read(path)?;
        
        // Falcon SK (~1281) + Falcon PK (~897)
        let sk_size = sign_key_size(FN_DSA_LOGN_512);
        let vk_size = vrfy_key_size(FN_DSA_LOGN_512);
        let total_size = sk_size + vk_size;

        if data.len() < total_size {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "File too short"));
        }
        
        let (secret_key, public_key) = data.split_at(sk_size);
        
        Ok(Self {
            secret_key: secret_key.to_vec(),
            public_key: public_key[..vk_size].to_vec(),
        })
    }

    /// Sign a message (byte slice) and return the signature as a hex string
    pub fn sign(&self, message: &[u8]) -> String {
        let mut sk = SigningKeyStandard::decode(&self.secret_key).expect("Invalid Falcon Key");
        let mut signature = vec![0u8; signature_size(FN_DSA_LOGN_512)];
        sk.sign(&mut OsRng, &DOMAIN_NONE, &HASH_ID_RAW, message, &mut signature);
        
        hex::encode(signature)
    }

    /// Get the public key as a hex string
    pub fn get_public_key(&self) -> String {
        hex::encode(&self.public_key)
    }

    /// Verify a signature for a given message and public key (static helper)
    pub fn verify(message: &[u8], signature_hex: &str, public_key_hex: &str) -> bool {
        let pub_bytes = match hex::decode(public_key_hex) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let sig_bytes = match hex::decode(signature_hex) {
            Ok(b) => b,
            Err(_) => return false,
        };

        let vk = match VerifyingKeyStandard::decode(&pub_bytes) {
            Some(k) => k,
            None => return false,
        };
        
        vk.verify(&sig_bytes, &DOMAIN_NONE, &HASH_ID_RAW, message)
    }
}
