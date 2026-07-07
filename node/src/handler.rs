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
            println!("handshake with peer failed: {e}, closing that connection");
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
                println!("invalid message from peer: {e}, closing that connection");
                strike(peer_ip, true);
                return;
            }
        };

        use btclib::network::Message::*;
        match message {
            UTXOs(_) | Template(_) | ChainTip(_, _) | TemplateValidity(_) | NodeList(_)
            | Hello { .. } | HelloAck { .. } => {
                println!("received neither a miner nor a wallet.");
                strike(peer_ip, true);
                return;
            }

            FetchBlock(height) => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let Some(block) = blockchain
                    .blocks()
                    .nth(height as usize)
                    .cloned()
                else {
                    return;
                };
                let message = NewBlock(block);
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
                                println!("failed to persist block: {e}");
                            }
                        }
                    }
                    Err(btclib::error::BtcError::OrphanBlock) => {
                        println!("received block buffered as orphan, waiting for its parent");
                    }
                    Err(e) => {
                        println!("block rejected: {e}, closing connection");
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
                                println!("failed to persist block: {e}");
                            }
                        }
                        println!("block looks good, broadcasting");
                        crate::util::broadcast(&Message::NewBlock(block.clone())).await;
                    }
                    Err(btclib::error::BtcError::OrphanBlock) => {
                        println!("submitted block references an unknown parent, buffered as orphan");
                    }
                    Err(e) => {
                        println!("block rejected: {e}, closing connection");
                        strike(peer_ip, false);
                        return;
                    }
                }
            }

            SubmitTransaction(tx) => {
                println!("submit tx");
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                if let Err(e) = blockchain.add_to_mempool(tx.clone()) {
                    println!("transaction rejected, closing connection: {e}");
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
                        eprintln!("{e}");
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

