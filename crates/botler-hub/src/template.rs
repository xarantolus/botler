//! The monorepo template, compiled into the binary so `botler init` needs nothing else.

use std::path::Path;

use include_dir::{Dir, include_dir};

static TEMPLATE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../template");

/// Write the template into `dest` (which must not exist yet).
pub fn write_to(dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    TEMPLATE.extract(dest)?;
    // include_dir drops permissions; the gate script must stay executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let gate = dest.join("gate/run");
        if gate.exists() {
            std::fs::set_permissions(gate, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}
