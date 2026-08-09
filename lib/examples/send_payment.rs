// Throwaway verification tool: sends a single payment from a saved
// private key to an arbitrary hex-encoded recipient pubkey, against a
// live node. Not part of any binary -- just enough wire-protocol
// plumbing to fund an address for manual/live testing (e.g. an
// exchange deposit address) without a full wallet.
//
// Usage: cargo run -p btclib --example send_payment -- <node_addr> <sender_priv_key_file> <recipient_pubkey_hex> <amount> <fee>

use btclib::crypto::PrivateKey;
use btclib::network::Message;
use btclib::payment::build_payment;
use btclib::types::TransactionOutput;
use btclib::util::Saveable;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let node_addr = &args[1];
    let sender_key_file = &args[2];
    let recipient_hex = &args[3];
    let amount: u64 = args[4].parse().unwrap();
    let fee: u64 = args[5].parse().unwrap();

    let sender = PrivateKey::load_from_file(sender_key_file).unwrap();
    let recipient_bytes = hex::decode(recipient_hex).unwrap();
    let recipient = btclib::crypto::PublicKey::from_sec1_bytes(&recipient_bytes).unwrap();

    let mut stream = tokio::net::TcpStream::connect(node_addr).await.unwrap();
    btclib::network::perform_handshake_initiator(&mut stream).await.unwrap();

    Message::FetchUTXOs(sender.public_key()).send_async(&mut stream).await.unwrap();
    let utxos: Vec<(TransactionOutput, bool)> = match Message::receive_async(&mut stream).await.unwrap() {
        Message::UTXOs(u) => u,
        other => panic!("unexpected reply: {other:?}"),
    };
    let available: Vec<(bool, TransactionOutput)> = utxos.into_iter().map(|(o, marked)| (marked, o)).collect();
    println!("sender has {} utxo(s)", available.len());

    let tx = build_payment(&available, &sender, recipient, amount, fee, sender.public_key()).unwrap();
    Message::SubmitTransaction(tx).send_async(&mut stream).await.unwrap();
    println!("submitted a payment of {amount} (fee {fee}) to {recipient_hex}");
}
