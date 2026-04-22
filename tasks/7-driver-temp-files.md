# Deferred Area: Driver Temp File Naming

Introduced in: `8fb7d2189773f69e09f3e089cdbdc4db0da0c1cf`

## Summary

The current driver temp-file behavior is acceptable for the current scope:

- one input file per invocation
- one intermediate assembly file per compile job

But the naming strategy is not robust enough for future:

- multiple-input driver support
- parallel compilation (`-j`)
- more driver phases with more than one intermediate file per job

## Current Status

- preserved temp files (`-save-temps`) use a deterministic name derived from
  the final output stem and input stem
- non-preserved temp files are placed under a unique `TempDir`
- within that temp dir, intermediate file naming is effectively flat

## Why This Is Fine For Now

With one translation unit per driver invocation, a unique temp directory is
already enough to avoid collisions in practice.

## Future Risk

If one driver invocation later handles multiple compile jobs, flat temp naming
can collide for inputs or outputs with the same stem.

Examples:

- `src/foo.c` and `tests/foo.c`
- multiple jobs using the same `-o` stem

## Future Plan

When multi-input or parallel driver work lands, move to one temp subdirectory
per compile job instead of relying on flat temp names.

That is the cleanest way to avoid collisions without encoding too much path
structure into file names.
