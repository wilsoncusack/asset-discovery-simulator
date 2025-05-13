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

pub struct MissingERC20AssetInfo {
    pub token_address: Address,
    pub total_amount: U256,
    pub amount_needed: U256,
}

sol! {
  function transfer(address to, uint256 amount) public returns (bool);
  function transferFrom(address from, address to, uint256 amount) public returns (bool);
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
       // find last call before the revert 
       // decode against erc20 calls: transfer, transferFrom, transfer with authorization (3009), permit transfer
       if let Some(traces) = result.traces {
        let missing_erc20_assets = traces.nodes().into_iter().rev().map(|trace_node| {
          let decode_result = transferCall::abi_decode(&trace_node.trace.data.as_ref());
          if decode_result.is_ok() {
            let missing = MissingERC20AssetInfo {
              token_address: trace_node.trace.address,
              total_amount: decode_result.unwrap().amount,
              amount_needed: // TODO: fetch balanceOf caller, note this is specific to transfer, transferFrom would take argument from call. Would be good to decompose  
            }
           // TODO prior calls for delegated calls. If prior calls matches calldata
           // keep going until you find defering calldata 
          }
        }
       }
    }
    
    
    Ok(vec![])
}

fn check_ERC20_caused_reverts(traces: [SparsedTraceArena]) -> Result<Vec<MissingERC20AssetInfo>, eyre::Error> {
  let trace = &find_last_non_proxy_call(&trace_arena.nodes())?.trace;
  let caller = trace.caller;
  // get balanceOf Caller
  Ok(vec![])
}

fn check_transfer(trace: CallTrace) -> MissingERC20AssetInfo {
  let missing = MissingERC20AssetInfo {
    token_address: trace.address,
    total_amount: U256::ZERO,
    amount_needed: U256::ZERO,
  };

  missing
}

fn find_last_non_proxy_call(nodes: &[CallTraceNode]) -> Result<&CallTraceNode, eyre::Error> {
  let len = nodes.len();
  let mut cur_index = len - 1;
  let mut cur = &nodes[cur_index];
  let mut is_checked = false;

  while !is_checked {
    if cur.trace.kind == CallKind::DelegateCall {
      if cur.trace.data == nodes[cur_index - 1].trace.data {
        cur_index -= 1;
        cur = &nodes[cur_index];
      } else {
        is_checked = true;
      }
    }
  }

  Ok(cur)
}


#[cfg(test)]
mod tests {
    use crate::simulate::check_missing_assets::transferCall;

    use super::{Call, ForkInfo, check_missing_assets};
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
      assert!(result.is_empty());
    }
}
