#!/bin/bash
sleep 30
date "+%s"
RUST_BACKTRACE=1 ./target/debug/nts nts-ke tests/nts-ke-config.yaml &
RUST_BACKTRACE=1 ./target/debug/nts ntp tests/ntp-config.yaml
