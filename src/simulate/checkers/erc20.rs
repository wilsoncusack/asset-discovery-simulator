use alloy_primitives::Address as AAddress;
use alloy_sol_types::{SolCall, sol};
use forge::executors::Executor;
use forge::revm::primitives::{Address, U256};
use forge::traces::CallTrace;

use crate::simulate::checkers::traits::{AssetChecker, PotentialMissingAsset};
use crate::simulate::types::{AssetType, MissingAssetInfo};

// Define ERC20 function signatures
sol! {
    function transfer(address to, uint256 amount) public returns (bool);
    function transferFrom(address from, address to, uint256 amount) public returns (bool);
    function balanceOf(address account) external view returns (uint256);
}

// Define a trait for ERC20 transfer operations (now object-safe)
pub trait ERC20TransferCheck {
    fn get_account(&self, trace: &CallTrace) -> Address;
    fn get_amount(&self) -> U256;
}

// Add static methods for decoding
impl transferCall {
    fn try_decode(data: &[u8]) -> Option<Self> {
        Self::abi_decode(data).ok()
    }
}

impl transferFromCall {
    fn try_decode(data: &[u8]) -> Option<Self> {
        Self::abi_decode(data).ok()
    }
}

// Implementation for transfer
impl ERC20TransferCheck for transferCall {
    fn get_account(&self, trace: &CallTrace) -> Address {
        trace.caller
    }

    fn get_amount(&self) -> U256 {
        self.amount
    }
}

// Implementation for transferFrom
impl ERC20TransferCheck for transferFromCall {
    fn get_account(&self, _trace: &CallTrace) -> Address {
        Address::from_slice(self.from.as_slice())
    }

    fn get_amount(&self) -> U256 {
        self.amount
    }
}

// ERC20 checker implementation
pub struct ERC20Checker {
    // Store a list of transfer checkers
    transfer_checkers: Vec<fn(&[u8]) -> Option<Box<dyn ERC20TransferCheck>>>,
}

impl ERC20Checker {
    pub fn new() -> Self {
        // Initialize with all supported transfer types
        Self {
            transfer_checkers: vec![
                |data| {
                    transferCall::try_decode(data)
                        .map(|d| Box::new(d) as Box<dyn ERC20TransferCheck>)
                },
                |data| {
                    transferFromCall::try_decode(data)
                        .map(|d| Box::new(d) as Box<dyn ERC20TransferCheck>)
                },
                // Add more transfer types here as needed
            ],
        }
    }
}

impl AssetChecker for ERC20Checker {
    fn identify_asset(&self, trace: &CallTrace) -> Option<PotentialMissingAsset> {
        let data = trace.data.as_ref();

        // Try each transfer checker until one succeeds
        for try_decode in &self.transfer_checkers {
            if let Some(decoded) = try_decode(data) {
                return Some(PotentialMissingAsset {
                    asset_type: AssetType::ERC20,
                    token_address: trace.address,
                    account: decoded.get_account(trace),
                    required_amount: decoded.get_amount(),
                });
            }
        }

        None
    }

    fn check_balance(
        &self,
        asset: PotentialMissingAsset,
        executor: &mut Executor,
    ) -> Result<MissingAssetInfo, eyre::Error> {
        // Execute the balanceOf call
        let balance_call = balanceOfCall {
            account: AAddress::from_slice(asset.account.as_slice()),
        };
        let balance_data = balance_call.abi_encode();

        let balance_result = executor.call_raw(
            asset.account,
            asset.token_address,
            balance_data.into(),
            U256::ZERO,
        )?;

        // Parse the result more idiomatically
        let current_balance = balance_result
            .out
            .and_then(|out| balanceOfCall::abi_decode_returns(&out.data()).ok())
            .unwrap_or(U256::ZERO);

        // Calculate missing amount more concisely
        let missing_amount = asset.required_amount.saturating_sub(current_balance);

        Ok(MissingAssetInfo {
            asset_type: asset.asset_type,
            token_address: asset.token_address,
            account: asset.account,
            required_amount: asset.required_amount,
            current_balance,
            missing_amount,
        })
    }
}
