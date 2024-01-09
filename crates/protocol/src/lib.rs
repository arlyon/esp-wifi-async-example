#![cfg_attr(not(feature = "std"), no_std)]

mod proto_capnp {
    core::include!(core::concat!(core::env!("OUT_DIR"), "/proto_capnp.rs"));
}

pub use proto_capnp::*;
