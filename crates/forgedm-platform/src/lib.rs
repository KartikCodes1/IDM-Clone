//! ForgeDM Win32 platform primitives. Only this crate may contain unsafe FFI.
//!
//! Unsafe is denied by the workspace lints; FFI modules opt in locally with a justification.

pub mod dpapi;
pub mod error;
pub mod fs;
pub mod instance_lock;
pub mod known_folders;
pub mod motw;
pub mod peer;
pub mod pipe;
pub mod sid;
pub mod wide;
