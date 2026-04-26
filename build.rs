fn main() {
    // Compiles the Event Bus envelope + payload types so the daemon
    // can decode `focus.*` and `window.fullscreen_*` events.
    //
    // TODO: Migrate to shared lunaris-proto crate. Currently duplicated from event-bus/proto/event.proto.
    // When more services need protos, create a central crate that all can depend on.
    prost_build::compile_protos(&["proto/event.proto"], &["proto/"]).unwrap();
}
