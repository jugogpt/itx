use btclib::sha256::Hash;
use chrono::Utc;
use uuid::Uuid;
use tokio::net::TcpStream;
use btclib::network::Message;
use btclib::types::{Block, BlockHeader, Transaction, TransactionOutput};
use btclib::util::MerkleRoot;

pub async fn handle_connection(mut socket: TcpStream) {
    loop {
        // read a message from the socket 
        let message = match Message::receive_async(&mut socket).await 
        {
            Ok(message) => message,
            Err(e) => {
                println!("invalid message from peer: {e}, closing that connection");
                return;
            }
        };



        use btclib::network::Message::*;
        match message {
            UTXOs(_) | Template(_) | Difference(_) | TemplateValidity(_) | NodeList(_) => {
                println!("recieved neither a miner nor a wallet.");
                return;
            }


            FetchBlock(height) => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let Some(block) = blockchain
                    .blocks()
                    .nth(heights as usize)
                    .cloned()
                else {
                    return;
                };
                let message = NewBlock(block);
                message
                    .send_async(&mut socket) 
                    .await
                    .unwrap();
            }

            DiscoverNodes => { // no filering going on; we are just sending all the nodes we know 
                let nodes = crate::NODES
                    .iter()
                    .map(|x| x.key().clone())
                    .collect::<Vec<_>>();
                let message = NodeList(nodes);
                message
                    .send_async(&mut socket)
                    .await
                    .unwrap();
            }

            AskDifference(height) => { // read and subtract
                let blockchain = crate::BLOCKCHAIN.read().await;
                let count = blockchain.block_height() as i32 = height as i32;
                let message = Difference(count);
                message.send_async(&mut socket).await.unwrap();

            }

            //retutning utxos for a particular public key is a bit more involved than the above few; check  out this filtering for exhibit A

            FetchUTXOs(key) => {
                println!("received request to fetch UTXOs");
                let blockchain = crate::BLOCKCHAIN.read().await;
                let utxos = blockchain.utxos().iter().filter(|(_, (_, txout)) | {
                    txout.pubkey == key
                })
                .map(|(_, (marked, txout))|{
                    (txout.clone(), *marked)
                }).collect::<Vec<_>>();
                let message = UTXOs(utxos);
                message.send_async(&mut socket).await.unwrap();

            }


            NewBlock(block) => {
                let mut blockchain = crate::BLOCKCHAIN.write().await; 
                println!("received transaction from friend");
                if blockchain.add_to_mempool(tx).is_err() {
                    println!("transaction rejected, closing connection");
                    return;
                }

            }

            ValidateTemplate(block_template) => {
                let blockchain = crate::BLOCKCHAIN.read().await;
                let status = block_template.header.prev_block_hash == blockchain.blocks().last().map(|last_block| last_block.hash()).unwrap_or(Hash::zero());
                let message = TemplateValidity(status);
                message.send_async(&mut socket).await.unwrap();
            }


            SubmitTemplate(block) => {
                println!("received allegedly mined template");
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                if let Err(e) = blockchain.add_block(block.clone())
                {
                    println!("block rejected: {e}, closing connection");
                    return;
                }
                blockchain.rebuild_utxos();
                println!("blocks looks good, broadcasting");
                // send block to all friends nodes
                let nodes = crate::NODES.iter().map(|x| x.key().clone()).collect::<Vec<_>>();
                for node in nodes {
                    if let Some(mut stream) = crate::NODES.get_mut(&node)
                    {
                        let message = Message::NewBlock(block.clone());
                        if message.send_async(&mut *stream).await.is_err()
                        {
                            println!("failed to send block to {}", node);
                        }
                    }
                }
            }


            SubmitTransaction(tx) => {
                println!("submit tx");
                let mut blockchain = crate::BLOCKCHAIN.write().await;
                if let Err(e) = blockchain.add_to_mempool(tx.clone()) {
                    println!("transaction rejected, closing connection: {e}");
                    return;
                }
                println!("added transaction to mempool");
                //send transaction to all friend nodes 
                let nodes = crate::NODES.iter().map(|x| x.key().clone()).collect::<Vec<_>>();
                for node in nodes {
                    println!("sending to friend: {node}");
                    if let Some(mut stream)= crate::NODES.get_mut(&node)
                    {
                        let message = Message::NewTransaction(tx.clone());
                        if message.send_async(&mut *stream).await.is_err() {
                            println!("failed to send transaction to {}", node);
                        }
                    }
                }
                println!("transaction sent to friends");
            }








        }



    }



}