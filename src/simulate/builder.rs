use crate::simulate::{
    checkers::{AssetChecker, ERC20Checker},
    types::ForkInfo,
};
use forge::{
    backend::Backend, executors::ExecutorBuilder, revm::primitives::Env, traces::TraceMode,
};
use foundry_config::Config;
use foundry_evm_core::opts::EvmOpts;

#[derive(Default)]
pub struct AssetSimulatorBuilder {
    env: Env,
    fork_info: Option<ForkInfo>,
    backend: Option<Backend>,
    checkers: Vec<Box<dyn AssetChecker>>,
}

impl AssetSimulatorBuilder {
    pub fn with_fork(mut self, rpc_url: impl Into<String>, block_number: Option<u64>) -> Self {
        self.fork_info = Some(ForkInfo {
            rpc_url: Some(rpc_url.into()),
            block_number,
        });
        self
    }

    pub fn with_env(mut self, env: Env) -> Self {
        self.env = env;
        self
    }

    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn with_erc20_checker(self) -> Self {
        self.with_checker(ERC20Checker::new())
    }

    pub fn with_checker<T: AssetChecker + 'static>(mut self, checker: T) -> Self {
        self.checkers.push(Box::new(checker));
        self
    }

    /// Build a fully-initialised `AssetSimulator`.
    pub async fn build(
        self,
    ) -> Result<crate::simulate::asset_simulator::AssetSimulator, eyre::Error> {
        // ── select / build backend ────────────────────────────────────────────────
        let backend = if let Some(backend) = self.backend {
            backend
        } else {
            let opts = if let Some(fork) = &self.fork_info {
                EvmOpts {
                    fork_url: fork.rpc_url.clone(),
                    fork_block_number: fork.block_number,
                    ..Default::default()
                }
            } else {
                EvmOpts::default()
            };

            let cfg = Config::default();
            let backend_env = opts.evm_env().await?;
            Backend::spawn(opts.get_fork(&cfg, backend_env))?
        };

        // ── executor ─────────────────────────────────────────────────────────────
        let executor = ExecutorBuilder::new()
            .inspectors(|stack| stack.trace_mode(TraceMode::Debug))
            .build(self.env.clone(), backend);

        Ok(
            crate::simulate::asset_simulator::AssetSimulator::new_from_parts(
                executor,
                self.checkers,
            ),
        )
    }
}
