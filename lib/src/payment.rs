use tracing::*;

use crate::crypto::{PrivateKey, PublicKey, Signature};
use crate::types::{Transaction, TransactionInput, TransactionOutput};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum PaymentError {
    #[error("insufficient funds: have {available}, need {needed}")]
    InsufficientFunds { available: u64, needed: u64 },
}

/// Greedily selects from `available_utxos` (skipping any marked as
/// already spoken for) until `amount + fee` is covered, signs each
/// selected input with `signing_key`, and returns a transaction paying
/// `amount` to `recipient` with any leftover sent back to
/// `change_pubkey`.
///
/// All of `available_utxos` must be spendable by the same
/// `signing_key` -- a wallet mixing UTXOs across multiple keys in one
/// transaction needs to do its own input selection across keys; this
/// covers the common single-key case (which is all a single-operator
/// service like a faucet ever needs).
pub fn build_payment(
    available_utxos: &[(bool, TransactionOutput)],
    signing_key: &PrivateKey,
    recipient: PublicKey,
    amount: u64,
    fee: u64,
    change_pubkey: PublicKey,
) -> Result<Transaction, PaymentError> {
    let total_needed = amount + fee;
    let mut inputs = Vec::new();
    let mut input_sum = 0u64;

    for (marked, utxo) in available_utxos {
        if input_sum >= total_needed {
            break;
        }
        if *marked {
            continue;
        }
        let hash = utxo.hash();
        inputs.push(TransactionInput {
            prev_transaction_output_hash: hash,
            signature: Signature::sign_output(&hash, signing_key),
        });
        input_sum += utxo.value;
    }

    if input_sum < total_needed {
        return Err(PaymentError::InsufficientFunds {
            available: input_sum,
            needed: total_needed,
        });
    }

    let mut outputs = vec![TransactionOutput {
        value: amount,
        unique_id: Uuid::new_v4(),
        pubkey: recipient,
    }];
    if input_sum > total_needed {
        outputs.push(TransactionOutput {
            value: input_sum - total_needed,
            unique_id: Uuid::new_v4(),
            pubkey: change_pubkey,
        });
    }

    Ok(Transaction::new(inputs, outputs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid as UuidLib;

    fn utxo(value: u64, pubkey: PublicKey) -> TransactionOutput {
        TransactionOutput {
            value,
            unique_id: UuidLib::new_v4(),
            pubkey,
        }
    }

    #[test]
    fn build_payment_covers_amount_and_returns_change() {
        let owner = PrivateKey::new_key();
        let recipient = PrivateKey::new_key().public_key();

        let available = vec![
            (false, utxo(100, owner.public_key())),
            (false, utxo(50, owner.public_key())),
        ];

        let tx = build_payment(
            &available,
            &owner,
            recipient.clone(),
            120,
            5,
            owner.public_key(),
        )
        .unwrap();

        assert_eq!(tx.inputs.len(), 2); // needed both to cover 125
        let recipient_output = tx.outputs.iter().find(|o| o.pubkey == recipient).unwrap();
        assert_eq!(recipient_output.value, 120);
        let change_output = tx.outputs.iter().find(|o| o.pubkey == owner.public_key());
        assert_eq!(change_output.unwrap().value, 150 - 125);
    }

    #[test]
    fn build_payment_skips_marked_utxos() {
        let owner = PrivateKey::new_key();
        let recipient = PrivateKey::new_key().public_key();

        let available = vec![
            (true, utxo(1_000, owner.public_key())), // already spoken for -- must be ignored
            (false, utxo(10, owner.public_key())),
        ];

        // exactly enough if (and only if) the marked UTXO is correctly
        // excluded from consideration
        let tx = build_payment(&available, &owner, recipient.clone(), 10, 0, owner.public_key())
            .unwrap();
        assert_eq!(tx.inputs.len(), 1);
        let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
        assert_eq!(total_out, 10);

        // requesting anything beyond the unmarked 10 must fail, proving
        // the 1000-value marked UTXO was never available to cover it
        let result = build_payment(&available, &owner, recipient, 11, 0, owner.public_key());
        assert!(matches!(
            result,
            Err(PaymentError::InsufficientFunds { available: 10, needed: 11 })
        ));
    }

    #[test]
    fn build_payment_rejects_insufficient_funds() {
        let owner = PrivateKey::new_key();
        let recipient = PrivateKey::new_key().public_key();
        let available = vec![(false, utxo(5, owner.public_key()))];

        let result = build_payment(&available, &owner, recipient, 100, 0, owner.public_key());
        assert!(matches!(
            result,
            Err(PaymentError::InsufficientFunds { available: 5, needed: 100 })
        ));
    }
}
