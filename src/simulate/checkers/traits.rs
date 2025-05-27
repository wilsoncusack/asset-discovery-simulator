use crate::simulate::types::{AssetContext, AssetSpec, AssetType, MissingAssetInfo};
use forge::executors::Executor;
use forge::revm::primitives::{Address, U256};
use forge::traces::CallTrace;

#[derive(Debug, Clone)]
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
        executor: &mut Executor,
    ) -> Result<MissingAssetInfo, eyre::Error>;

    // Third phase: deal assets to fix missing balances (like Foundry's deal)
    fn deal(
        &self,
        recipient: Address,
        asset_spec: AssetSpec,
        executor: &mut Executor,
        context: &AssetContext,
    ) -> Result<(), eyre::Error>;

    // Helper to get the asset type this checker handles
    fn asset_type(&self) -> AssetType;
}
