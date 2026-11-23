//! ViceModVI's readers and writers for the native Xbox Series formats:
//! DDS, RPF9 texture dictionaries and drawables, TTPF texture pools and
//! Wavefront OBJ.
//!
//! Everything here treats its input as untrusted. Reads are bounds-checked,
//! sizes are capped before allocation, and malformed data is reported as an
//! [`Error`](crate::Error) or, for probes, as "not this format".

mod bin;
pub mod dds;
pub mod dictionary;
pub mod drawable;
pub mod dxgi;
pub mod model;
pub mod obj;
pub mod pixels;
pub mod pool;
pub mod pool_copy;
pub mod scan;
pub mod slots;
mod swizzle;

pub use dds::DdsInfo;
pub use model::{Material, Mesh, Model, TextureRef};
pub use pixels::Pixels;
