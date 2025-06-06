use crate::simulate::builder::AssetSimulatorBuilder;
use crate::simulate::checkers::AssetChecker;
use crate::simulate::types::{AssetContext, AssetSpec, Call, MissingAssetInfo};
use crate::simulate::utils::find_last_non_proxy_call;
use forge::executors::Executor;
use forge::revm::primitives::{Address, U256};
use log::{error, info};

// Main simulator that orchestrates simulation and checking
pub struct AssetSimulator {
    executor: Executor,
    checkers: Vec<Box<dyn AssetChecker>>,
}

impl AssetSimulator {
    /// Entry-point for users – returns the fluent builder.
    pub fn builder() -> AssetSimulatorBuilder {
        AssetSimulatorBuilder::default()
    }

    /// Internal helper used only by the builder.
    pub(crate) fn new_from_parts(executor: Executor, checkers: Vec<Box<dyn AssetChecker>>) -> Self {
        Self { executor, checkers }
    }

    /// Mutable access for advanced helpers/tests.
    pub fn executor_mut(&mut self) -> &mut Executor {
        &mut self.executor
    }

    // ========================================================================
    //  TRANSACTION CHECKING
    // ========================================================================
    pub async fn check_transaction(
        &mut self,
        call: Call,
    ) -> Result<Vec<MissingAssetInfo>, eyre::Error> {
        self.check_transaction_with_options(call, true, 10).await
    }

    pub async fn check_transaction_with_options(
        &mut self,
        call: Call,
        auto_fix: bool,
        max_iterations: usize,
    ) -> Result<Vec<MissingAssetInfo>, eyre::Error> {
        let mut all_missing_assets = Vec::new();

        for _iteration in 0..max_iterations {
            // Run the simulation
            let result =
                self.executor
                    .transact_raw(call.from, call.to, call.data.clone(), call.value)?;

            // Transaction succeeded → done
            if !result.exit_reason.is_revert() {
                break;
            }

            // ── process traces and apply checkers ─────────────────────────────
            let mut found_any_missing = false;
            if let Some(traces) = result.traces {
                if let Some(trace) = find_last_non_proxy_call(&traces) {
                    for checker in &self.checkers {
                        if let Some(potential_asset) = checker.identify_asset(trace) {
                            match checker.check_balance(potential_asset.clone(), &mut self.executor)
                            {
                                Ok(missing) if missing.missing_amount > U256::ZERO => {
                                    // Always record what the checker returns; if the
                                    // same asset shows up again in a later iteration
                                    // (e.g. higher amount needed) we still want to
                                    // capture it.
                                    all_missing_assets.push(missing.clone());
                                    found_any_missing = true;

                                    if auto_fix {
                                        let ctx = AssetContext::from_trace(
                                            potential_asset,
                                            trace.clone(),
                                        );
                                        info!("Dealing asset for {:?}", missing.account);
                                        checker.deal(
                                            missing.account,
                                            missing.required,
                                            &mut self.executor,
                                            &ctx,
                                        )?;
                                    }
                                }
                                Ok(_) => {} // balance fine
                                Err(e) => error!(
                                    "{:?} error while checking balance: {}",
                                    checker.asset_type(),
                                    e
                                ),
                            }
                        }
                    }
                }
            }

            // Either not auto-fixing or nothing missing → we're done
            if auto_fix && found_any_missing {
                continue; // another simulation round
            }
            break; // exit the loop, aggregate once
        }

        // Hit max_iterations OR broke out of the loop for any reason
        Ok(Self::aggregate_missing_assets(all_missing_assets))
    }

