//! Source files of the pi extension and Python SDK, compiled into the binary.
//! Paths are relative to their package roots.

pub const EXTENSION: &[(&str, &str)] = &[("index.ts", include_str!("../../../pi-extension/src/index.ts"))];

pub const SDK: &[(&str, &str)] = &[];
