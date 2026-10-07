// Run the real core codec gates independently while sibling core modules are
// being integrated. This uses Cargo's own target tree, not a substitute core so.
#[path = "../../core/src/voice_codec.rs"]
mod voice_codec;
