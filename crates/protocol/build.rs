fn main() {
    capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/proto.capnp")
        .run()
        .expect("schema compiler command");
}
