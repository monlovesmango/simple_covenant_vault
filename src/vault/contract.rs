use anyhow::{anyhow, Result};
use bitcoin::absolute::LockTime;
use bitcoin::consensus::Encodable;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::hex::{Case, DisplayHex};
use bitcoin::key::{Keypair, Secp256k1};
use bitcoin::secp256k1::{rand, Message, ThirtyTwoByteHash};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::taproot::{LeafVersion, Signature, TaprootBuilder, TaprootSpendInfo};
use bitcoin::transaction::Version;
use bitcoin::{
    Address, Amount, Network, OutPoint, Sequence, TapLeafHash, TapSighashType, Transaction, TxIn,
    TxOut, XOnlyPublicKey,
};
use bitcoincore_rpc::jsonrpc::serde_json::{self};
use log::{debug, info};
use secp256kfun::marker::{EvenY, NonZero, Public};
use secp256kfun::{Point, G};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::settings::Settings;
use crate::vault::script::{
    ctv_vault_cancel_withdrawal, ctv_vault_complete_withdrawal, ctv_vault_deposit,
    vault_cancel_withdrawal, vault_complete_withdrawal, vault_trigger_withdrawal,
};
use crate::vault::signature_building;
use crate::vault::signature_building::{get_sigmsg_components, TxCommitmentSpec};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) enum VaultState {
    Inactive,
    Triggered,
    Completed,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) enum VaultType {
    CAT,
    CTV,
}

