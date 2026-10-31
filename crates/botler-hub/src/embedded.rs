//! Source files of the pi extension and Python SDK, compiled into the binary.
//! Paths are relative to their package roots. `tests/sdk.rs` checks the SDK list is complete.

pub const EXTENSION: &[(&str, &str)] = &[("index.ts", include_str!("../../../pi-extension/src/index.ts"))];

macro_rules! sdk {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../python/botler-sdk/", $path)))),*]
    };
}

pub const SDK: &[(&str, &str)] = sdk![
    "pyproject.toml",
    "botler/__init__.py",
    "botler/_client.py",
    "botler/_discover.py",
    "botler/_ops.py",
    "botler/events.py",
    "botler/settings.py",
    "botler/tools.py",
    "botler/py.typed",
];
