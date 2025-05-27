use alloy_primitives::Address as AAddress;
use alloy_sol_types::{SolCall, sol};
use forge::executors::Executor;
use forge::revm::primitives::{Address, U256};
use forge::traces::CallTrace;

use crate::simulate::checkers::traits::{AssetChecker, PotentialMissingAsset};
use crate::simulate::types::{AssetContext, AssetSpec, AssetType, MissingAssetInfo};

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

        // Use the zero-address as sender to avoid problems when `asset.account`
        // contains code.
        let balance_result = executor.call_raw(
            Address::ZERO,
            asset.token_address,
            balance_data.into(),
            U256::ZERO,
        )?;

        let current_balance = balance_result
            .out
            .and_then(|out| balanceOfCall::abi_decode_returns(&out.data()).ok())
            .unwrap_or(U256::ZERO);

        // Calculate missing amount more concisely
        let missing_amount = asset.required_amount.saturating_sub(current_balance);

        Ok(MissingAssetInfo {
            account: asset.account,
            required: AssetSpec::ERC20 {
                token: asset.token_address,
                amount: asset.required_amount,
            },
            current_balance,
            missing_amount,
        })
    }

    fn deal(
        &self,
        recipient: Address,
        asset_spec: AssetSpec,
        executor: &mut Executor,
        context: &AssetContext,
    ) -> Result<(), eyre::Error> {
        if let AssetSpec::ERC20 { token, amount } = asset_spec {
            println!(
                "Dealing ERC20: token={:?}, recipient={:?}, amount={}",
                token, recipient, amount
            );
            println!("Storage accesses found: {:?}", context.storage_accesses);

            if context.storage_accesses.is_empty() {
                return Err(eyre::eyre!(
                    "No storage accesses found in trace - cannot determine balance slot"
                ));
            }

            let backend = executor.backend_mut();
            let large_balance = U256::MAX >> 1; // Use a large but not max value

            // Try patching all storage slots that were accessed
            // This handles cases where balance might be split across multiple slots
            // or where we need to patch both balance and total supply
            for (i, &storage_slot) in context.storage_accesses.iter().enumerate() {
                println!(
                    "Patching storage slot {} of {}: {:?}",
                    i + 1,
                    context.storage_accesses.len(),
                    storage_slot
                );

                backend.insert_account_storage(token, storage_slot, large_balance)?;

                println!(
                    "Successfully patched storage slot {:?} with balance {}",
                    storage_slot, large_balance
                );
            }

            // Also try to read the balance after patching to verify it worked
            let balance_call = balanceOfCall {
                account: AAddress::from_slice(recipient.as_slice()),
            };
            let balance_data = balance_call.abi_encode();

            match executor.call_raw(recipient, token, balance_data.into(), U256::ZERO) {
                Ok(balance_result) => {
                    let new_balance = balance_result
                        .out
                        .and_then(|out| balanceOfCall::abi_decode_returns(&out.data()).ok())
                        .unwrap_or(U256::ZERO);
                    println!("After patching, balance check shows: {}", new_balance);
                }
                Err(e) => {
                    println!("Warning: Could not verify balance after patching: {}", e);
                }
            }

            Ok(())
        } else {
            Err(eyre::eyre!("ERC20Checker can only deal ERC20 assets"))
        }
    }

    fn asset_type(&self) -> AssetType {
        AssetType::ERC20
    }
}
