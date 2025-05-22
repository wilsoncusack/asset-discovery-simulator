use forge::revm::primitives::{Address, Bytes, U256};

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AssetType {
    ERC20,
    ERC721,
    ERC1155,
    Native,
}

#[derive(Debug)]
pub struct MissingAssetInfo {
    pub asset_type: AssetType,
    pub token_address: Address,
    pub account: Address,
    pub required_amount: U256,
    pub current_balance: U256,
    pub missing_amount: U256,
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
