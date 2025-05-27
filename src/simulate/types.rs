use forge::revm::primitives::{Address, Bytes, U256};
use forge::traces::CallTrace;
use std::collections::HashMap;

use super::PotentialMissingAsset;

pub struct Call {
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
}

impl Call {
    // Add a constructor for more ergonomic usage
    pub fn new(from: Address, to: Address, data: impl Into<Bytes>, value: impl Into<U256>) -> Self {
        Self {
            from,
            to,
            value: value.into(),
            data: data.into(),
        }
    }
}

pub struct ForkInfo {
    pub rpc_url: Option<String>,
    pub block_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetSpec {
    Native(U256),
    ERC20 {
        token: Address,
        amount: U256,
    },
    ERC721 {
        token: Address,
        token_ids: Vec<U256>,
    },
    ERC1155 {
        token: Address,
        token_amounts: HashMap<U256, U256>,
    },
}

// -------------------------------------------------------------------------
//  Manual Hash implementation (maps don't implement Hash)
// -------------------------------------------------------------------------
use std::hash::{Hash, Hasher};

impl Hash for AssetSpec {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            AssetSpec::Native(amount) => {
                state.write_u8(0);
                amount.hash(state);
            }
            AssetSpec::ERC20 { token, amount } => {
                state.write_u8(1);
                token.hash(state);
                amount.hash(state);
            }
            AssetSpec::ERC721 { token, token_ids } => {
                state.write_u8(2);
                token.hash(state);
                for id in token_ids {
                    id.hash(state);
                }
            }
            AssetSpec::ERC1155 {
                token,
                token_amounts,
            } => {
                state.write_u8(3);
                token.hash(state);
                // Hash entries in deterministic key order
                let mut entries: Vec<_> = token_amounts.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                for (id, amt) in entries {
                    id.hash(state);
                    amt.hash(state);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct AssetGrant {
    pub recipient: Address,
    pub asset: AssetSpec,
}

impl AssetGrant {
    // Convenience constructors
    pub fn native(recipient: Address, amount: U256) -> Self {
        Self {
            recipient,
            asset: AssetSpec::Native(amount),
        }
    }

    pub fn erc20(recipient: Address, token: Address, amount: U256) -> Self {
        Self {
            recipient,
            asset: AssetSpec::ERC20 { token, amount },
        }
    }

    pub fn erc721(recipient: Address, token: Address, token_ids: Vec<U256>) -> Self {
        Self {
            recipient,
            asset: AssetSpec::ERC721 { token, token_ids },
        }
    }

    pub fn erc1155(recipient: Address, token: Address, token_amounts: HashMap<U256, U256>) -> Self {
        Self {
            recipient,
            asset: AssetSpec::ERC1155 {
                token,
                token_amounts,
            },
        }
    }

    pub fn asset_type(&self) -> AssetType {
        match &self.asset {
            AssetSpec::Native(_) => AssetType::Native,
            AssetSpec::ERC20 { .. } => AssetType::ERC20,
            AssetSpec::ERC721 { .. } => AssetType::ERC721,
            AssetSpec::ERC1155 { .. } => AssetType::ERC1155,
        }
    }
}

#[derive(Debug, Clone)]
pub enum AssetType {
    Native,
    ERC20,
    ERC721,
    ERC1155,
}

#[derive(Debug, Clone)]
pub struct MissingAssetInfo {
    pub account: Address,
    pub required: AssetSpec,   // What asset/amount is needed
    pub current_balance: U256, // Current balance (for reporting)
    pub missing_amount: U256,  // How much is missing (for reporting)
}

#[derive(Debug)]
pub struct AssetContext {
    pub potential_asset: PotentialMissingAsset,
    pub trace: CallTrace,
    pub storage_accesses: Vec<U256>, // Storage slots accessed during this call
}

impl AssetContext {
    /// Extract storage slots accessed during SLOAD operations from trace steps
    pub fn extract_storage_accesses(trace: &CallTrace) -> Vec<U256> {
        let mut storage_slots = Vec::new();

        for step in &trace.steps {
            // Look for SLOAD operations
            if step.op.as_str() == "SLOAD" {
                // The storage slot is the top item on the stack before SLOAD
                if let Some(stack) = &step.stack {
                    if let Some(slot) = stack.last() {
                        storage_slots.push(*slot);
                    }
                }
            }
        }

        storage_slots
    }

    /// Create AssetContext from trace and potential asset
    pub fn from_trace(potential_asset: PotentialMissingAsset, trace: CallTrace) -> Self {
        let storage_accesses = Self::extract_storage_accesses(&trace);

        Self {
            potential_asset,
            trace,
            storage_accesses,
        }
    }
}

// Add default implementations for testing
#[cfg(test)]
impl Default for Call {
    fn default() -> Self {
        Self {
            from: Address::random(),
            to: Address::random(),
            value: U256::ZERO,
            data: Bytes::default(),
        }
    }
}

#[cfg(test)]
impl Default for ForkInfo {
    fn default() -> Self {
        Self {
            rpc_url: Some("https://mainnet.base.org".to_string()),
            block_number: Some(30155463),
        }
    }
}
