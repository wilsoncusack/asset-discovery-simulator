use forge::executors::Executor;
use forge::revm::primitives::{Address, U256};
use forge::traces::CallTrace;
use crate::simulate::types::{AssetType, MissingAssetInfo};

// Intermediate struct for identified assets before balance checking
pub struct PotentialMissingAsset {
    pub asset_type: AssetType,
    pub token_address: Address,
    pub account: Address,
    pub required_amount: U256,
}

// Core trait for checking a specific asset type
pub trait AssetChecker {
    // First phase: identify potential missing assets
    fn identify_asset(&self, trace: &CallTrace) -> Option<PotentialMissingAsset>;
    
    // Second phase: check balances and calculate missing amounts
    fn check_balance(
        &self, 
        asset: PotentialMissingAsset, 
        executor: &mut Executor
    ) -> Result<MissingAssetInfo, eyre::Error>;
} 