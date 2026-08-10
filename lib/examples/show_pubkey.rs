// Throwaway verification tool: prints the hex-encoded SEC1 public key for
// a saved private key file, in the same hex format every hub HTTP
// endpoint expects (matches `PublicKey`'s `Display` impl).
// Usage: cargo run -p btclib --example show_pubkey -- <priv_key_file>

use btclib::crypto::PrivateKey;
use btclib::util::Saveable;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let key = PrivateKey::load_from_file(&args[1]).unwrap();
    println!("{}", key.public_key());
}
