set windows-shell := ["powershell"]
set shell := ["bash", "-cu"]

alias t := test

_default:
    just --list -u

fmt:
    cargo +nightly fmt

lint:
    cargo clippy --fix --allow-dirty --allow-staged -- -D warnings

doc *flags:
    cargo +nightly doc --no-deps --document-private-items -Z rustdoc-map {{ flags }}

test:
    cargo test --test e2e