    // --------------------------------------------------------------------
    //  Helper: merge duplicates (same account & asset)
    // --------------------------------------------------------------------
    fn aggregate_missing_assets(assets: Vec<MissingAssetInfo>) -> Vec<MissingAssetInfo> {
        use std::collections::HashMap;

        let mut map: HashMap<(Address, AssetSpec), MissingAssetInfo> = HashMap::new();

        for a in assets {
            let key = (a.account, a.required.clone());

            map.entry(key)
                .and_modify(|existing| {
                    // accumulate amounts
                    existing.missing_amount += a.missing_amount;
                    if let AssetSpec::ERC20 { token, amount } = &mut existing.required {
                        if let AssetSpec::ERC20 {
                            amount: add_amt, ..
                        } = a.required
                        {
                            *amount += add_amt;
                        }
                    }
                })
                .or_insert(a);
        }

        map.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulate::{checkers::erc20::transferFromCall, types::AssetSpec};
    use alloy_primitives::Address as AAddress;
    use alloy_sol_types::{SolCall, sol};
    use forge::revm::primitives::{Address, Bytes, U256};
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

    sol!(
        struct TargetCall {
            address target;
            bytes   callData;
        }

        contract Multicall3 {
            function aggregate(TargetCall[] calls)
                public
                payable
                returns (uint256 blockNumber, bytes[] returnData);
        }

        contract IERC20 {
            function transfer(address to, uint256 amount) public returns (bool);
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
        let exec = simulator.executor_mut();
        let res = exec.transact_raw(
            minter,
            contract_address,
            MockERC20::mintCall {
                to: AAddress::from_slice(to.as_slice()),
                amount,
            }
            .abi_encode()
            .into(),
            U256::ZERO,
        )?;
        assert!(!res.exit_reason.is_revert(), "Mint should succeed");
        Ok(())
    }

    // Simplified setup that just deploys the contract and returns basic info
    async fn setup_local_erc20_test() -> Result<(AssetSimulator, Address), eyre::Error> {
        let minter = Address::repeat_byte(4);

        let mut simulator = AssetSimulator::builder()
            .with_erc20_checker()
            .build()
            .await?;

        let exec = simulator.executor_mut();

        // Deploy the ERC20 contract using the executor's deploy method
        let bytecode = Bytes::from_str(
            "60806040526040518060400160405280600481526020017f4d6f636b000000000000000000000000000000000000000000000000000000008152505f908161004791906102f3565b506040518060400160405280600381526020017f4d434b00000000000000000000000000000000000000000000000000000000008152506001908161008c91906102f3565b50601260025f6101000a81548160ff021916908360ff1602179055503480156100b3575f80fd5b506103c2565b5f81519050919050565b7f4e487b71000000000000000000000000000000000000000000000000000000005f52604160045260245ffd5b7f4e487b71000000000000000000000000000000000000000000000000000000005f52602260045260245ffd5b5f600282049050600182168061013457607f821691505b602082108103610147576101466100f0565b5b50919050565b5f819050815f5260205f209050919050565b5f6020601f8301049050919050565b5f82821b905092915050565b5f600883026101a97fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff8261016e565b6101b3868361016e565b95508019841693508086168417925050509392505050565b5f819050919050565b5f819050919050565b5f6101f76101f26101ed846101cb565b6101d4565b6101cb565b9050919050565b5f819050919050565b610210836101dd565b61022461021c826101fe565b84845461017a565b825550505050565b5f90565b61023861022c565b610243818484610207565b505050565b5b818110156102665761025b5f82610230565b600181019050610249565b5050565b601f8211156102ab5761027c8161014d565b6102858461015f565b81016020851015610294578190505b6102a86102a08561015f565b830182610248565b50505b505050565b5f82821c905092915050565b5f6102cb5f19846008026102b0565b1980831691505092915050565b5f6102e383836102bc565b9150826002028217905092915050565b6102fc826100b9565b67ffffffffffffffff811115610315576103146100c3565b5b61031f825461011d565b61032a82828561026a565b5f60209050601f83116001811461035b575f8415610349578287015190505b61035385826102d8565b8655506103ba565b601f1984166103698661014d565b5f5b828110156103905784890151825560018201915060208501945060208101905061036b565b868310156103ad57848901516103a9601f8916826102bc565b8355505b6001600288020188555050505b505050505050565b611275806103cf5f395ff3fe608060405234801561000f575f80fd5b506004361061009c575f3560e01c806340c10f191161006457806340c10f191461015a57806370a082311461017657806395d89b41146101a6578063a9059cbb146101c4578063dd62ed3e146101f45761009c565b806306fdde03146100a0578063095ea7b3146100be57806318160ddd146100ee57806323b872dd1461010c578063313ce5671461013c575b5f80fd5b6100a8610224565b6040516100b59190610b5c565b60405180910390f35b6100d860048036038101906100d39190610c0d565b6102af565b6040516100e59190610c65565b60405180910390f35b6100f6610478565b6040516101039190610c8d565b60405180910390f35b61012660048036038101906101219190610ca6565b61047e565b6040516101339190610c65565b60405180910390f35b610144610655565b6040516101519190610d11565b60405180910390f35b610174600480360381019061016f9190610c0d565b610667565b005b610190600480360381019061018b9190610d2a565b6107a9565b60405161019d9190610c8d565b60405180910390f35b6101ae6107be565b6040516101bb9190610b5c565b60405180910390f35b6101de60048036038101906101d99190610c0d565b61084a565b6040516101eb9190610c65565b60405180910390f35b61020e60048036038101906102099190610d55565b610860565b60405161021b9190610c8d565b60405180910390f35b5f805461023090610dc0565b80601f016020809104026020016040519081016040528092919081815260200182805461025c90610dc0565b80156102a75780601f1061027e576101008083540402835291602001916102a7565b820191905f5260205f20905b81548152906001019060200180831161028a57829003601f168201915b505050505081565b5f8073ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff160361031e576040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161031590610e60565b60405180910390fd5b5f73ffffffffffffffffffffffffffffffffffffffff168373ffffffffffffffffffffffffffffffffffffffff160361038c576040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161038390610eee565b60405180910390fd5b8160055f3373ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f8573ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f20819055508273ffffffffffffffffffffffffffffffffffffffff163373ffffffffffffffffffffffffffffffffffffffff167f8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925846040516104669190610c8d565b60405180910390a36001905092915050565b60035481565b5f8160055f8673ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f3373ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f2054101561053a576040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161053190610f7c565b60405180910390fd5b610545848484610880565b5f60055f8673ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f3373ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f2054905082816105cd9190610fc7565b60055f8773ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f3373ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f208190555060019150509392505050565b60025f9054906101000a900460ff1681565b5f73ffffffffffffffffffffffffffffffffffffffff168273ffffffffffffffffffffffffffffffffffffffff16036106d5576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004016106cc90611044565b60405180910390fd5b8060035f8282546106e69190611062565b925050819055508060045f8473ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f8282546107399190611062565b925050819055508173ffffffffffffffffffffffffffffffffffffffff165f73ffffffffffffffffffffffffffffffffffffffff167fddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef8360405161079d9190610c8d565b60405180910390a35050565b6004602052805f5260405f205f915090505481565b600180546107cb90610dc0565b80601f01602080910402602001604051908101604052809291908181526020018280546107f790610dc0565b80156108425780601f1061081957610100808354040283529160200191610842565b820191905f5260205f20905b81548152906001019060200180831161082557829003601f168201915b505050505081565b5f610856338484610880565b6001905092915050565b6005602052815f5260405f20602052805f5260405f205f91509150505481565b5f73ffffffffffffffffffffffffffffffffffffffff168373ffffffffffffffffffffffffffffffffffffffff16036108ee576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004016108e590611105565b60405180910390fd5b5f73ffffffffffffffffffffffffffffffffffffffff168273ffffffffffffffffffffffffffffffffffffffff160361095c576040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161095390611193565b60405180910390fd5b8060045f8573ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205410156109dc576040517f08c379a00000000000000000000000000000000000000000000000000000000081526004016109d390611221565b60405180910390fd5b8060045f8573ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f828254610a289190610fc7565b925050819055508060045f8473ffffffffffffffffffffffffffffffffffffffff1673ffffffffffffffffffffffffffffffffffffffff1681526020019081526020015f205f828254610a7b9190611062565b925050819055508173ffffffffffffffffffffffffffffffffffffffff168373ffffffffffffffffffffffffffffffffffffffff167fddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef83604051610adf9190610c8d565b60405180910390a3505050565b5f81519050919050565b5f82825260208201905092915050565b8281835e5f83830152505050565b5f601f19601f8301169050919050565b5f610b2e82610aec565b610b388185610af6565b9350610b48818560208601610b06565b610b5181610b14565b840191505092915050565b5f6020820190508181035f830152610b748184610b24565b905092915050565b5f80fd5b5f73ffffffffffffffffffffffffffffffffffffffff82169050919050565b5f610ba982610b80565b9050919050565b610bb981610b9f565b8114610bc3575f80fd5b50565b5f81359050610bd481610bb0565b92915050565b5f819050919050565b610bec81610bda565b8114610bf6575f80fd5b50565b5f81359050610c0781610be3565b92915050565b5f8060408385031215610c2357610c22610b7c565b5b5f610c3085828601610bc6565b9250506020610c4185828601610bf9565b9150509250929050565b5f8115159050919050565b610c5f81610c4b565b82525050565b5f602082019050610c785f830184610c56565b92915050565b610c8781610bda565b82525050565b5f602082019050610ca05f830184610c7e565b92915050565b5f805f60608486031215610cbd57610cbc610b7c565b5b5f610cca86828701610bc6565b9350506020610cdb86828701610bc6565b9250506040610cec86828701610bf9565b9150509250925092565b5f60ff82169050919050565b610d0b81610cf6565b82525050565b5f602082019050610d245f830184610d02565b92915050565b5f60208284031215610d3f57610d3e610b7c565b5b5f610d4c84828501610bc6565b91505092915050565b5f8060408385031215610d6b57610d6a610b7c565b5b5f610d7885828601610bc6565b9250506020610d8985828601610bc6565b9150509250929050565b7f4e487b71000000000000000000000000000000000000000000000000000000005f52602260045260245ffd5b5f6002820490506001821680610dd757607f821691505b602082108103610dea57610de9610d93565b5b50919050565b7f45524332303a20617070726f76652066726f6d20746865207a65726f206164645f8201527f7265737300000000000000000000000000000000000000000000000000000000602082015250565b5f610e4a602483610af6565b9150610e5582610df0565b604082019050919050565b5f6020820190508181035f830152610e7781610e3e565b9050919050565b7f45524332303a20617070726f766520746f20746865207a65726f2061646472655f8201527f7373000000000000000000000000000000000000000000000000000000000000602082015250565b5f610ed8602283610af6565b9150610ee382610e7e565b604082019050919050565b5f6020820190508181035f830152610f0581610ecc565b9050919050565b7f45524332303a207472616e7366657220616d6f756e74206578636565647320615f8201527f6c6c6f77616e6365000000000000000000000000000000000000000000000000602082015250565b5f610f66602883610af6565b9150610f7182610f0c565b604082019050919050565b5f6020820190508181035f830152610f9381610f5a565b9050919050565b7f4e487b71000000000000000000000000000000000000000000000000000000005f52601160045260245ffd5b5f610fd182610bda565b9150610fdc83610bda565b9250828203905081811115610ff457610ff3610f9a565b5b92915050565b7f45524332303a206d696e7420746f20746865207a65726f2061646472657373005f82015250565b5f61102e601f83610af6565b915061103982610ffa565b602082019050919050565b5f6020820190508181035f83015261105b81611022565b9050919050565b5f61106c82610bda565b915061107783610bda565b925082820190508082111561108f5761108e610f9a565b5b92915050565b7f45524332303a207472616e736665722066726f6d20746865207a65726f2061645f8201527f6472657373000000000000000000000000000000000000000000000000000000602082015250565b5f6110ef602583610af6565b91506110fa82611095565b604082019050919050565b5f6020820190508181035f83015261111c816110e3565b9050919050565b7f45524332303a207472616e7366657220746f20746865207a65726f20616464725f8201527f6573730000000000000000000000000000000000000000000000000000000000602082015250565b5f61117d602383610af6565b915061118882611123565b604082019050919050565b5f6020820190508181035f8301526111aa81611171565b9050919050565b7f45524332303a207472616e7366657220616d6f756e74206578636565647320625f8201527f616c616e63650000000000000000000000000000000000000000000000000000602082015250565b5f61120b602683610af6565b9150611216826111b1565b604082019050919050565b5f6020820190508181035f830152611238816111ff565b905091905056fea26469706673582212201d35366bfa4fa8b350f189a7463eae1d0178da5ae3c024061d8db38d49a3952364736f6c634300081a0033",
        )?;

        let deploy_result = exec.deploy(
            minter, // Deploy from the minter address
            bytecode,
            U256::ZERO, // No ETH value
            None,       // No revert decoder
        )?;

        let contract_address = deploy_result.address;

        Ok((simulator, contract_address))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_successful_transfer_returns_empty() -> Result<(), eyre::Error> {
        let (mut simulator, contract_address) = setup_local_erc20_test().await?;

        let sender = Address::from_str("0x1000000000000000000000000000000000000001").unwrap();
        let recipient = Address::from_str("0x2000000000000000000000000000000000000002").unwrap();
        let minter = Address::repeat_byte(4);
        let amount = U256::from(100);

        // Mint tokens to sender
        mint_tokens(
            &mut simulator,
            contract_address,
            minter,
            sender,
            amount * U256::from(2),
        )
        .await?;

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
        assert!(
            result.is_empty(),
            "Successful transfer should return no missing assets"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_insufficient_balance_detected() -> Result<(), eyre::Error> {
        let (mut simulator, contract_address) = setup_local_erc20_test().await?;

        let sender = Address::from_str("0x1000000000000000000000000000000000000001").unwrap();
        let recipient = Address::from_str("0x2000000000000000000000000000000000000002").unwrap();
        let amount = U256::from(100);

        // Test transfer with insufficient balance (sender has 0 tokens in this fresh simulator)
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

        // Use no_fix version to just detect without fixing
        let result = simulator.check_transaction(transfer_call).await?;

        assert!(!result.is_empty(), "Should detect missing balance");

        if let Some(asset) = result.first() {
            assert_eq!(asset.account, sender);
            assert_eq!(asset.missing_amount, amount);
            assert_eq!(asset.current_balance, U256::ZERO);

            assert_eq!(
                asset.required,
                AssetSpec::ERC20 {
                    token: contract_address,
                    amount
                }
            );
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_allowance_issue_not_balance_issue() -> Result<(), eyre::Error> {
        let (mut simulator, contract_address) = setup_local_erc20_test().await?;

        let sender = Address::from_str("0x1000000000000000000000000000000000000001").unwrap();
        let recipient = Address::from_str("0x2000000000000000000000000000000000000002").unwrap();
        let spender = Address::from_str("0x3000000000000000000000000000000000000003").unwrap();
        let minter = Address::repeat_byte(4);
        let amount = U256::from(100);

        // Mint tokens to sender (so balance is sufficient)
        mint_tokens(
            &mut simulator,
            contract_address,
            minter,
            sender,
            amount * U256::from(2),
        )
        .await?;

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
        assert!(
            result.is_empty(),
            "Should not detect balance issues when problem is allowance"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_empty_transaction() -> Result<(), eyre::Error> {
        let mut simulator = AssetSimulator::builder()
            .with_fork("https://mainnet.base.org", Some(30155463))
            .with_erc20_checker()
            .build()
            .await?;

        let call = Call::default();
        let result = simulator.check_transaction(call).await?;
        assert!(result.is_empty());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_usdc_proxy_on_base() -> Result<(), eyre::Error> {
        let mut simulator = AssetSimulator::builder()
            .with_fork("https://mainnet.base.org", None)
            .with_erc20_checker()
            .build()
            .await?;

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

        // Use no_fix version to just detect without auto-fixing
        let result = simulator.check_transaction(call).await?;

        // We expect to find a missing asset since our test address likely doesn't have 1000 USDC
        assert!(!result.is_empty());
        if let Some(asset) = result.first() {
            assert_eq!(asset.account, sender);
            assert_eq!(asset.missing_amount, amount);
            assert_eq!(asset.current_balance, U256::ZERO);

            assert_eq!(asset.required, AssetSpec::ERC20 { token, amount });
        }
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    //  NEW TEST: multiple missing assets discovered in a single Multicall
    // ─────────────────────────────────────────────────────────────────────────
    #[tokio::test(flavor = "multi_thread")]
    async fn test_multicall_detects_two_missing_assets() -> Result<(), eyre::Error> {
        // Build simulator forked from Base main-net
        let mut simulator = AssetSimulator::builder()
            .with_fork("https://mainnet.base.org", None)
            .with_erc20_checker()
            .build()
            .await?;

        // Basic actors & token addresses
        let sender = Address::new([1u8; 20]);
        let recipient = Address::new([2u8; 20]);

        // Base-USDC & canonical WETH
        let usdc = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")?; // USDC
        let weth = Address::from_str("0x4200000000000000000000000000000000000006")?; // WETH

        // Pick absurdly large amounts so Multicall-3 certainly **cannot** cover them
        // and the transfers revert.
        // 1 e23 USDC (= 100 000 000 000 000 000 000 000, still << 2²⁵⁶-1)
        // 1 e24 wei  (= 1 000 000 WETH)
        let amount_usdc = U256::from_str("100000000000000000000000")?; // 1e23
        let amount_weth = U256::from_str("1000000000000000000000000")?; // 1e24 wei (≈1 000 000 WETH)

        // ---------------------------------------------------------------------
        //  Build inner ERC-20 transfer calldatas
        // ---------------------------------------------------------------------
        let usdc_transfer_calldata = IERC20::transferCall {
            to: AAddress::from_slice(recipient.as_slice()),
            amount: amount_usdc,
        }
        .abi_encode();

        let weth_transfer_calldata = IERC20::transferCall {
            to: AAddress::from_slice(recipient.as_slice()),
            amount: amount_weth,
        }
        .abi_encode();

        // ---------------------------------------------------------------------
        //  Encode Multicall-3.aggregate
        // ---------------------------------------------------------------------
        let multicall_payload = Multicall3::aggregateCall {
            calls: vec![
                TargetCall {
                    target: AAddress::from_slice(usdc.as_slice()),
                    callData: usdc_transfer_calldata.into(),
                },
                TargetCall {
                    target: AAddress::from_slice(weth.as_slice()),
                    callData: weth_transfer_calldata.into(),
                },
            ],
        }
        .abi_encode();

        // Deployed Multicall-3 address on Base
        let multicall_addr = Address::from_str("0xca11bde05977b3631167028862be2a173976ca11")?;

        let call = Call::new(sender, multicall_addr, multicall_payload, U256::ZERO);

        // Run the check - uses auto-fix, so it will iterate, patch storage and
        // finally return every missing asset it encountered.
        let missing = simulator.check_transaction(call).await?;

        assert_eq!(
            missing.len(),
            2,
            "should report two distinct missing assets"
        );

        // Helper map (token → amount) to make assertions easier
        let mut by_token = std::collections::HashMap::new();
        for m in &missing {
            if let AssetSpec::ERC20 { token, amount } = m.required {
                by_token.insert(token, amount);
            }
        }

        assert_eq!(by_token.get(&usdc), Some(&amount_usdc));
        assert_eq!(by_token.get(&weth), Some(&amount_weth));

        Ok(())
    }
}
