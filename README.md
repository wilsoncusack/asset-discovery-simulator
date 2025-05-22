This is a rough start on a tool to simulate EVM transactions and discover 
what assets are needed, in what amount, and for which accounts, in order for the transaction to succeed.

### TODO
- [x] Handle proxied calls
- [ ] More test coverage: 
  - [ ] When revert is from allowance not token amount
  - [ ] When asset transfer is not top level call (e.g. Uniswap swap)
- [ ] Test against purely local backend rather than always using fork state
- [ ] Add checkers for all possible ERC-20 calls 
  - [x] transferFrom
  - [ ] transfer
  - [ ] EIP-3009 transferWithAuthorization
  - [ ] ERC-2612 permit
- [ ] Add checkers for different asset types.
  - [ ] ERC-721
  - [ ] ERC-1155
- [ ] Add a `deal` function to the AssetChecker trait which allows simulating the account has the required assets, in order to discover other assets that are needed. [Prior art](https://github.com/foundry-rs/forge-std/pull/505).
