#################
# Demo Commands #
#################

status:
    RUST_LOG=info ./target/release/simple_covenant_vault status

deposit:
    RUST_LOG=info ./target/release/simple_covenant_vault deposit

trigger:
    RUST_LOG=info ./target/release/simple_covenant_vault trigger

steal:
    RUST_LOG=info ./target/release/simple_covenant_vault steal

cancel:
    RUST_LOG=info ./target/release/simple_covenant_vault cancel

complete:
    RUST_LOG=info ./target/release/simple_covenant_vault complete

switch:
    RUST_LOG=info ./target/release/simple_covenant_vault switch

delete: 
    rm ./vault_covenant.json

###################################
# Build and boostrapping commands #
###################################

bitcoin_datadir := "./bitcoin-data"
bcli := "../../bitcoin-inquisition/bitcoin/src/bitcoin-cli -regtest -rpcuser=user -rpcpassword=password"

start-bitcoind *ARGS:
    mkdir -p {{ bitcoin_datadir }}
    ../../bitcoin-inquisition/bitcoin/src/bitcoind -regtest -timeout=15000 -server=1 -txindex=1 -rpcuser=user -rpcpassword=password -minrelaytxfee=0 {{ ARGS }}

stop-bitcoind:
    {{ bcli }} stop

clean-bitcoin-data:
    rm -rf {{ bitcoin_datadir }}

build:
    cargo build --release

bootstrap:
    #bash ./scripts/build_bitcoincore.sh
    just build
    #just clean-bitcoin-data
    #just start-bitcoind

cli *ARGS:
    {{ bcli }} {{ ARGS }}

