fn main() {
    prost_build::compile_protos(&["proto/notification.proto"], &["proto/"]).unwrap();
}
