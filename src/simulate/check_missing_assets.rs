// use alloy_primitives::{Address, Bytes, U256};
use forge::{
    backend::Backend, executors::ExecutorBuilder, revm::{interpreter::InstructionResult, primitives::{Address, Bytes, Env, U256}}, traces::{CallKind, CallTrace, CallTraceDecoder, CallTraceDecoderBuilder, CallTraceNode, SparsedTraceArena, TraceMode}
};
use foundry_evm_core::opts::EvmOpts;
use foundry_config::Config;
use alloy_sol_types::{sol, SolCall};

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
pub struct MissingERC20AssetInfo {
    pub token_address: Address,
    pub account: Address,
    pub amount: U256,
}

sol! {
  function transfer(address to, uint256 amount) public returns (bool);
  function transferFrom(address from, address to, uint256 amount) public returns (bool);
}

// Add this trait to abstract the common functionality
trait ERC20CallChecker {
    fn check_call(trace: &CallTrace) -> Option<MissingERC20AssetInfo>;
}

// Implement for transfer calls
struct TransferChecker;
impl ERC20CallChecker for TransferChecker {
    fn check_call(trace: &CallTrace) -> Option<MissingERC20AssetInfo> {
        let decode_result = transferCall::abi_decode(&trace.data);
        decode_result.ok().map(|decoded| MissingERC20AssetInfo {
            token_address: trace.address,
            account: trace.caller,
            amount: decoded.amount,
        })
    }
}

// Implement for transferFrom calls
struct TransferFromChecker;
impl ERC20CallChecker for TransferFromChecker {
    fn check_call(trace: &CallTrace) -> Option<MissingERC20AssetInfo> {
        let decode_result = transferFromCall::abi_decode(&trace.data);
        decode_result.ok().map(|decoded| MissingERC20AssetInfo {
            token_address: trace.address,
            account: Address::from_slice(decoded.from.as_slice()),
            amount: decoded.amount,
        })
    }
}

pub async fn check_missing_assets(
    call: Call,
    fork_info: ForkInfo,
) -> Result<Vec<MissingERC20AssetInfo>, eyre::Error> {
    let opts = EvmOpts {
        fork_url: fork_info.rpc_url,
        fork_block_number: fork_info.block_number,
        ..Default::default()
    };

    let env = Env {
        ..Default::default()
    };

    let backend = Backend::spawn(opts.get_fork(&Config::default(), opts.evm_env().await?))?;
    let mut executor = ExecutorBuilder::new()
        .inspectors(|stack| stack.trace_mode(TraceMode::Call))
        .build(env, backend);
    
    let result = executor.transact_raw(call.from, call.to, call.data, call.value)?;
    // println!("result: {:?}", result);

    if result.exit_reason.is_revert() {
      //  // find last call before the revert 
      //  // decode against erc20 calls: transfer, transferFrom, transfer with authorization (3009), permit transfer
       if let Some(trace_arena) = result.traces {
        let traces: Vec<&CallTrace> = trace_arena.nodes().iter().map(|trace_node| { &trace_node.trace }).collect();
        let trace = find_last_non_proxy_call(&traces)?;

        // Try all checkers in sequence
        let checkers: [&dyn Fn(&CallTrace) -> Option<MissingERC20AssetInfo>; 2] = [
            &TransferChecker::check_call,
            &TransferFromChecker::check_call,
        ];
        
        for checker in checkers {
            if let Some(missing_asset) = checker(trace) {
                return Ok(vec![missing_asset]);
            }
        }
       }
      //   let missing_erc20_assets = traces.nodes().into_iter().rev().map(|trace_node| {
      //     let decode_result = transferCall::abi_decode(&trace_node.trace.data.as_ref());
      //     if decode_result.is_ok() {
      //       let missing = MissingERC20AssetInfo {
      //         token_address: trace_node.trace.address,
      //         total_amount: decode_result.unwrap().amount,
      //         amount_needed: // TODO: fetch balanceOf caller, note this is specific to transfer, transferFrom would take argument from call. Would be good to decompose  
      //       }
      //      // TODO prior calls for delegated calls. If prior calls matches calldata
      //      // keep going until you find defering calldata 
      //     }
      //   }
      //  }
    }
    
    
    Ok(vec![])
}

fn find_last_non_proxy_call<'a>(traces: &'a [&'a CallTrace]) -> Result<&'a CallTrace, eyre::Error> {
  if traces.is_empty() {
      return Err(eyre::eyre!("No traces provided"));
  }
  
  let mut cur_index = traces.len() - 1;
  let mut cur = traces[cur_index];
  
  while cur_index > 0 && cur.kind == CallKind::DelegateCall {
      let prior_call = traces[cur_index - 1];
      if cur.data == prior_call.data {
          cur_index -= 1;
          cur = prior_call;
      } else {
          break;
      }
  }
  
  Ok(cur)
}


#[cfg(test)]
mod tests {
    use super::{Call, ForkInfo, check_missing_assets, transferCall};
    use alloy_sol_types::SolCall;
    use forge::revm::primitives::{hex::FromHex, Address, Bytes, U256};
    use alloy_primitives::{Address as AAddress};

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
                // fetch latest block number
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
