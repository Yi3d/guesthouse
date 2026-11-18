//! Work on the card through the kernel's xe driver.
//!
//! [`abi`] is the calls as bytes and [`batch`] the few instructions written here. Nothing in the
//! backend uses them yet.

pub mod abi;
pub mod batch;
