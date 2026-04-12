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
    @stat -c %s ./target/release/chacc | numfmt --to=iec

fmt:
    cargo +nightly fmt

lint:
    cargo clippy --fix --allow-dirty --allow-staged -- -D warnings

test *flags:
    cargo nextest run {{ flags }}

doc *flags:
    cargo +nightly doc --no-deps --document-private-items -Z rustdoc-map {{ flags }}

ci: fmt lint test

all: ci doc build

compile code:
    #!/usr/bin/env bash
    set -eou pipefail

    ID=$(shuf -i 10000-99999 -n 1)
    ASM_FILE="tmp_${ID}.s"
    BIN_FILE="tmp_${ID}"

    printf '%s' {{ quote(code) }} | cargo run --quiet -- -o "${ASM_FILE}" -
    cc -x assembler -o "${BIN_FILE}" "${ASM_FILE}"

    set +e
    "./${BIN_FILE}"
    echo $?
    set -e

    echo "ASM: ${ASM_FILE}"
    echo "BIN: ${BIN_FILE}"
