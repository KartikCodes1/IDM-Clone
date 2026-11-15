//! Re-embed the migrations whenever a file under `migrations/` changes (`sqlx::migrate!`).

use std::io::Write;

fn main() -> std::io::Result<()> {
    writeln!(std::io::stdout(), "cargo:rerun-if-changed=migrations")
}
