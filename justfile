set windows-shell := ["powershell"]
set shell := ["bash", "-cu"]

alias f := fmt
alias l := lint
alias t := test
alias d := doc

_default:
    just --list -u

fmt:
    cargo +nightly fmt

lint:
    cargo clippy --fix --allow-dirty --allow-staged -- -D warnings

test *flags:
    cargo nextest run {{ flags }}

doc *flags:
    cargo +nightly doc --no-deps --document-private-items -Z rustdoc-map {{ flags }}

ci: fmt lint test
