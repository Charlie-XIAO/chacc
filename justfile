set windows-shell := ["powershell"]
set shell := ["bash", "-cu"]

_default:
    just --list -u

fmt:
    cargo +nightly fmt

lint:
    cargo clippy --fix --allow-dirty --allow-staged -- -D warnings

alias t := test
alias tu := test-unit
alias ti := test-integration

test: test-unit test-integration

test-unit:
    cargo test --lib

test-integration:
    ./test.sh
