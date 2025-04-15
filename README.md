# A Simple Bitcoin Vault with OP_CTV or OP_CAT

## What?

This repo contains a demo of a (regtest) Bitcoin Vault that allows for a multi-step withdrawal process to be validated onchain. The basic pattern for this vault is comprised of a total of 3 states:
- Inactive: the funds are safely stored in the vault and the withdrawal process as not been initiated
- Triggered: the funds are in the intermediary unvaulting address and can only be spent in 2 ways:
  1. cancel the withdrawal
  2. wait X blocks and then withdraw the funds
- Complete: the funds have been withdrawn from the vault

For the purposes of this demo cancelling a withdrawal will revault the coins, however in the real world it is more likely that cancel would send the funds to some sort of cold storage.

This vault pattern has been implemented with both OP_CTV and OP_CAT and you can choose which implementation to use when running the demo. The goal of this demo was to wrap my head around what a OP_CAT vault looks like next to an OP_CTV vault.

## Special Thanks

This repo is basically copy pasta made possible by:
- [rot13maxi](https://github.com/rot13maxi)'s [purrfect_vault](https://github.com/taproot-wizards/purrfect_vault) - from which I forked the framework for this demo and the OP_CAT vault implementation
- [jamesob](https://github.com/jamesob)'s [simple-ctv-vault](https://github.com/jamesob/simple-ctv-vault) - as a reference for implementing the OP_CTV vault.
- [ajtowns](https://github.com/ajtowns)'s [bitcoin-inquisition](https://github.com/bitcoin-inquisition/bitcoin) - which is a fork of bitcoin core that has OP_CAT and OP_CTV activated, this is used to run this vault on a chain that has these op codes live.
- [stutxo](https://github.com/stutxo)'s [simple_ctv](https://github.com/stutxo/simple_ctv) and [ursuscamp](https://github.com/ursuscamp)'s [ctvlib](https://github.com/ursuscamp/ctvlib) - as references for implementing OP_CTV in Rust

## Vault Contract Construction

For explanation of OP_CAT vault construction, please reference [purrfect_vault](https://github.com/taproot-wizards/purrfect_vault).

For explanation of OP_CTV vault construction, please reference [simple-ctv-vault](https://github.com/jamesob/simple-ctv-vault). A major difference between [simple-ctv-vault](https://github.com/jamesob/simple-ctv-vault) and this vault is that the former cancels the withdrawal by way of another OP_CTV which only spends to a cold wallet, whereas this vault cancels the withdrawal by sending funds back to the vault.

## How to run it

You will need to be able to build bitcoin-core. Go get set up with a C++ compiler for your platform. Those directions are outside the scope of this document.

From there, there are some scripts and helpers in this project to build a copy of bitcoin-core that has OP_CAT and OP_CTV enabled, and then you can use [Just](https://github.com/casey/just) as a command runner to build and run the vault demo.

If you have a rust toolchain installed, and don't want to use `just`, you can also just poke around yourself. Choose your own adventure!

### Have nothing installed?
This project can use [Hermit](https://cashapp.github.io/hermit/) to provide a copy of [Just](https://github.com/casey/just) and the rust toolchain. So you don't have to install anything.

To activate the hermit environment, run `source bin/activate-hermit` and it will set up a shell environment with the tools you need. 

Run `just bootstrap` to checkout and build a copy of bitcoin-core with OP_CAT and OP_CTV enabled. It will be placed in a directory called `bitcoin-core-inq` in the root of the project.
It will also build the `simple_covenant_vault` binary, which is the demo.

Proceed to the "Running the demo" section.

### Have `just` and `cargo` already installed?
You're all set!

Run `just bootstrap` to checkout and build a copy of bitcoin-core with OP_CAT and OP_CTV enabled. It will be placed in a directory called `bitcoin-core-inq` in the root of the project.
It will also build the `simple_covenant_vault` binary, which is the demo. 

Proceed to the "Running the demo" section.

### Running the demo

Follow these steps to create a vault that is configured to allow a withdrawal after 20 blocks. You will try to "steal" from this vault, see that a theft is in-progress and foil the theft. Then you will trigger a new withdrawal and complete it.

These steps use `just` as a command wrapper around the `simple_covenant_vault` binary to set the log level. If you don't want to use `just`, you can run the `simple_covenant_vault` binary directly from the `target/release/` directory with the same arguments, or pass `-h` to see options.

1. Run a covenant enabled bitcoind in regtest mode. This will be done either using `just bootstrap`, or you can run it with `just start-bitcoind`, or run it yourself with the `bitcoin-core-inq` binary that was built. If you already have an existing covenant enabled bitcoind binary that you would like to use instead, update the `bitcoin_src` variable in the `justfile` file to point to the location of of your existing bitcoind (and bitcoin-cli) binary and run `just start-bitcoind`.
2. Run `just switch` to choose between the OP_CTV vault (the default) and the OP_CAT vault.
3. Start by running `just deposit`. This will create a miner wallet, mine some coins, and then create a new vault and deposit some coins into it.
4. Run `just status` to see the status of the vault.
5. Try to steal from the vault with `just steal`. This will generate an address from the miner wallet and initiate a withdrawal to it. Alternatively you can execute the `simple_covenant_vault` binary with the `steal` subcommand and pass an address of your choosing. It will also mine a block to confirm the transaction
6. Run `just status` to see the status of the vault and see that the on-chain state of the vault is that it's in a withdrawal-triggered state, but that the internal state of the wallet is that no withdrawal is in-progress, so it looks like a theft is happening! oh no!
7. Foil the theft with `just cancel`. This will send the coins back to the vault and mine a block to confirm the transaction.
8. Initiate a withdrawal from the vault with `just trigger`. This will generate an address from the miner wallet and initiate a withdrawal to it. Alternatively you can execute the `simple_covenant_vault` binary with the `trigger` subcommand and pass an address of your choosing. It will also mine a block to confirm the transaction
9. Run `just status` to see the status of the vault and see that the vault is in the Triggered state.
10. Complete the withdrawal with `just complete`. This will mine 20 blocks to satisfy the timelock, send the withdrawal-completion transaction, and then a block to confirm the transaction.
11. To re-run the demo use `just delete` to delete the existing vault.

To access the bitcoin-cli you can use `just bcli`.

## Comparisons Between Implementations

| Tx Type  | CTV                                 | CAT                                   |
|----------|-------------------------------------|---------------------------------------|
| Deposit  | `size`:205 `vsize`:154 `weight`:616 | `size`:205 `vsize`:154 `weight`:616   |
| Trigger  | `size`:273 `vsize`:170 `weight`:678 | `size`:892 `vsize`:357 `weight`:1426  |
| Cancel   | `size`:371 `vsize`:194 `weight`:776 | `size`:831 `vsize`:309 `weight`:1236  |
| Complete | `size`:375 `vsize`:195 `weight`:780 | `size`:1013 `vsize`:355 `weight`:1418 |
