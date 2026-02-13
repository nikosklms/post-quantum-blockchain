use serde::{Deserialize, Serialize};
use crate::wallet::Wallet;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub sender: String,   // Public Key (Hex)
    pub receiver: String, // Public Key (Hex)
    pub amount: f64,
    pub signature: String, // Hex
    pub timestamp: u128,
}

impl Transaction {
    pub fn new(sender: String, receiver: String, amount: f64, wallet: &Wallet) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();

        let mut tx = Transaction {
            sender,
            receiver,
            amount,
            signature: String::new(),
            timestamp,
        };

        tx.signature = tx.sign(wallet);
        tx
    }

    pub fn calculate_hash(&self) -> String {
        let input = format!("{}{}{}{}", self.sender, self.receiver, self.amount, self.timestamp);
        let mut hasher = Sha256::new();
        hasher.update(input);
        format!("{:x}", hasher.finalize())
    }

    pub fn sign(&self, wallet: &Wallet) -> String {
        let hash = self.calculate_hash();
        wallet.sign(hash.as_bytes())
    }

    pub fn verify(&self) -> bool {
        // Coinabase transaction (Genesis) has no signature
        if self.sender == "0" {
            return true;
        }

        let hash = self.calculate_hash();
        Wallet::verify(hash.as_bytes(), &self.signature, &self.sender)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::Wallet;

    #[test]
    fn test_valid_transaction() {
        let wallet = Wallet::new();
        let receiver = "some_receiver_key".to_string();
        let tx = Transaction::new(wallet.get_public_key(), receiver, 10.0, &wallet);

        assert!(tx.verify(), "Valid transaction should verify correctly");
    }

    #[test]
    fn test_tampered_transaction() {
        let wallet = Wallet::new();
        let receiver = "some_receiver_key".to_string();
        let mut tx = Transaction::new(wallet.get_public_key(), receiver, 10.0, &wallet);

        // 🚨 ATTACK: Change amount AFTER signing
        tx.amount = 1000.0; 

        assert!(!tx.verify(), "Tampered transaction should fail verification");
    }

    #[test]
    fn test_signature_forgery() {
        let alice_wallet = Wallet::new();
        let eve_wallet = Wallet::new(); // Attacker

        // Eve tries to create a transaction "From Alice"
        let receiver = "bob_key".to_string();
        
        let mut tx = Transaction {
            sender: alice_wallet.get_public_key(), // Claiming to be Alice
            receiver,
            amount: 100.0,
            signature: String::new(),
            timestamp: 12345,
        };

        // Eve signs it with HER key
        tx.signature = tx.sign(&eve_wallet);

        assert!(!tx.verify(), "forged signature should fail verification");
    }
}
