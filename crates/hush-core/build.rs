use std::path::Path;

fn main() {
    let protoc = Path::new("../../.tools/protoc-dist/bin/protoc");
    if protoc.exists() {
        std::env::set_var("PROTOC", protoc.canonicalize().unwrap());
    }
}
