fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .bytes(["."])
        .compile_protos(&["src/proto/agent.proto"], &["src/proto/"])?;
    Ok(())
}
