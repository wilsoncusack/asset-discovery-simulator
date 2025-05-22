use crate::simulate::checkers::{AssetChecker, ERC20Checker};
use crate::simulate::types::{Call, ForkInfo, MissingAssetInfo};
use crate::simulate::utils::find_last_non_proxy_call;
use forge::{
    backend::Backend,
    executors::ExecutorBuilder,
    revm::primitives::{Env, U256},
    traces::TraceMode,
};
use foundry_config::Config;
use foundry_evm_core::opts::EvmOpts;

// Main simulator that orchestrates simulation and checking
pub struct AssetSimulator {
    fork_info: Option<ForkInfo>,
    env: Option<Env>,
    backend: Option<Backend>,
    checkers: Vec<Box<dyn AssetChecker>>,
}

impl AssetSimulator {
    // Create a new simulator with no settings
    pub fn new() -> Self {
        Self {
            fork_info: None,
            env: None,
            backend: None,
            checkers: Vec::new(),
        }
    }

    // Simplified fork configuration
    pub fn with_fork(mut self, rpc_url: impl Into<String>, block_number: Option<u64>) -> Self {
        self.fork_info = Some(ForkInfo {
            rpc_url: Some(rpc_url.into()),
            block_number,
        });
        self
    }

    // Convenience method for adding ERC20 checker
    pub fn with_erc20_checker(self) -> Self {
        self.with_checker(ERC20Checker::new())
    }

    // Add a checker for a specific asset type
    pub fn with_checker<T: AssetChecker + 'static>(mut self, checker: T) -> Self {
        self.checkers.push(Box::new(checker));
        self
    }

    // Set custom environment
    pub fn with_env(mut self, env: Env) -> Self {
        self.env = Some(env);
        self
    }

    // For testing: directly set a backend
    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = Some(backend);
        self
    }

    // Initialize the backend if needed
    async fn ensure_backend(&mut self) -> Result<&mut Backend, eyre::Error> {
        if self.backend.is_none() {
            let opts = if let Some(fork_info) = &self.fork_info {
                EvmOpts {
                    fork_url: fork_info.rpc_url.clone(),
                    fork_block_number: fork_info.block_number,
                    ..Default::default()
                }
            } else {
                EvmOpts::default()
            };

            let config = Config::default();
            let evm_env = opts.evm_env().await?;
            let backend = Backend::spawn(opts.get_fork(&config, evm_env))?;
            self.backend = Some(backend);
        }

        Ok(self.backend.as_mut().unwrap())
    }

    // Run the simulation and check for missing assets
    pub async fn check_transaction(
        &mut self,
        call: Call,
    ) -> Result<Vec<MissingAssetInfo>, eyre::Error> {
        // Get the env before borrowing self as mutable
        let env = self.env.clone().unwrap_or_default();

        // Ensure we have a backend
        let backend = self.ensure_backend().await?;

        // Set up the executor
        let mut executor = ExecutorBuilder::new()
            .inspectors(|stack| stack.trace_mode(TraceMode::Call))
            .build(env, backend.clone());

        // Run the simulation
        let result = executor.transact_raw(call.from, call.to, call.data, call.value)?;

        // Process traces and apply checkers
        let mut missing_assets = Vec::new();

        if result.exit_reason.is_revert() {
            if let Some(traces) = result.traces {
                if let Some(trace) = find_last_non_proxy_call(&traces) {
                    // Process all checkers and collect results more idiomatically
                    missing_assets = self
                        .checkers
                        .iter()
                        .filter_map(|checker| {
                            checker.identify_asset(trace).and_then(|asset| {
                                match checker.check_balance(asset, &mut executor) {
                                    Ok(missing_asset)
                                        if missing_asset.missing_amount > U256::ZERO =>
                                    {
                                        Some(missing_asset)
                                    }
                                    Ok(_) => None,
                                    Err(e) => {
                                        eprintln!("Error checking balance: {}", e);
                                        None
                                    }
                                }
                            })
                        })
                        .collect();
                }
            }
        }

        Ok(missing_assets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulate::{AssetType, checkers::erc20::transferFromCall};
    use alloy_primitives::Address as AAddress;
    use alloy_sol_types::{SolCall, sol};
    use forge::revm::primitives::{AccountInfo, Address, Bytecode, Bytes, U256};
    use std::str::FromStr;

    sol!(
        contract MockERC20 {
            function transferFrom(address from, address to, uint256 amount) public returns (bool) {
                return true;
            }
        }
    );

    #[tokio::test(flavor = "multi_thread")]
    async fn test_local_network() {
        let contract_address = Address::new([1; 20]);
        let mut backend_owner = Backend::spawn(None);
        let backend = backend_owner.as_mut().unwrap();

        let bytecode = Bytes::from_str("0x608060405234801561000f575f80fd5b506004361061004a575f3560e01c80632a1afcd91461004e57806342cbb15c1461006c57806360fe47b11461008a5780636d4ce63c146100a6575b5f80fd5b6100566100c4565b6040516100639190610130565b60405180910390f35b6100746100c9565b6040516100819190610130565b60405180910390f35b6100a4600480360381019061009f9190610177565b6100d0565b005b6100ae610110565b6040516100bb9190610130565b60405180910390f35b5f5481565b5f43905090565b805f819055507fe0dca1a932506e28dc1cd7f50b0604489287b36ba09c37f13b25ee518d813528816040516101059190610130565b60405180910390a150565b5f8054905090565b5f819050919050565b61012a81610118565b82525050565b5f6020820190506101435f830184610121565b92915050565b5f80fd5b61015681610118565b8114610160575f80fd5b50565b5f813590506101718161014d565b92915050565b5f6020828403121561018c5761018b610149565b5b5f61019984828501610163565b9150509291505056fea2646970667358221220f7399e877793618afbf93c1ab591511f69fa1330a3fd5526ff45418127a04af964736f6c634300081a0033").unwrap();
        let deployed_bytecode = Bytecode::new_raw(bytecode);

        backend.insert_account_info(
            contract_address,
            AccountInfo {
                code_hash: deployed_bytecode.hash_slow(),
                code: Some(deployed_bytecode),
                ..Default::default()
            },
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_empty_transaction() {
        let mut simulator = AssetSimulator::new()
            .with_fork("https://mainnet.base.org", Some(30155463))
            .with_erc20_checker();

        let call = Call::default();
        let result = simulator.check_transaction(call).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_transfer_from_with_insufficient_balance() {
        let mut simulator = AssetSimulator::new()
            .with_fork("https://mainnet.base.org", None)
            .with_erc20_checker();

        let sender = Address::new([1; 20]);
        let recipient = Address::new([2; 20]);
        let token = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap(); // USDC on Base
        let amount = U256::from(1000000000);

        let call = Call::new(
            sender,
            token,
            transferFromCall {
                from: AAddress::from_slice(sender.as_slice()),
                to: AAddress::from_slice(recipient.as_slice()),
                amount: amount,
            }
            .abi_encode(),
            U256::ZERO,
        );

        let result = simulator.check_transaction(call).await.unwrap();

        // We expect to find a missing asset since our test address likely doesn't have 1000 USDC
        assert!(!result.is_empty());
        if let Some(asset) = result.first() {
            assert_eq!(asset.token_address, token);
            assert_eq!(asset.account, sender);
            assert_eq!(asset.missing_amount, amount);
            assert_eq!(asset.required_amount, amount);
            assert_eq!(asset.asset_type, AssetType::ERC20);
            assert_eq!(asset.current_balance, U256::ZERO);
        }
    }
}
