# ADR 0002 — Toolchain and unsafe

## Context

Edition 2024 treats `no_mangle` as an unsafe attribute. `wasm32-unknown-unknown` must build. An MSRV would be a promise to users we do not have.

## Decision

Develop on Rust 1.98.1, edition 2024, as pinned in `rust-toolchain.toml`. Do not declare an MSRV yet. `moonseed` forbids `unsafe`. The probe has no unsafe blocks; each export uses `#[unsafe(no_mangle)]`.

## Alternatives

Stay on the previously installed 1.94 and edition 2021 to avoid the export attribute. Rejected: 1.98.1 installs here, and edition 2024 is the current default.

## Revisit

When a second compiler is tested, record that version as MSRV or explicitly refuse it. Revisit `unsafe` only for a measured hotspot with a test that fails if the safe version and the unsafe version disagree.
