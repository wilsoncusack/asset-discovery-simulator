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
use alloy_primitives::Address as AAddress;
use alloy_sol_types::{SolCall, sol};
use forge::revm::primitives::{AccountInfo, Address, Bytecode, Bytes};
use std::str::FromStr;

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

    // Initialize the backend if needed and return ownership
    async fn ensure_backend(&mut self) -> Result<Backend, eyre::Error> {
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

        Ok(self.backend.take().unwrap())
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

        // Set up the executor - use the backend directly
        let mut executor = ExecutorBuilder::new()
            .inspectors(|stack| stack.trace_mode(TraceMode::Call))
            .build(env, backend);

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
            function transferFrom(address from, address to, uint256 amount) public returns (bool);
            function transfer(address to, uint256 amount) public returns (bool);
            function balanceOf(address account) public view returns (uint256);
            function approve(address spender, uint256 amount) public returns (bool);
            function allowance(address owner, address spender) public view returns (uint256);
            function mint(address to, uint256 amount) public;
        }
    );

    // Helper function for minting tokens in tests
    async fn mint_tokens(
        simulator: &mut AssetSimulator,
        contract_address: Address,
        minter: Address,
        to: Address,
        amount: U256,
    ) -> Result<(), eyre::Error> {
        let backend = simulator.ensure_backend().await?;
        let env = Env::default();
        let mut executor = ExecutorBuilder::new()
            .inspectors(|stack| stack.trace_mode(TraceMode::Call))
            .build(env, backend);

        let mint_result = executor.transact_raw(
            minter,
            contract_address,
            MockERC20::mintCall {
                to: AAddress::from_slice(to.as_slice()),
                amount,
            }
            .abi_encode().into(),
            U256::ZERO,
        )?;
        
        assert!(!mint_result.exit_reason.is_revert(), "Minting should succeed");
        Ok(())
    }

    // Shared setup for local network tests
    async fn setup_local_erc20_test() -> Result<(AssetSimulator, Address, Address, Address, Address, Address, U256), eyre::Error> {
        let contract_address = Address::new([1; 20]);
        
        // Create a unique backend for each test by using a different address
        let mut backend_owner = Backend::spawn(None);
        let backend = backend_owner.as_mut().unwrap();

        let bytecode = Bytes::from_str("0x608060405234801561000f575f80fd5b506004361061009c575f3560e01c806340c10f191161006457806340c10f191461015a57806370a082311461017657806395d89b41146101a6578063a9059cbb146101c4578063dd62ed3e146101f45761009c565b806306fdde03146100a0578063095ea7b3146100be57806318160ddd146100ee57806323b872dd1461010c578063313ce5671461013c575b5f80fd5b6100a8610224565b6040516100b59190610b5c565b60405180910390f35b6100d860048036038101906100d39190610c0d565b6102af565b6040516100e59190610c65565b60405180910390f35b6100f6610478565b6040516101039190610c8d565b60405180910390f35b61012660048036038101906101219190610ca6565b61047e565b6040516101339190610c65565b60405180910390f35b610144610655565b6040516101519190610d11565b60405180910390f35b610174600480360381019061016f9190610c0d565b610667565b005b610190600480360381019061018b9190610d2a565b6107a9565b60405161019d9190610c8d565b60405180910390f35b6101ae6107be565b6040516101bb9190610b5c565b60405180910390f35b6101de60048036038101906101d99190610c0d565b61084a565b6040516101eb9190610c65565b60405180910390f35b61020e60048036038101906102099190610d55565b610860565b60405161021b9190610c8d565b60405180910390f35b5f805461023090610dc0565b80601f016020809104026020016040519081016040528092919081815260200182805461025c90610dc0565b80156102a75780601f1061027e576101008083540402835291602001916102a7565b820191905f5260205f20905b81548152906001019060200180831161028a57829003601f168201915b505050505081565b5f8073ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff160361031e576040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161031590610e60565b60405180910390fd5b5f73ffffffffffffffffffffffffffffffffffffffff168373ffffffffffffffffffffffffffffffffffffffff160361038c576040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161038390610eee565b60405180910390fd5b8160055f3373ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f8573ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f20819055508273ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff167f8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925846040516104669190610c8d565b60405180910390a36001905092915050565b60035481565b5f8160055f8673ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f3373ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f2054101561053a576040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161053190610f7c565b60405180910390fd5b610545848484610880565b5f60055f8673ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f3373ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f2054905082816105cd9190610fc7565b60055f8773ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f3373ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f208190555060019150509392505050565b60025f9054906101000a900460ff1681565b5f73ffffffffffffffffffffffffffffffffffffffff168273ffffffffffffffffffffffffffffffffffffffff16036106d5576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004016106cc90611044565b60405180910390fd5b8060035f8282546106e69190611062565b925050819055508060045f8473ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f8282546107399190611062565b925050819055508173ffffffffffffffffffffffffffffffffffffffff165f73ffffffffffffffffffffffffffffffffffffffff167fddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef8360405161079d9190610c8d565b60405180910390a35050565b6004602052805f5260405f205f915090505481565b600180546107cb90610dc0565b80601f01602080910402602001604051908101604052809291908181526020018280546107f790610dc0565b80156108425780601f1061081957610100808354040283529160200191610842565b820191905f5260205f20905b81548152906001019060200180831161082557829003601f168201915b505050505081565b5f610856338484610880565b6001905092915050565b6005602052815f5260405f20602052805f5260405f205f91509150505481565b5f73ffffffffffffffffffffffffffffffffffffffff168373ffffffffffffffffffffffffffffffffffffffff16036108ee576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004016108e590611105565b60405180910390fd5b5f73ffffffffffffffffffffffffffffffffffffffff168273ffffffffffffffffffffffffffffffffffffffff160361095c576040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161095390611193565b60405180910390fd5b8060045f8573ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205410156109dc576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004016109d390611221565b60405180910390fd5b8060045f8573ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f828254610a289190610fc7565b925050819055508060045f8473ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f828254610a7b9190611062565b925050819055508173ffffffffffffffffffffffffffffffffffffffff168373ffffffffffffffffffffffffffffffffffffffff167fddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef83604051610adf9190610c8d565b60405180910390a3505050565b5f81519050919050565b5f82825260208201905092915050565b8281835e5f83830152505050565b5f601f19601f8301169050919050565b5f610b2e82610aec565b610b388185610af6565b9350610b48818560208601610b06565b610b5181610b14565b840191505092915050565b5f6020820190508181035f830152610b748184610b24565b905092915050565b5f80fd5b5f73ffffffffffffffffffffffffffffffffffffffff82169050919050565b5f610ba982610b80565b9050919050565b610bb981610b9f565b8114610bc3575f80fd5b50565b5f81359050610bd481610bb0565b92915050565b5f819050919050565b610bec81610bda565b8114610bf6575f80fd5b50565b5f81359050610c0781610be3565b92915050565b5f8060408385031215610c2357610c22610b7c565b5b5f610c3085828601610bc6565b9250506020610c4185828601610bf9565b9150509250929050565b5f8115159050919050565b610c5f81610c4b565b82525050565b5f602082019050610c785f830184610c56565b92915050565b610c8781610bda565b82525050565b5f602082019050610ca05f830184610c7e565b92915050565b5f805f60608486031215610cbd57610cbc610b7c565b5b5f610cca86828701610bc6565b9350506020610cdb86828701610bc6565b9250506040610cec86828701610bf9565b9150509250925092565b5f60ff82169050919050565b610d0b81610cf6565b82525050565b5f602082019050610d245f830184610d02565b92915050565b5f60208284031215610d3f57610d3e610b7c565b5b5f610d4c84828501610bc6565b91505092915050565b5f8060408385031215610d6b57610d6a610b7c565b5b5f610d7885828601610bc6565b9250506020610d8985828601610bc6565b9150509250929050565b7f4e487b71000000000000000000000000000000000000000000000000000000005f52602260045260245ffd5b5f6002820490506001821680610dd757607f821691505b602082108103610dea57610de9610d93565b5b50919050565b7f45524332303a20617070726f76652066726f6d20746865207a65726f206164645f8201527f7265737300000000000000000000000000000000000000000000000000000000602082015250565b5f610e4a602483610af6565b9150610e5582610df0565b604082019050919050565b5f6020820190508181035f830152610e7781610e3e565b9050919050565b7f45524332303a20617070726f766520746f20746865207a65726f2061646472655f8201527f7373000000000000000000000000000000000000000000000000000000000000602082015250565b5f610ed8602283610af6565b9150610ee382610e7e565b604082019050919050565b5f6020820190508181035f830152610f0581610ecc565b9050919050565b7f45524332303a207472616e7366657220616d6f756e74206578636565647320615f8201527f6c6c6f77616e6365000000000000000000000000000000000000000000000000602082015250565b5f610f66602883610af6565b9150610f7182610f0c565b604082019050919050565b5f6020820190508181035f830152610f9381610f5a565b9050919050565b7f4e487b71000000000000000000000000000000000000000000000000000000005f52601160045260245ffd5b5f610fd182610bda565b9150610fdc83610bda565b9250828203905081811115610ff457610ff3610f9a565b5b92915050565b7f45524332303a206d696e7420746f20746865207a65726f2061646472657373005f82015250565b5f61102e601f83610af6565b915061103982610ffa565b602082019050919050565b5f6020820190508181035f83015261105b81611022565b9050919050565b5f61106c82610bda565b915061107783610bda565b925082820190508082111561108f5761108e610f9a565b5b92915050565b7f45524332303a207472616e736665722066726f6d20746865207a65726f2061645f8201527f6472657373000000000000000000000000000000000000000000000000000000602082015250565b5f6110ef602583610af6565b91506110fa82611095565b604082019050919050565b5f6020820190508181035f83015261111c816110e3565b9050919050565b7f45524332303a207472616e7366657220746f20746865207a65726f20616464725f8201527f6573730000000000000000000000000000000000000000000000000000000000602082015250565b5f61117d602383610af6565b915061118882611123565b604082019050919050565b5f6020820190508181035f8301526111aa81611171565b9050919050565b7f45524332303a207472616e7366657220616d6f756e74206578636565647320625f8201527f616c616e63650000000000000000000000000000000000000000000000000000602082015250565b5f61120b602683610af6565b9150611216826111b1565b604082019050919050565b5f6020820190508181035f830152611238816111ff565b905091905056fea26469706673582212207498205d767944fba8d1f2a840dde04cbb33d4a6f07ceaeb424db38a43e951cf64736f6c634300081a0033")?;
        let deployed_bytecode = Bytecode::new_raw(bytecode);

        backend.insert_account_info(
            contract_address,
            AccountInfo {
                code_hash: deployed_bytecode.hash_slow(),
                code: Some(deployed_bytecode),
                ..Default::default()
            },
        );

        // Use unique addresses for each test to avoid conflicts
        let test_id = std::thread::current().id();
        let test_hash = format!("{:?}", test_id).chars().take(8).collect::<String>();
        
        let sender = Address::from_str(&format!("0x100000000000000000000000000000000000{:0>4}", &test_hash[..4])).unwrap_or(Address::from_str("0x1000000000000000000000000000000000000001").unwrap());
        let recipient = Address::from_str(&format!("0x200000000000000000000000000000000000{:0>4}", &test_hash[..4])).unwrap_or(Address::from_str("0x2000000000000000000000000000000000000002").unwrap());
        let spender = Address::from_str(&format!("0x300000000000000000000000000000000000{:0>4}", &test_hash[..4])).unwrap_or(Address::from_str("0x3000000000000000000000000000000000000003").unwrap());
        let minter = Address::from_str(&format!("0x400000000000000000000000000000000000{:0>4}", &test_hash[..4])).unwrap_or(Address::from_str("0x4000000000000000000000000000000000000004").unwrap());

        // Set up EOA accounts with some ETH for gas
        let eoa_info = AccountInfo {
            balance: U256::from(1000000000000000000u64), // 1 ETH
            ..Default::default()
        };
        
        backend.insert_account_info(sender, eoa_info.clone());
        backend.insert_account_info(recipient, eoa_info.clone());
        backend.insert_account_info(spender, eoa_info.clone());
        backend.insert_account_info(minter, eoa_info);

        let amount = U256::from(100);

        // Create the simulator with the backend
        let simulator = AssetSimulator::new()
            .with_backend(backend_owner.unwrap())
            .with_erc20_checker();

        Ok((simulator, contract_address, sender, recipient, spender, minter, amount))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_successful_transfer_returns_empty() -> Result<(), eyre::Error> {
        let (mut simulator, contract_address, sender, recipient, _spender, minter, amount) = 
            setup_local_erc20_test().await?;

        // Mint tokens to sender
        mint_tokens(&mut simulator, contract_address, minter, sender, amount * U256::from(2)).await?;

        // Test a successful transfer
        let transfer_call = Call::new(
            sender,
            contract_address,
            MockERC20::transferCall {
                to: AAddress::from_slice(recipient.as_slice()),
                amount,
            }
            .abi_encode(),
            U256::ZERO,
        );

        let result = simulator.check_transaction(transfer_call).await?;
        assert!(result.is_empty(), "Successful transfer should return no missing assets");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_insufficient_balance_detected() -> Result<(), eyre::Error> {
        let (mut simulator, contract_address, sender, recipient, _spender, _minter, amount) = 
            setup_local_erc20_test().await?;

        // Debug: Check initial balance
        let backend = simulator.ensure_backend().await?;
        let env = Env::default();
        let mut executor = ExecutorBuilder::new()
            .inspectors(|stack| stack.trace_mode(TraceMode::Call))
            .build(env, backend);

        // Check balance before the transfer
        let balance_result = executor.transact_raw(
            sender,
            contract_address,
            MockERC20::balanceOfCall {
                account: AAddress::from_slice(sender.as_slice()),
            }
            .abi_encode().into(),
            U256::ZERO,
        )?;
        
        println!("Balance check result: {:?}", balance_result);
        println!("Balance check output: {:?}", balance_result.result);

        // Now test transfer with insufficient balance
        let transfer_call = Call::new(
            sender,
            contract_address,
            MockERC20::transferCall {
                to: AAddress::from_slice(recipient.as_slice()),
                amount,
            }
            .abi_encode(),
            U256::ZERO,
        );

        // Create a fresh simulator for the actual test
        let (mut fresh_simulator, _, _, _, _, _, _) = setup_local_erc20_test().await?;
        let result = fresh_simulator.check_transaction(transfer_call).await?;
        println!("result: {:?}", result);
        assert!(!result.is_empty(), "Should detect missing balance");
        
        if let Some(asset) = result.first() {
            assert_eq!(asset.token_address, contract_address);
            assert_eq!(asset.account, sender);
            assert_eq!(asset.missing_amount, amount);
            assert_eq!(asset.required_amount, amount);
            assert_eq!(asset.asset_type, AssetType::ERC20);
            assert_eq!(asset.current_balance, U256::ZERO);
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_allowance_issue_not_balance_issue() -> Result<(), eyre::Error> {
        let (mut simulator, contract_address, sender, recipient, spender, minter, amount) = 
            setup_local_erc20_test().await?;

        // Mint tokens to sender (so balance is sufficient)
        mint_tokens(&mut simulator, contract_address, minter, sender, amount * U256::from(2)).await?;

        // Attempt transferFrom without approval - this should revert due to allowance, not balance
        let transfer_from_call = Call::new(
            spender, // Spender trying to transfer from sender
            contract_address,
            MockERC20::transferFromCall {
                from: AAddress::from_slice(sender.as_slice()),
                to: AAddress::from_slice(recipient.as_slice()),
                amount, // Amount that sender has, but spender is not approved for
            }
            .abi_encode(),
            U256::ZERO,
        );

        let result = simulator.check_transaction(transfer_from_call).await?;
        // This should return empty because the revert is due to missing allowance, not insufficient balance
        // The ERC20Checker should only identify balance issues, not allowance issues
        assert!(result.is_empty(), "Should not detect balance issues when problem is allowance");
        Ok(())
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
    async fn test_usdc_proxy_on_base() {
        let mut simulator = AssetSimulator::new()
            .with_fork("https://mainnet.base.org", None)
            .with_erc20_checker();

        let sender = Address::new([1; 20]);
        let recipient = Address::new([2; 20]);
        let token = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap(); // USDC on Base
        let amount = U256::from(1000000000); // 1000 USDC

        let call = Call::new(
            sender,
            token,
            transferFromCall {
                from: AAddress::from_slice(sender.as_slice()),
                to: AAddress::from_slice(recipient.as_slice()),
                amount,
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
