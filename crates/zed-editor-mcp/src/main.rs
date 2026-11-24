//! \u{201c}One tick late\u{201d} thin wrapper: argument handling lives in the library so it can be tested.
//! Run `zed-editor-mcp --help` or set `ZED_EDITOR_MCP_SOCKET`.

fn main() {
    let socket_path = match zed_editor_mcp::parse_args(std::env::args()) {
        Ok(path) => path,
        Err(usage) => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };
    if let Err(error) = zed_editor_mcp::connect_and_run(&socket_path) {
        eprintln!(
            "zed-editor-mcp: {error}. Is Zed running with \
             `agent.editor_mcp_server.enabled = true` for this workspace? (socket: {})",
            socket_path.display()
        );
        std::process::exit(1);
    }
}
