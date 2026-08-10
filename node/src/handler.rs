use tracing::*;

use btclib::network::Message;
use btclib::sha256::Hash;
use tokio::net::TcpStream;

pub async fn handle_connection(mut socket: TcpStream) {
    let peer_ip = socket.peer_addr().ok().map(|addr| addr.ip());

    match btclib::network::perform_handshake_acceptor(&mut socket).await {
        Ok(offset) => {
            if let Some(ip) = peer_ip {
                crate::time_sync::record_sample(ip, offset).await;
            }
        }
        Err(e) => {
            warn!("handshake with peer failed: {e}, closing that connection");
            strike(peer_ip, true);
            return;
        }
    }

    loop {
        let message = match Message::receive_async(&mut socket).await {
            Ok(message) => message,
            Err(e) if btclib::network::is_benign_disconnect(&e) => {
                // The peer simply closed the connection after it was done
                // talking to us -- completely normal for a short-lived
                // client (e.g. a service that opens one connection per
                // request rather than holding a persistent one). Not a
                // protocol violation, so no strike.
                return;
            }
            Err(e) => {
                warn!("invalid message from peer: {e}, closing that connection");
                strike(peer_ip, true);
                return;
            }
        };

        use btclib::network::Message::*;
        match message {
            UTXOs(_) | Template(_) | ChainTip(_, _) | TemplateValidity(_) | NodeList(_)
            | Blocks(_) | Hello { .. } | HelloAck { .. } => {
                warn!("received neither a miner nor a wallet.");
                strike(peer_ip, true);
                return;
            }

            FetchBlocks { start, count } => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let max_count = (count as usize).min(btclib::BLOCKS_PER_FETCH_BATCH);
                let mut blocks = Vec::new();
                let mut total_bytes = 0usize;
                for block in blockchain.blocks().skip(start).take(max_count) {
                    let size = block.serialized_size();
                    // Always include at least one block if any exist here,
                    // even if it alone is unusually large -- otherwise an
                    // oversized block would make this reply come back
                    // empty, which the client would misread as "peer has
                    // nothing more" instead of "this one didn't fit."
                    if total_bytes + size > btclib::MAX_FETCH_BATCH_BYTES && !blocks.is_empty() {
                        break;
                    }
                    total_bytes += size;
                    blocks.push(block.clone());
                }
                drop(blockchain);
                let message = Blocks(blocks);
                message.send_async(&mut socket).await.unwrap();
            }

            DiscoverNodes => {
                let nodes = crate::NODES
                    .iter()
                    .map(|x| x.key().clone())
                    .collect::<Vec<_>>();
                let message = NodeList(nodes);
                message.send_async(&mut socket).await.unwrap();
            }

            AskChainTip => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let message = ChainTip(blockchain.block_height() as u32, blockchain.chain_work());
                message.send_async(&mut socket).await.unwrap();
            }

            FetchUTXOs(key) => {
                println!("received request to fetch UTXOs");
                let blockchain = crate::BLOCKCHAIN.read().await;
                let utxos = blockchain
                    .utxos()
                    .iter()
                    .filter(|(_, (_, txout))| txout.pubkey == key)
                    .map(|(_, (marked, txout))| (txout.clone(), *marked))
                    .collect::<Vec<_>>();
                let message = UTXOs(utxos);
                message.send_async(&mut socket).await.unwrap();
            }

            NewBlock(block) => {
                println!("received block from friend");
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                let result = blockchain.add_block(block.clone());
                drop(blockchain);
                match result {
                    Ok(()) => {
                        if let Some(store) = crate::BLOCK_STORE.get() {
                            if let Err(e) = crate::util::persist_chain_state(store, &block).await {
                                error!("failed to persist block: {e}");
                            }
                        }
                    }
                    Err(btclib::error::BtcError::OrphanBlock) => {
                        println!("received block buffered as orphan, waiting for its parent");
                    }
                    Err(e) => {
                        warn!("block rejected: {e}, closing connection");
                        strike(peer_ip, false);
                        return;
                    }
                }
            }

            NewTransaction(tx) => {
                println!("received transaction from friend");
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                if blockchain.add_to_mempool(tx).is_err() {
                    // not struck: a transaction can legitimately lose a
                    // race against a conflicting one already relayed by
                    // someone else, which isn't this peer's fault
                    println!("transaction rejected");
                }
            }

            ValidateTemplate(block_template) => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let prev_hash_matches = block_template.header.prev_block_hash
                    == blockchain
                        .blocks()
                        .last()
                        .map(|last_block| last_block.hash())
                        .unwrap_or(Hash::zero());
                // Also check the target: a difficulty retarget can happen
                // while a miner is still working on a template fetched just
                // before the boundary. Without this, the miner would only
                // find out its work was wasted when SubmitTemplate rejects
                // it outright, instead of on its next periodic check-in.
                let target_matches = block_template.header.target == blockchain.target();
                let status = prev_hash_matches && target_matches;
                let message = TemplateValidity(status);
                message.send_async(&mut socket).await.unwrap();
            }

            SubmitTemplate(block) => {
                println!("received allegedly mined template");
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                let add_result = blockchain.add_block(block.clone());
                drop(blockchain);
                match add_result {
                    Ok(()) => {
                        if let Some(store) = crate::BLOCK_STORE.get() {
                            if let Err(e) = crate::util::persist_chain_state(store, &block).await {
                                error!("failed to persist block: {e}");
                            }
                        }
                        println!("block looks good, broadcasting");
                        crate::util::broadcast(&Message::NewBlock(block.clone())).await;
                    }
                    Err(btclib::error::BtcError::OrphanBlock) => {
                        println!("submitted block references an unknown parent, buffered as orphan");
                    }
                    Err(e) => {
                        warn!("block rejected: {e}, closing connection");
                        strike(peer_ip, false);
                        return;
                    }
                }
            }

            SubmitTransaction(tx) => {
                println!("submit tx");
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                if let Err(e) = blockchain.add_to_mempool(tx.clone()) {
                    warn!("transaction rejected, closing connection: {e}");
                    strike(peer_ip, false);
                    return;
                }
                println!("added transaction to mempool");
                crate::util::broadcast(&Message::NewTransaction(tx.clone())).await;
                println!("transaction sent to friends");
            }

            FetchTemplate(pubkey) => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let block = match blockchain.create_block_template(pubkey) {
                    Ok(block) => block,
                    Err(e) => {
                        error!("failed to create block template: {e}");
                        return;
                    }
                };
                let message = Template(block);
                message.send_async(&mut socket).await.unwrap();
            }
        }
    }
}

