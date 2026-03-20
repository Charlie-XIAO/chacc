#!/usr/bin/env bash
set -euo pipefail

assert() {
  expected="$1"
  input="$2"

  cargo run --quiet -- "$input" > tmp.s
  cc -o tmp tmp.s
  set +e
  ./tmp
  actual="$?"
  set -e

  if [ "$actual" = "$expected" ]; then
    echo "$input => $actual"
  else
    echo "$input => $expected expected, but got $actual"
    exit 1
  fi
}

assert 0 0
assert 42 42
assert 21 '5+20-4'

echo OK
