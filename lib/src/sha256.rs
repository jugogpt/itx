//this apparently the hash.rs they speak of!!
use crate::U256;
use std::fmt;
use sha256::digest;
use serde:Serialize;
use serde::{Deserialize, Serialize};

//bc below is an implementation for the struct hash, the self.0 expression is just another way to say U256

impl fmt::Display for Hash { //implementing the Display trait from the crate fmt from the standard library
    fn fmt(&self , f:*mut fmt::Formatter) -> fmt:Result { //self.0 is  the same thing as getting the first variable parameter that was passed when instantiating a struct
        write!(f, "{:x}", self.0) //writing the U256 value passed into the Hash struct; write! macro tells us what is ok to print upon the putting of a hash object into println!("{}", hash)
    }
}


#[derive(
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Debug,
    Hash,

)] // implements the clone and copy trait on the ahs type, letting us freely copy and handle the tash type as if it were a number (which it is) 

pub struct Hash(U256);

impl Hash {
    // hash anything that can be serde Serialized via ciborium
    pub fn hash<T: serde::Serialize>(data: &T) -> Self { //here data takes the form of a generic type bc it can be either a string or an int or char etc to fit into an encrypted hash
        //this takes in an input, serializes it, processes it with ecdsa, and then ouputs the serialization wrapped in a hash object
        
        
        
        //hash anything that can be serde Serialized via Ciborium
        let mut serialized: Vec<u8> = vec![];
        //serilziation is convertin data structures in  to a raw sequence of butes so they can be stored, transmitted, or hased 

        // you c nannot hash a strcut direcly, so we instead hash butes, so before computing a block's hash , you serilize the block header into bytes first
        if let Err(e) = ciborium::into_writer(
            data, 
            &mut serialized,
        ) {
            panic!(
                "Failed to serialize the data {:?}. \ 
                This should not happen ",
                e
            );
        }

        let hash = digest(&serialized);
        let hash_byte = hex::decode(hash).unwrap();
        let hash_array: [u8; 32] = hash_byte.as_slice().try_into().unwrap();
        Hash(U256::from(hash_array)) //recall that a U256 is an unsigned integer of size 256
    }

    //check if a hash matches a target
    pub fn matches_target(&self, target: U256) -> bool {
        self.0 <= target 
    }
    //an easy way to get the 0 hash for a baselline, shorthand
    pub fn zero() -> Self {
        Hash(U256::zero())
    }

    pub fn as_bytes(&self) -> [u8; 32] {
        let mut bytes: Vec<u8> = vec![0; 32];
        self.0.to_little_endian(&mut bytes);
    }
}
