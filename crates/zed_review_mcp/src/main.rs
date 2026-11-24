use anyhow::Result;
use std::path::PathBuf;
use zed_review_mcp::ReviewStore;

fn main() -> Result<()> {
    let working_directory = std::env::current_dir()?;
    let trusted_root = smol::block_on(zed_review_mcp::trusted_root::resolve_trusted_root(
        &working_directory,
    ))?;
    let session_directory = default_session_directory();
    let review_store = ReviewStore::start(&trusted_root, &session_directory)?;
    println!(
        "zed-review-mcp: review session ready\n  trusted root: {}\n  session directory: {}",
        review_store.trusted_root().display(),
        review_store.session_dir().display(),
    );
    Ok(())
}

fn default_session_directory() -> PathBuf {
    std::env::temp_dir().join(format!("zed-review-mcp-{}", std::process::id()))
}
