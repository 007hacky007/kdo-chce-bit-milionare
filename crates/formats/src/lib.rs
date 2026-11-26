//! Decoders for the original game's file formats.

pub mod audiolib;
pub mod error;
pub mod questions;
pub mod smacker;
pub mod udf;
pub mod wav;

pub use error::{FormatError, Result};
