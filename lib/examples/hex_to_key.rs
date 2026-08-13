// Throwaway verification tool: converts a raw hex-encoded secp256k1
// private key scalar (the format `agent-sdk-py`'s `identity.py` persists)
// into this project's own CBOR `PrivateKey` file format, so a Python-side
// MCP agent identity can be funded on-chain with `send_payment` during
// local smoke testing without reimplementing the wire protocol in Python.
//
// Usage: cargo run -p btclib --example hex_to_key -- <hex_private_key> <output_file>

use btclib::crypto::PrivateKey;
use btclib::util::Saveable;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hex_key = &args[1];
    let output_file = &args[2];

    let bytes = hex::decode(hex_key).unwrap();
    let key = PrivateKey::from_fixed_bytes(&bytes).unwrap();
    key.save_to_file(output_file).unwrap();
    println!("wrote {output_file}, pubkey: {}", key.public_key());
}
