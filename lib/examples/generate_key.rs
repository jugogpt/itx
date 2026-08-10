// Throwaway verification tool: generates a fresh keypair and saves the
// private and public halves to the given paths.
// Usage: cargo run -p btclib --example generate_key -- <priv_out_file> <pub_out_file>

use btclib::crypto::PrivateKey;
use btclib::util::Saveable;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let key = PrivateKey::new_key();
    key.save_to_file(&args[1]).unwrap();
    key.public_key().save_to_file(&args[2]).unwrap();
    println!("{}", key.public_key());
}
