// Throwaway verification tool: prints confirmed on-chain balance for an
// arbitrary hex-encoded pubkey against a live node.
// Usage: cargo run -p btclib --example check_balance -- <node_addr> <pubkey_hex>

use btclib::crypto::PublicKey;
use btclib::network::Message;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let node_addr = &args[1];
    let pubkey_hex = &args[2];
    let pubkey = PublicKey::from_sec1_bytes(&hex::decode(pubkey_hex).unwrap()).unwrap();

    let mut stream = tokio::net::TcpStream::connect(node_addr).await.unwrap();
    btclib::network::perform_handshake_initiator(&mut stream).await.unwrap();
    Message::FetchUTXOs(pubkey).send_async(&mut stream).await.unwrap();
    match Message::receive_async(&mut stream).await.unwrap() {
        Message::UTXOs(utxos) => {
            let confirmed: u64 = utxos.iter().filter(|(_, m)| !m).map(|(o, _)| o.value).sum();
            let mempool: u64 = utxos.iter().filter(|(_, m)| *m).map(|(o, _)| o.value).sum();
            println!("confirmed: {confirmed}, mempool(marked): {mempool}, total utxos: {}", utxos.len());
        }
        other => println!("unexpected: {other:?}"),
    }
}
