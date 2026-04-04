set windows-shell := ["powershell"]
set shell := ["bash", "-cu"]

alias b := build
alias f := fmt
alias l := lint
alias t := test
alias d := doc

_default:
    just --list -u

build:
    cargo build --release

fmt:
    cargo +nightly fmt

lint:
    cargo clippy --fix --allow-dirty --allow-staged -- -D warnings

test *flags:
    cargo nextest run {{ flags }}

doc *flags:
    cargo +nightly doc --no-deps --document-private-items -Z rustdoc-map {{ flags }}

ci: fmt lint test

compile code:
    printf "{{ code }}" | cargo run --quiet -- - && cc -x assembler -o tmp a.out && ./tmp; echo $?
