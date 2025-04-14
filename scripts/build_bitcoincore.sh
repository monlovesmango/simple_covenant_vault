#!/usr/bin/env bash

set -e


echo "Building a copy of Bitcoin Core with covenants active..."

git clone --depth 1 --branch v28.0-inq git@github.com:bitcoin-inquisition/bitcoin.git bitcoin-core-inq || true

pushd bitcoin-core-inq
./autogen.sh
./configure --without-tests --disable-bench
make -j10
popd
