mod engine_ref;
mod request_local;
mod request_send;

pub(crate) use engine_ref::EngineRef;
pub use request_local::RequestBuilderLocal;
pub use request_send::RequestBuilderSend;

fn generate_websocket_key() -> String {
    use base64::Engine;
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut key = [0u8; 16];
    for chunk in key.chunks_exact_mut(8) {
        let val = RandomState::new().build_hasher().finish();
        chunk.copy_from_slice(&val.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(key)
}
