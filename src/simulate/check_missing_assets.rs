// use alloy_primitives::{Address, Bytes, U256};
use forge::{
    backend::Backend, executors::ExecutorBuilder, revm::primitives::{Env, U256}, traces::TraceMode
};
use foundry_evm_core::opts::EvmOpts;
use foundry_config::Config;
use crate::simulate::types::{Call, ForkInfo, MissingAssetInfo};
use crate::simulate::checkers::{AssetChecker, ERC20Checker};
use crate::simulate::utils::find_last_non_proxy_call;


// Main checker that orchestrates simulation and checking
pub struct SimulationChecker {
    fork_info: Option<ForkInfo>,
    env: Option<Env>,
    backend: Option<Backend>,
    checkers: Vec<Box<dyn AssetChecker>>,
}

impl SimulationChecker {
    // Create a new checker with no settings
    pub fn new() -> Self {
        Self {
            fork_info: None,
            env: None,
            backend: None,
            checkers: Vec::new(),
        }
    }
    
    // Add fork information
    pub fn with_fork(mut self, fork_info: ForkInfo) -> Self {
        self.fork_info = Some(fork_info);
        self
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
    pub async fn check_call(&mut self, call: Call) -> Result<Vec<MissingAssetInfo>, eyre::Error> {
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
                    missing_assets = self.checkers.iter()
                        .filter_map(|checker| {
                            checker.identify_asset(trace)
                                .and_then(|asset| {
                                    match checker.check_balance(asset, &mut executor) {
                                        Ok(missing_asset) if missing_asset.missing_amount > U256::ZERO => {
                                            Some(missing_asset)
                                        },
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

// Public API - maintains backward compatibility
pub async fn check_missing_assets(
    call: Call,
    fork_info: ForkInfo,
) -> Result<Vec<MissingAssetInfo>, eyre::Error> {
    let mut checker = SimulationChecker::new()
        .with_fork(fork_info)
        .with_checker(ERC20Checker::new());
    
    checker.check_call(call).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge::revm::primitives::{hex::FromHex, Address, Bytes, U256};
    use alloy_primitives::{Address as AAddress};
    use alloy_sol_types::SolCall;
    use crate::simulate::checkers::erc20::{transferCall, transferFromCall};
    use std::str::FromStr;

    #[tokio::test(flavor = "multi_thread")]
    async fn null() {
        let result = check_missing_assets(Call::default(), ForkInfo::default())
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_direct_transfer_with_zero_balance() {
        let call = Call {
            to: Address::from_hex("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap(),
            data: transferCall {
                to: AAddress::new([1; 20]),
                amount: U256::from(1)
            }.abi_encode().into(),
            ..Default::default()
        };

        let result = check_missing_assets(call, ForkInfo::default())
            .await
            .unwrap();
        println!("result: {:?}", result.first());
        // assert!(result.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_transfer_from_with_insufficient_balance() {
        let sender = Address::from_str("0x1111111111111111111111111111111111111111").unwrap();
        let recipient = Address::from_str("0x2222222222222222222222222222222222222222").unwrap();
        let token = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap(); // USDC on Base
        
        let call = Call {
            from: sender,
            to: token,
            data: transferFromCall {
                from: AAddress::from_slice(sender.as_slice()),
                to: AAddress::from_slice(recipient.as_slice()),
                amount: U256::from(1000000000) // 1000 USDC (6 decimals)
            }.abi_encode().into(),
            value: U256::ZERO,
        };

        let result = check_missing_assets(call, ForkInfo::default())
            .await
            .unwrap();
        
        // We expect to find a missing asset since our test address likely doesn't have 1000 USDC
        assert!(!result.is_empty());
        if let Some(asset) = result.first() {
            assert_eq!(asset.token_address, token);
            assert_eq!(asset.account, sender);
            assert!(asset.missing_amount > U256::ZERO);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_simulation_checker_builder_pattern() {
        // Test the builder pattern for SimulationChecker
        let mut checker = SimulationChecker::new()
            .with_fork(ForkInfo::default())
            .with_checker(ERC20Checker::new());
            
        let call = Call::default();
        let result = checker.check_call(call).await.unwrap();
        assert!(result.is_empty());
    }
}
