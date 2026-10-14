//! Store an artifact once and restore it into two worktrees.
//!
//! This is the core move every tinymold cache hit makes. Run it with:
//!
//! ```sh
//! cargo run -p tinymold --example basic
//! ```

use tinymold::tinymold_cas::{Store, Strategy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("tinymold-example-{}", std::process::id()));
    let store = Store::open(dir.join("store"))?;

    let digest = store.insert_bytes(b"pretend this is a 600 MB rlib")?;
    println!("stored     {digest}");

    for worktree in ["worktree-a", "worktree-b"] {
        let destination = dir.join(worktree).join("target/debug/deps/libdep.rlib");
        let method = store.materialize(&digest, &destination, Strategy::Auto)?;
        println!("{method:<10} {}", destination.display());
    }

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
