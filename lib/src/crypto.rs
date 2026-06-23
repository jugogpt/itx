
use serde::{Deserialize, Serialize};
use ecdsa::{
    signature::signature,
    Signature as ECDSASignature,
    SigningKey, 
    VerifyingKey,
};
use ecdsa::signature::Verifier;
use k256::Secp256k1;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Signature(ECDSASignature<Secp256k1>); //we use elliptic curve digital signature algorithm (ECDSA) in order to create a signature-like object

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)] //these are called traits/attributes
pub struct PublicKey(VerifyingKey<Secp256k1>); // we use the public key for the purpose of verifying transactions


#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PrivateKey(
    #[serde(with = "signkey_serde")] //attributes allow cerrtain instantiations of structs to use functions from another module, here we use #[serde(with = "signkey_serde")]
    SigningKey<Secp256k1>,
); // we make a signature made by ECDSA ours by calculatig it with our private key signature

impl PrivateKey {
    pub fn new_key() -> Self {
        PrivateKey(SigningKey::random(&mut rand::thread_rng())) 
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.verifying_key().clone())
    }
}


impl Signature {
    
    // sign a crate::types::TransactionOutput from it Sha256 hash
    pub fn sign_output(
        output_hash: &Hash,
        private_key: &PrivateKey,
    ) -> Self {
        let signing_key = &private_key.0;
        let signature = signing_key.sign(&output_hash.as_bytes());
        Signature(signature)
    }
    // verify a signature
    pub fn verify(
        &self, 
        output_hash: &Hash,
        public_key: &PublicKey,
    ) -> bool {
        public_key.0.verify(&output_hash.as_bytes(), &self.0).is_ok()
    }


}




//what is Secp256k1 is an elliptic curve that is commonlu used by Bitcoin and etherium 
//Serialize and Deserialize are owned by the serde crate, and SigningKey is owned by the k245 crate
//we need the signkey_serde because SigningKey is a type from an external crate and you can't derive Serial and Desearilzie on it directly, so we make a local mod to define serialize and deserialize to use in crypto.rs
mod signkey_serde {
    use serde::Deserialize;
    pub fn serialize<S>(
        key: &super::SigningKey<super::Secp256k1> // the key uses the SigningKey struct created by the ecdsa crate
        serializer: S,
    ) -> result<S: Ok, S::Error> 
    where S: serde::Serializer, //these 'where' statements add more specificity to the <T> generic varaible and come before the implementation of the actual function with the generic variable as input
    {
        serializer.serialize_bytes(&key.to_bytes())
    }
    pub fn deserialize<'de, D>(
        deserializier: D,
    ) -> Result<super::SigningKey<super::Secp256k1>, D::Error> {
        let bytes: Vec::<u8>::deserialize(deserializer)?;
        Ok(super::SigningKey::from_slice(&bytes).unwrap()) //super:: refers the the partent module (here it is crypto.rs which conviently instantiates the crate ecdsa which contains the SigningKey struct that we use here for its implement function from_slice and unwrap())
    }


}