/// Get the vault state from the transaction and the vault address
impl From<(Transaction, Address)> for VaultState {
    fn from(spec: (Transaction, Address)) -> Self {
        let (tx, address) = spec;
        if tx.output.len() == 2 && tx.output.get(1).unwrap().value == Amount::from_sat(546) {
            VaultState::Triggered
        } else if tx.output.len() == 1
            && tx.output.first().unwrap().script_pubkey != address.script_pubkey()
        {
            VaultState::Completed
        } else {
            VaultState::Inactive
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct VaultCovenant {
    current_outpoint: Option<OutPoint>,
    amount: Amount,
    network: Network,
    pub(crate) timelock_in_blocks: u16,
    withdrawal_address: Option<String>,
    trigger_transaction: Option<Transaction>,
    state: VaultState,
    keypair: Keypair,
    vault_type: VaultType,
}

impl Default for VaultCovenant {
    fn default() -> Self {
        let secp = Secp256k1::new();
        let keypair = Keypair::new(&secp, &mut rand::thread_rng());
        Self {
            current_outpoint: None,
            amount: Amount::ZERO,
            network: Network::Regtest,
            timelock_in_blocks: 20,
            withdrawal_address: None,
            trigger_transaction: None,
            state: VaultState::Inactive,
            keypair,
            vault_type: VaultType::CAT,
        }
    }
}

impl VaultCovenant {
    pub(crate) fn new(timelock_in_blocks: u16, settings: &Settings) -> Result<Self> {
        Ok(Self {
            network: settings.network,
            timelock_in_blocks,
            vault_type: VaultType::CAT,
            ..Default::default()
        })
    }

    pub(crate) fn new_ctv(
        timelock_in_blocks: u16,
        amount: Amount,
        settings: &Settings,
    ) -> Result<Self> {
        Ok(Self {
            network: settings.network,
            timelock_in_blocks,
            amount,
            vault_type: VaultType::CTV,
            ..Default::default()
        })
    }

    pub(crate) fn from_file(filename: &Option<String>) -> Result<Self> {
        let filename = filename
            .clone()
            .unwrap_or("vault_covenant.json".to_string());
        info!("reading vault covenant from file: {}", filename);
        let file = std::fs::File::open(filename)?;
        let covenant: VaultCovenant = serde_json::from_reader(file)?;
        Ok(covenant)
    }

    pub(crate) fn to_file(&self, filename: &Option<String>) -> Result<()> {
        let filename = filename
            .clone()
            .unwrap_or("vault_covenant.json".to_string());
        info!("writing vault covenant to file: {}", filename);
        let file = std::fs::File::create(filename)?;
        serde_json::to_writer(file, self)?;
        Ok(())
    }

    pub(crate) fn set_current_outpoint(&mut self, outpoint: OutPoint) {
        self.current_outpoint = Some(outpoint);
    }

    pub(crate) fn get_current_outpoint(&self) -> Result<OutPoint> {
        self.current_outpoint.ok_or(anyhow!("no current outpoint"))
    }

    pub(crate) fn set_amount(&mut self, amount: Amount) {
        self.amount = amount
    }
    pub(crate) fn set_withdrawal_address(&mut self, address: Option<Address>) {
        self.withdrawal_address = address.map(|a| a.to_string());
    }

    pub(crate) fn get_withdrawal_address(&self) -> Result<Address> {
        Ok(Address::from_str(
            self.withdrawal_address
                .as_ref()
                .ok_or(anyhow!("no withdrawal address"))?,
        )?
        .require_network(self.network)?)
    }

    pub(crate) fn set_trigger_transaction(&mut self, txn: Option<Transaction>) {
        self.trigger_transaction = txn;
    }

    pub(crate) fn get_trigger_transaction(&self) -> Result<Transaction> {
        self.trigger_transaction
            .clone()
            .ok_or(anyhow!("no trigger transaction"))
    }

    pub(crate) fn set_state(&mut self, state: VaultState) {
        if state == VaultState::Completed {
            self.set_trigger_transaction(None);
            self.set_withdrawal_address(None);
        }
        self.state = state;
    }

    pub(crate) fn get_state(&self) -> VaultState {
        self.state.clone()
    }

    pub(crate) fn get_type(&self) -> VaultType {
        self.vault_type.clone()
    }

    pub(crate) fn address(&self) -> Result<Address> {
        let spend_info = if self.vault_type == VaultType::CAT {
            self.taproot_spend_info()?
        } else {
            self.ctv_deposit_spend_info()?
        };
        Ok(Address::p2tr_tweaked(spend_info.output_key(), self.network))
    }

    fn ctv_trigger_address(&self) -> Result<Address> {
        let spend_info = self.ctv_trigger_spend_info()?;
        Ok(Address::p2tr_tweaked(spend_info.output_key(), self.network))
    }

    fn taproot_spend_info(&self) -> Result<TaprootSpendInfo> {
        // hash G into a NUMS point
        let hash = sha256::Hash::hash(G.to_bytes_uncompressed().as_slice());
        let point: Point<EvenY, Public, NonZero> = Point::from_xonly_bytes(hash.into_32())
            .ok_or(anyhow!("G_X hash should be a valid x-only point"))?;
        let nums_key = XOnlyPublicKey::from_slice(point.to_xonly_bytes().as_slice())?;
        let secp = Secp256k1::new();
        Ok(TaprootBuilder::new()
            .add_leaf(1, vault_trigger_withdrawal(self.x_only_public_key()))?
            .add_leaf(
                2,
                vault_complete_withdrawal(self.x_only_public_key(), self.timelock_in_blocks),
            )?
            .add_leaf(2, vault_cancel_withdrawal(self.x_only_public_key()))?
            .finalize(&secp, nums_key)
            .expect("finalizing taproot spend info with a NUMS point should always work"))
    }

    fn ctv_deposit_spend_info(&self) -> Result<TaprootSpendInfo> {
        // hash G into a NUMS point
        let hash = sha256::Hash::hash(G.to_bytes_uncompressed().as_slice());
        let point: Point<EvenY, Public, NonZero> = Point::from_xonly_bytes(hash.into_32())
            .ok_or(anyhow!("G_X hash should be a valid x-only point"))?;
        let nums_key = XOnlyPublicKey::from_slice(point.to_xonly_bytes().as_slice())?;
        let secp = Secp256k1::new();

        Ok(TaprootBuilder::new()
            .add_leaf(0, ctv_vault_deposit(self.ctv_hash()))?
            .finalize(&secp, nums_key)
            .expect("finalizing taproot spend info with a new keypair should always work"))
    }

    fn ctv_trigger_spend_info(&self) -> Result<TaprootSpendInfo> {
        // hash G into a NUMS point
        let hash = sha256::Hash::hash(G.to_bytes_uncompressed().as_slice());
        let point: Point<EvenY, Public, NonZero> = Point::from_xonly_bytes(hash.into_32())
            .ok_or(anyhow!("G_X hash should be a valid x-only point"))?;
        let nums_key = XOnlyPublicKey::from_slice(point.to_xonly_bytes().as_slice())?;
        let secp = Secp256k1::new();

        Ok(TaprootBuilder::new()
            .add_leaf(
                1,
                ctv_vault_complete_withdrawal(self.x_only_public_key(), self.timelock_in_blocks),
            )?
            .add_leaf(1, ctv_vault_cancel_withdrawal(self.x_only_public_key()))?
            //.add_leaf(0, ctv_vault_cancel_withdrawal(self.x_only_public_key()))?
            .finalize(&secp, nums_key)
            .expect("finalizing taproot spend info with a new keypair should always work"))
    }

    fn ctv_hash(&self) -> [u8; 32] {
        let txn = self.ctv_trigger_tx_template();

        let tx_commitment_spec = TxCommitmentSpec {
            epoch: false,
            control: false,
            prevouts: false,
            prev_amounts: false,
            prev_sciptpubkeys: false,
            spend_type: false,
            annex: false,
            single_output: false,
            scriptpath: false,
            ..Default::default()
        };

        let components = get_sigmsg_components(
            &tx_commitment_spec,
            &txn,
            0,
            &[],
            None,
            TapLeafHash::all_zeros(),
            TapSighashType::Default,
        )
        .unwrap();

        let mut buffer = Vec::new();
        buffer.extend(components[0].clone()); // version
        buffer.extend(components[1].clone()); // locktime
        buffer.extend((txn.input.len() as u32).to_le_bytes()); // inputs len
        buffer.extend(components[2].clone()); // sequences
        buffer.extend((txn.output.len() as u32).to_le_bytes()); // outputs len
        buffer.extend(components[3].clone()); // outputs hash
        buffer.extend(components[4].clone()); // input index

        let hash = sha256::Hash::hash(&buffer);

        hash.to_byte_array()
    }

    fn x_only_public_key(&self) -> XOnlyPublicKey {
        return self.keypair.x_only_public_key().0;
    }

    fn sign_transaction(
        &self,
        txn: &Transaction,
        prevouts: &[TxOut],
        leaf_hash: TapLeafHash,
    ) -> Vec<u8> {
        let secp = Secp256k1::new();
        let mut sighashcache = SighashCache::new(txn);
        let sighash = sighashcache
            .taproot_script_spend_signature_hash(
                0,
                &Prevouts::All(prevouts),
                leaf_hash,
                TapSighashType::All,
            )
            .unwrap();
        let message = Message::from_digest_slice(sighash.as_byte_array()).unwrap();
        let signature = secp.sign_schnorr(&message, &self.keypair);
        let final_sig = Signature {
            sig: signature,
            hash_ty: TapSighashType::All,
        };
        return final_sig.to_vec();
    }

    pub(crate) fn create_trigger_tx(
        &self,
        fee_paying_utxo: &OutPoint,
        fee_paying_output: TxOut,
        target_address: &Address,
    ) -> Result<Transaction> {
        let mut vault_txin = TxIn {
            previous_output: self
                .current_outpoint
                .ok_or(anyhow!("no current outpoint"))?,
            ..Default::default()
        };
        let fee_txin = TxIn {
            previous_output: *fee_paying_utxo,
            ..Default::default()
        };
        let vault_output = TxOut {
            script_pubkey: self.address()?.script_pubkey(),
            value: self.amount,
        };
        let target_output = TxOut {
            script_pubkey: target_address.script_pubkey(),
            value: Amount::from_sat(546),
        };

        let txn = Transaction {
            lock_time: LockTime::ZERO,
            version: Version::TWO,
            input: vec![vault_txin.clone(), fee_txin],
            output: vec![vault_output.clone(), target_output.clone()],
        };

        let tx_commitment_spec = TxCommitmentSpec {
            prev_sciptpubkeys: false,
            prev_amounts: false,
            input_index: false,
            outputs: false,
            ..Default::default()
        };

        let leaf_hash = TapLeafHash::from_script(
            &vault_trigger_withdrawal(self.x_only_public_key()),
            LeafVersion::TapScript,
        );
        let vault_txout = TxOut {
            script_pubkey: self.address()?.script_pubkey().clone(),
            value: self.amount,
        };
        let contract_components = signature_building::grind_transaction(
            txn,
            signature_building::GrindField::LockTime,
            &[vault_txout.clone(), fee_paying_output.clone()],
            leaf_hash,
        )?;

        let mut txn = contract_components.transaction;
        let witness_components = get_sigmsg_components(
            &tx_commitment_spec,
            &txn,
            0,
            &[vault_txout.clone(), fee_paying_output.clone()],
            None,
            leaf_hash,
            TapSighashType::Default,
        )?;

        for component in witness_components.iter() {
            debug!(
                "pushing component <0x{}> into the witness",
                component.to_hex_string(Case::Lower)
            );
            vault_txin.witness.push(component.as_slice());
        }

        let mut target_scriptpubkey_buffer = Vec::new();
        target_output
            .script_pubkey
            .consensus_encode(&mut target_scriptpubkey_buffer)?;
        vault_txin
            .witness
            .push(target_scriptpubkey_buffer.as_slice());

        let mut amount_buffer = Vec::new();
        self.amount.consensus_encode(&mut amount_buffer)?;
        vault_txin.witness.push(amount_buffer.as_slice());
        let mut scriptpubkey_buffer = Vec::new();
        vault_output
            .script_pubkey
            .consensus_encode(&mut scriptpubkey_buffer)?;
        vault_txin.witness.push(scriptpubkey_buffer.as_slice());

        let mut fee_amount_buffer = Vec::new();
        fee_paying_output
            .value
            .consensus_encode(&mut fee_amount_buffer)?;
        vault_txin.witness.push(fee_amount_buffer.as_slice());
        let mut fee_scriptpubkey_buffer = Vec::new();
        fee_paying_output
            .script_pubkey
            .consensus_encode(&mut fee_scriptpubkey_buffer)?;
        vault_txin.witness.push(fee_scriptpubkey_buffer.as_slice());

        let computed_signature = signature_building::compute_signature_from_components(
            &contract_components.signature_components,
        )?;
        let mangled_signature: [u8; 63] = computed_signature[0..63].try_into().unwrap(); // chop off the last byte, so we can provide the 0x00 and 0x01 bytes on the stack
        vault_txin.witness.push(mangled_signature);
        vault_txin.witness.push([computed_signature[63]]); // push the last byte of the signature
        vault_txin.witness.push([computed_signature[63] + 1]); // push the last byte of the signature

        let sig = self.sign_transaction(
            &txn,
            &[vault_txout.clone(), fee_paying_output.clone()],
            leaf_hash,
        );
        vault_txin.witness.push(sig);

        vault_txin
            .witness
            .push(vault_trigger_withdrawal(self.x_only_public_key()).to_bytes());
        vault_txin.witness.push(
            self.taproot_spend_info()?
                .control_block(&(
                    vault_trigger_withdrawal(self.x_only_public_key()).clone(),
                    LeafVersion::TapScript,
                ))
                .expect("control block should work")
                .serialize(),
        );
        txn.input.first_mut().unwrap().witness = vault_txin.witness.clone();

        Ok(txn)
    }

    pub(crate) fn create_complete_tx(
        &self,
        fee_paying_utxo: &OutPoint,
        fee_paying_output: TxOut,
        target_address: &Address,
        trigger_tx: &Transaction,
    ) -> Result<Transaction> {
        let mut vault_txin = TxIn {
            previous_output: self
                .current_outpoint
                .ok_or(anyhow!("no current outpoint"))?,
            sequence: Sequence::from_height(self.timelock_in_blocks),
            ..Default::default()
        };
        let fee_txin = TxIn {
            previous_output: *fee_paying_utxo,
            ..Default::default()
        };

        let target_output = TxOut {
            script_pubkey: target_address.script_pubkey(),
            value: self.amount,
        };

        let txn = Transaction {
            lock_time: LockTime::ZERO,
            version: Version::TWO,
            input: vec![vault_txin.clone(), fee_txin],
            output: vec![target_output.clone()],
        };

        let tx_commitment_spec = TxCommitmentSpec {
            prevouts: false,
            outputs: false,
            ..Default::default()
        };

        let leaf_hash = TapLeafHash::from_script(
            &vault_complete_withdrawal(self.x_only_public_key(), self.timelock_in_blocks),
            LeafVersion::TapScript,
        );
        let vault_txout = TxOut {
            script_pubkey: self.address()?.script_pubkey().clone(),
            value: self.amount,
        };
        let contract_components = signature_building::grind_transaction(
            txn,
            signature_building::GrindField::Sequence,
            &[vault_txout.clone(), fee_paying_output.clone()],
            leaf_hash,
        )?;

        let mut txn = contract_components.transaction;
        let witness_components = get_sigmsg_components(
            &tx_commitment_spec,
            &txn,
            0,
            &[vault_txout.clone(), fee_paying_output.clone()],
            None,
            leaf_hash,
            TapSighashType::Default,
        )?;

        for component in witness_components.iter() {
            debug!(
                "pushing component <0x{}> into the witness",
                component.to_hex_string(Case::Lower)
            );
            vault_txin.witness.push(component.as_slice());
        }

        debug!("Previous TXID: {}", trigger_tx.txid());

        // stick all the previous txn components except the outputs into the witness
        let mut version_buffer = Vec::new();
        trigger_tx.version.consensus_encode(&mut version_buffer)?;
        vault_txin.witness.push(version_buffer.as_slice());

        // push the trigger_tx input in chunks no larger than 80 bytes
        let mut input_buffer = Vec::new();
        trigger_tx.input.consensus_encode(&mut input_buffer)?;
        //vault_txin.witness.push(input_buffer.as_slice());
        // TODO: handle the case where we have more than 2 chunks
        // we have to break this up into 80 byte chunks because there's a policy limit on the size of a single push
        let chunk_size = 80;
        for chunk in input_buffer.chunks(chunk_size) {
            vault_txin.witness.push(chunk);
        }

        let mut locktime_buffer = Vec::new();
        trigger_tx
            .lock_time
            .consensus_encode(&mut locktime_buffer)?;
        vault_txin.witness.push(locktime_buffer.as_slice());

        let mut vault_scriptpubkey_buffer = Vec::new();
        self.address()?
            .script_pubkey()
            .consensus_encode(&mut vault_scriptpubkey_buffer)?;
        vault_txin
            .witness
            .push(vault_scriptpubkey_buffer.as_slice());

        let mut amount_buffer = Vec::new();
        self.amount.consensus_encode(&mut amount_buffer)?;
        vault_txin.witness.push(amount_buffer.as_slice());

        let mut target_scriptpubkey_buffer = Vec::new();
        target_output
            .script_pubkey
            .consensus_encode(&mut target_scriptpubkey_buffer)?;
        vault_txin
            .witness
            .push(target_scriptpubkey_buffer.as_slice());

        let mut fee_paying_prevout_buffer = Vec::new();
        fee_paying_utxo.consensus_encode(&mut fee_paying_prevout_buffer)?;
        vault_txin
            .witness
            .push(fee_paying_prevout_buffer.as_slice());

        let computed_signature = signature_building::compute_signature_from_components(
            &contract_components.signature_components,
        )?;
        let mangled_signature: [u8; 63] = computed_signature[0..63].try_into().unwrap(); // chop off the last byte, so we can provide the 0x00 and 0x01 bytes on the stack
        vault_txin.witness.push(mangled_signature);
        vault_txin.witness.push([computed_signature[63]]); // push the last byte of the signature
        vault_txin.witness.push([computed_signature[63] + 1]); // push the last byte of the signature

        let sig = self.sign_transaction(
            &txn,
            &[vault_txout.clone(), fee_paying_output.clone()],
            leaf_hash,
        );
        vault_txin.witness.push(sig);

        vault_txin.witness.push(
            vault_complete_withdrawal(self.x_only_public_key(), self.timelock_in_blocks).to_bytes(),
        );
        vault_txin.witness.push(
            self.taproot_spend_info()?
                .control_block(&(
                    vault_complete_withdrawal(self.x_only_public_key(), self.timelock_in_blocks)
                        .clone(),
                    LeafVersion::TapScript,
                ))
                .expect("control block should work")
                .serialize(),
        );

        txn.input.first_mut().unwrap().witness = vault_txin.witness.clone();

        Ok(txn)
    }

    pub(crate) fn create_cancel_tx(
        &self,
        fee_paying_utxo: &OutPoint,
        fee_paying_output: TxOut,
    ) -> Result<Transaction> {
        let mut vault_txin = TxIn {
            previous_output: self
                .current_outpoint
                .ok_or(anyhow!("no current outpoint"))?,
            ..Default::default()
        };
        let fee_txin = TxIn {
            previous_output: fee_paying_utxo.clone(),
            ..Default::default()
        };
        let output = TxOut {
            script_pubkey: self.address()?.script_pubkey(),
            value: self.amount,
        };

        let txn = Transaction {
            lock_time: LockTime::ZERO,
            version: Version::TWO,
            input: vec![vault_txin.clone(), fee_txin],
            output: vec![output.clone()],
        };

        let tx_commitment_spec = TxCommitmentSpec {
            prev_sciptpubkeys: false,
            prev_amounts: false,
            input_index: false,
            outputs: false,
            ..Default::default()
        };

        let leaf_hash = TapLeafHash::from_script(
            &vault_cancel_withdrawal(self.x_only_public_key()),
            LeafVersion::TapScript,
        );
        let vault_txout = TxOut {
            script_pubkey: self.address()?.script_pubkey().clone(),
            value: self.amount,
        };
        let contract_components = signature_building::grind_transaction(
            txn,
            signature_building::GrindField::LockTime,
            &[vault_txout.clone(), fee_paying_output.clone()],
            leaf_hash,
        )?;

        let mut txn = contract_components.transaction;
        let witness_components = get_sigmsg_components(
            &tx_commitment_spec,
            &txn,
            0,
            &[vault_txout.clone(), fee_paying_output.clone()],
            None,
            leaf_hash,
            TapSighashType::Default,
        )?;

        for component in witness_components.iter() {
            debug!(
                "pushing component <0x{}> into the witness",
                component.to_hex_string(Case::Lower)
            );
            vault_txin.witness.push(component.as_slice());
        }
        let computed_signature = signature_building::compute_signature_from_components(
            &contract_components.signature_components,
        )?;

        let mut amount_buffer = Vec::new();
        self.amount.consensus_encode(&mut amount_buffer)?;
        vault_txin.witness.push(amount_buffer.as_slice());
        let mut scriptpubkey_buffer = Vec::new();
        output
            .script_pubkey
            .consensus_encode(&mut scriptpubkey_buffer)?;
        vault_txin.witness.push(scriptpubkey_buffer.as_slice());

        let mut fee_amount_buffer = Vec::new();
        fee_paying_output
            .value
            .consensus_encode(&mut fee_amount_buffer)?;
        vault_txin.witness.push(fee_amount_buffer.as_slice());
        let mut fee_scriptpubkey_buffer = Vec::new();
        fee_paying_output
            .script_pubkey
            .consensus_encode(&mut fee_scriptpubkey_buffer)?;
        vault_txin.witness.push(fee_scriptpubkey_buffer.as_slice());

        let mangled_signature: [u8; 63] = computed_signature[0..63].try_into().unwrap(); // chop off the last byte, so we can provide the 0x00 and 0x01 bytes on the stack
        vault_txin.witness.push(mangled_signature);
        vault_txin.witness.push([computed_signature[63]]); // push the last byte of the signature
        vault_txin.witness.push([computed_signature[63] + 1]); // push the last byte of the signature

        let sig = self.sign_transaction(
            &txn,
            &[vault_txout.clone(), fee_paying_output.clone()],
            leaf_hash,
        );
        vault_txin.witness.push(sig);

        vault_txin
            .witness
            .push(vault_cancel_withdrawal(self.x_only_public_key()).to_bytes());
        vault_txin.witness.push(
            self.taproot_spend_info()?
                .control_block(&(
                    vault_cancel_withdrawal(self.x_only_public_key()).clone(),
                    LeafVersion::TapScript,
                ))
                .expect("control block should work")
                .serialize(),
        );
        txn.input.first_mut().unwrap().witness = vault_txin.witness.clone();

        Ok(txn)
    }

    pub(crate) fn create_ctv_cancel_tx(
        &self,
        fee_paying_utxo: &OutPoint,
        fee_paying_output: TxOut,
    ) -> Result<Transaction> {
        let mut vault_txin = TxIn {
            previous_output: self
                .current_outpoint
                .ok_or(anyhow!("no current outpoint"))?,
            ..Default::default()
        };
        let fee_txin = TxIn {
            previous_output: fee_paying_utxo.clone(),
            ..Default::default()
        };
        let output = TxOut {
            script_pubkey: self.address()?.script_pubkey(),
            value: self.amount,
        };
        let mut txn = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![vault_txin.clone(), fee_txin],
            output: vec![output],
        };
        let leafhash = TapLeafHash::from_script(
            &ctv_vault_cancel_withdrawal(self.x_only_public_key()),
            LeafVersion::TapScript,
        );

        let vault_txout = TxOut {
            script_pubkey: self.ctv_trigger_address()?.script_pubkey().clone(),
            value: self.amount,
        };
        let sig = self.sign_transaction(
            &txn,
            &[vault_txout.clone(), fee_paying_output.clone()],
            leafhash,
        );
        vault_txin.witness.push(sig);

        vault_txin
            .witness
            .push(ctv_vault_cancel_withdrawal(self.x_only_public_key()).to_bytes());
        vault_txin.witness.push(
            self.ctv_trigger_spend_info()?
                .control_block(&(
                    ctv_vault_cancel_withdrawal(self.x_only_public_key()).clone(),
                    LeafVersion::TapScript,
                ))
                .expect("control block should work")
                .serialize(),
        );
        txn.input.first_mut().unwrap().witness = vault_txin.witness.clone();

        Ok(txn)
    }

    fn ctv_trigger_tx_template(&self) -> Transaction {
        let output = TxOut {
            script_pubkey: self.ctv_trigger_address().unwrap().script_pubkey(),
            value: self.amount,
        };
        let input = TxIn {
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            ..Default::default()
        };

        let txn = Transaction {
            lock_time: LockTime::ZERO,
            version: Version::TWO,
            input: vec![input.clone(), input],
            output: vec![output],
        };

        txn
    }

    pub(crate) fn create_ctv_trigger_tx(&self, fee_paying_utxo: &OutPoint) -> Result<Transaction> {
        let mut txn = self.ctv_trigger_tx_template();
        let fee_txin = TxIn {
            previous_output: *fee_paying_utxo,
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            ..Default::default()
        };
        let mut trigger_txin = TxIn {
            previous_output: self
                .current_outpoint
                .ok_or(anyhow!("no current outpoint"))?,
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            ..Default::default()
        };
        txn.input = vec![trigger_txin.clone(), fee_txin];

        trigger_txin
            .witness
            .push(ctv_vault_deposit(self.ctv_hash()).to_bytes());
        trigger_txin.witness.push(
            self.ctv_deposit_spend_info()?
                .control_block(&(
                    ctv_vault_deposit(self.ctv_hash()).clone(),
                    LeafVersion::TapScript,
                ))
                .expect("control block should work")
                .serialize(),
        );
        txn.input.first_mut().unwrap().witness = trigger_txin.witness.clone();

        Ok(txn)
    }
}
