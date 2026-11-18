//! Work on the card through the kernel's xe driver.
//!
//! [`abi`] is the calls as bytes, [`batch`] the few instructions written here and [`plan`] the
//! decisions that need no card. Nothing in the
//! backend uses them yet.

pub mod abi;
pub mod batch;
pub mod plan;