fn strike(peer_ip: Option<std::net::IpAddr>, severe: bool) {
    if let Some(ip) = peer_ip {
        crate::ban::strike(ip, severe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;
    use btclib::types::{Block, BlockHeader, Blockchain, Transaction, TransactionOutput};
    use btclib::util::MerkleRoot;
    use btclib::U256;
    use chrono::Utc;
    use tokio::net::TcpListener;
    use uuid::Uuid;

    /// Mirrors `blockchain.rs`'s own private test helper of the same
    /// shape (not importable across the crate boundary) -- mines a valid
    /// single-coinbase child block extending `parent`.
    fn mined_child_block(parent: Hash, target: U256, reward: u64) -> Block {
        let private_key = PrivateKey::new_key();
        let coinbase = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: private_key.public_key(),
            }],
        );
        let merkle_root = MerkleRoot::calculate(&[coinbase.clone()]);
        let mut header = BlockHeader::new(Utc::now(), 0, parent, merkle_root, target);
        assert!(
            header.mine(1_000_000),
            "failed to find a valid nonce within the test mining budget"
        );
        Block::new(header, vec![coinbase])
    }

    /// Ensures `crate::BLOCKCHAIN` (a global shared by every test in this
    /// binary) has at least a genesis block, then mines and appends
    /// `count` more real children on top of whatever's already there --
    /// tests build on top of the shared chain rather than assuming a
    /// specific starting height. Caller must already hold
    /// `crate::TEST_GLOBAL_STATE_LOCK`. Returns the height `count` blocks
    /// were added at, and the blocks themselves in order.
    async fn ensure_chain_with_extra_blocks(count: usize) -> (usize, Vec<Block>) {
        let mut blockchain = crate::BLOCKCHAIN.write().await;
        if blockchain.block_height() == 0 {
            blockchain
                .add_block(Blockchain::genesis_block().clone())
                .expect("BUG: canonical genesis must always be accepted by a fresh chain");
        }
        let start = blockchain.block_height() as usize;
        let mut added = Vec::new();
        for _ in 0..count {
            let parent = blockchain.blocks().last().expect("genesis exists").hash();
            let target = blockchain.target();
            let reward = blockchain.calculate_block_reward();
            let block = mined_child_block(parent, target, reward);
            blockchain
                .add_block(block.clone())
                .expect("freshly mined valid child must be accepted");
            added.push(block);
        }
        (start, added)
    }

    /// Spawns a real `handle_connection` against a fresh ephemeral
    /// listener and returns a connected, handshaken client stream.
    async fn connect_handshaken_client() -> TcpStream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            handle_connection(socket).await;
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        btclib::network::perform_handshake_initiator(&mut client)
            .await
            .unwrap();
        client
    }

    #[tokio::test]
    async fn fetch_blocks_returns_exact_count_when_available() {
        let _guard = crate::TEST_GLOBAL_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (start, added) = ensure_chain_with_extra_blocks(3).await;
        let mut client = connect_handshaken_client().await;

        Message::FetchBlocks { start, count: 3 }
            .send_async(&mut client)
            .await
            .unwrap();
        match Message::receive_async(&mut client).await.unwrap() {
            Message::Blocks(blocks) => {
                assert_eq!(blocks.len(), 3);
                for (got, expected) in blocks.iter().zip(added.iter()) {
                    assert_eq!(got.hash(), expected.hash());
                }
            }
            other => panic!("expected Blocks, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_blocks_returns_fewer_than_requested_when_not_enough_exist() {
        let _guard = crate::TEST_GLOBAL_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (start, added) = ensure_chain_with_extra_blocks(2).await;
        let mut client = connect_handshaken_client().await;

        // ask for far more than exist past `start`
        Message::FetchBlocks { start, count: 1000 }
            .send_async(&mut client)
            .await
            .unwrap();
        match Message::receive_async(&mut client).await.unwrap() {
            Message::Blocks(blocks) => assert_eq!(blocks.len(), added.len(), "no error, just fewer than asked"),
            other => panic!("expected Blocks, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_blocks_past_the_tip_returns_empty_without_dropping_the_connection() {
        let _guard = crate::TEST_GLOBAL_STATE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (start, _added) = ensure_chain_with_extra_blocks(1).await;
        let mut client = connect_handshaken_client().await;

        Message::FetchBlocks { start: start + 1000, count: 5 }
            .send_async(&mut client)
            .await
            .unwrap();
        match Message::receive_async(&mut client).await.unwrap() {
            Message::Blocks(blocks) => assert!(blocks.is_empty()),
            other => panic!("expected an empty Blocks, got {other:?}"),
        }

        // Regression check for the bug this replaces: the old FetchBlock
        // handler bare-`return`d on an out-of-range height, closing the
        // socket. Prove the connection is still alive by sending a second,
        // unrelated request on it and getting a real reply back.
        Message::AskChainTip.send_async(&mut client).await.unwrap();
        let reply = Message::receive_async(&mut client).await.unwrap();
        assert!(matches!(reply, Message::ChainTip(_, _)));
    }
}

