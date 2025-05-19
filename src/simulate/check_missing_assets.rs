// use alloy_primitives::{Address, Bytes, U256};
use forge::{
    backend::Backend, executors::ExecutorBuilder, revm::{interpreter::InstructionResult, primitives::{Address, Bytes, Env, U256}}, traces::{CallKind, CallTrace, CallTraceDecoder, CallTraceDecoderBuilder, CallTraceNode, SparsedTraceArena, TraceMode}
};
use foundry_evm_core::opts::EvmOpts;
use foundry_config::Config;
use alloy_sol_types::{sol, SolCall, SolValue};
use std::collections::HashMap;

pub struct Call {
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
}

pub struct ForkInfo {
    pub rpc_url: Option<String>,
    pub block_number: Option<u64>,
}

#[derive(Debug)]
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

// Define ERC20 function signatures
sol! {
    function transfer(address to, uint256 amount) public returns (bool);
    function transferFrom(address from, address to, uint256 amount) public returns (bool);
    function balanceOf(address account) external view returns (uint256);
}

// Intermediate struct for identified assets before balance checking
struct PotentialMissingAsset {
    asset_type: AssetType,
    token_address: Address,
    account: Address,
    required_amount: U256,
}

// Core trait for checking a specific asset type
trait AssetChecker {
    fn name(&self) -> &'static str;
    
    // First phase: identify potential missing assets
    fn identify_asset(&self, trace: &CallTrace) -> Option<PotentialMissingAsset>;
    
    // Second phase: check balances and calculate missing amounts
    fn check_balance(
        &self, 
        asset: PotentialMissingAsset, 
        executor: &mut forge::executors::Executor
    ) -> Result<MissingAssetInfo, eyre::Error>;
}

// ERC20 checker implementation
struct ERC20Checker;

impl AssetChecker for ERC20Checker {
    fn name(&self) -> &'static str {
        "ERC20"
    }
    
    fn identify_asset(&self, trace: &CallTrace) -> Option<PotentialMissingAsset> {
        // Check for transfer
        if let Some(decoded) = transferCall::abi_decode(&trace.data).ok() {
            return Some(PotentialMissingAsset {
                asset_type: AssetType::ERC20,
                token_address: trace.address,
                account: trace.caller,
                required_amount: decoded.amount,
            });
        }
        
        // Check for transferFrom
        if let Some(decoded) = transferFromCall::abi_decode(&trace.data).ok() {
            return Some(PotentialMissingAsset {
                asset_type: AssetType::ERC20,
                token_address: trace.address,
                account: Address::from_slice(decoded.from.as_slice()),
                required_amount: decoded.amount,
            });
        }
        
        None
    }
    
    fn check_balance(
        &self, 
        asset: PotentialMissingAsset, 
        executor: &mut forge::executors::Executor
    ) -> Result<MissingAssetInfo, eyre::Error> {
        // Execute the balanceOf call
        let balance_call = balanceOfCall { 
            account: alloy_primitives::Address::from_slice(asset.account.as_slice())
        };
        let balance_data = balance_call.abi_encode();
        
        let balance_result = executor.call_raw(
            asset.account,
            asset.token_address,
            balance_data.into(),
            U256::ZERO
        )?;
        
        // Parse the result more idiomatically
        let current_balance = balance_result.out
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
                    // Check just this one trace with all checkers
                    for checker in &self.checkers {
                        if let Some(asset) = checker.identify_asset(trace) {
                            match checker.check_balance(asset, &mut executor) {
                                Ok(missing_asset) => {
                                    if missing_asset.missing_amount > U256::ZERO {
                                        missing_assets.push(missing_asset);
                                    }
                                },
                                Err(e) => {
                                    eprintln!("Error checking balance: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        }
        
        Ok(missing_assets)
    }
}

// Simplified function that returns only the last relevant trace
fn find_last_non_proxy_call(traces: &SparsedTraceArena) -> Option<&CallTrace> {
  let trace_list: Vec<&CallTrace> = traces.nodes().iter()
      .map(|node| &node.trace)
      .collect();
  
  // Start from the last trace and work backwards
  for (i, trace) in trace_list.iter().enumerate().rev() {
      // Skip pure proxy delegatecalls (where calldata matches exactly)
      if trace.kind == CallKind::DelegateCall && i > 0 {
          let prev_trace = trace_list[i-1];
          // If calldata matches exactly, this is likely a pure proxy - skip it
          if trace.data == prev_trace.data {
              continue;
          }
      }
      
      // Return this trace as it's either not a delegatecall or it's a delegatecall with modified data
      return Some(*trace);
  }
  
  None
}


// Public API - maintains backward compatibility
pub async fn check_missing_assets(
    call: Call,
    fork_info: ForkInfo,
) -> Result<Vec<MissingAssetInfo>, eyre::Error> {
    let mut checker = SimulationChecker::new()
        .with_fork(fork_info)
        .with_checker(ERC20Checker);
    
    checker.check_call(call).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge::revm::primitives::{hex::FromHex, Address, Bytes, U256};
    use alloy_primitives::{Address as AAddress};
    use alloy_sol_types::SolCall;

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

    impl Default for ForkInfo {
        fn default() -> Self {
            Self {
                rpc_url: Some("https://mainnet.base.org".to_string()),
                block_number: Some(30155463),
            }
        }
    }

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
        assert!(result.is_empty());
    }
}
