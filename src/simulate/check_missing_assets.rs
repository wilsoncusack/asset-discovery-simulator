// use alloy_primitives::{Address, Bytes, U256};
use forge::{
    backend::Backend, executors::ExecutorBuilder, revm::{interpreter::InstructionResult, primitives::{Address, Bytes, Env, U256}}, traces::{CallKind, CallTrace, CallTraceDecoder, CallTraceDecoderBuilder, CallTraceNode, SparsedTraceArena, TraceMode}
};
use foundry_evm_core::opts::EvmOpts;
use foundry_config::Config;
use alloy_sol_types::{sol, SolCall, SolValue};
use std::collections::HashMap;
use std::str::FromStr;

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
    // First phase: identify potential missing assets
    fn identify_asset(&self, trace: &CallTrace) -> Option<PotentialMissingAsset>;
    
    // Second phase: check balances and calculate missing amounts
    fn check_balance(
        &self, 
        asset: PotentialMissingAsset, 
        executor: &mut forge::executors::Executor
    ) -> Result<MissingAssetInfo, eyre::Error>;
}

// Define a trait for ERC20 transfer operations (now object-safe)
trait ERC20TransferCheck {
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
    fn get_account(&self, trace: &CallTrace) -> Address {
        Address::from_slice(self.from.as_slice())
    }
    
    fn get_amount(&self) -> U256 {
        self.amount
    }
}

// ERC20 checker implementation
struct ERC20Checker {
    // Store a list of transfer checkers
    transfer_checkers: Vec<fn(&[u8]) -> Option<Box<dyn ERC20TransferCheck>>>,
}

impl ERC20Checker {
    fn new() -> Self {
        // Initialize with all supported transfer types
        Self {
            transfer_checkers: vec![
                |data| transferCall::try_decode(data).map(|d| Box::new(d) as Box<dyn ERC20TransferCheck>),
                |data| transferFromCall::try_decode(data).map(|d| Box::new(d) as Box<dyn ERC20TransferCheck>),
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

// Simplified function that returns only the last relevant trace
fn find_last_non_proxy_call(traces: &SparsedTraceArena) -> Option<&CallTrace> {
    // Convert to a vector for easier iteration from the end
    let trace_list: Vec<&CallTrace> = traces.nodes().iter()
        .map(|node| &node.trace)
        .collect();
    
    // Use iterator methods for a more idiomatic approach
    trace_list.iter().rev()
        .find(|trace| {
            // If it's not a delegate call, it's definitely not a proxy
            if trace.kind != CallKind::DelegateCall {
                return true;
            }
            
            // For delegate calls, check if it's a pure proxy by comparing with previous trace
            let trace_idx = trace_list.iter().position(|t| t == *trace).unwrap();
            if trace_idx == 0 {
                return true; // First trace can't be a proxy of a previous one
            }
            
            // If calldata doesn't match exactly, it's not a pure proxy
            trace.data != trace_list[trace_idx - 1].data
        })
        .copied()
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
