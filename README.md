This is a rough start on a tool to simulate EVM transactions and discover 
what assets are needed, in what amount, and for which accounts, in order for the transaction to succeed.

TODO
- [ ] More test coverage 
- [ ] Test against purely local backend rather than always using fork state
- [ ] Add more checkers for different asset types.
- [ ] Add a `deal` function to the AssetChecker trait which allows simulating the account has the required assets, in order to discover other assets that are needed. [Prior art](https://github.com/foundry-rs/forge-std/pull/505).
