//! Shared helpers for the `oxideav-id3-fuzz` targets.
//!
//! Currently empty — the `parse` target is self-contained because the
//! ID3 fuzz surface is decode-only (no encoder bootstrap, no oracle
//! cross-decode). The library exists so future targets that need a
//! shared corpus generator can grow here without re-wiring Cargo.
