//! ViceModVI core: source catalogs, fingerprint checks, texture conversion,
//! texture links and the experimental TTPF and RPF9 copy writers.

pub mod cache;
pub mod catalog;
mod drawable_materials;
pub mod error;
pub mod files;
pub mod manifests;
pub mod pool_writer;
pub mod project;
mod resample;
pub mod resource_models;
pub mod rpf_copy;
pub mod texture_conversion;
pub mod texture_links;
mod time;
pub mod util;
pub mod xbox_assets;
mod zlib;

pub use catalog::{Asset, ByteRange, Catalog, Fingerprint, ReadMeasurement, Source};
pub use error::{Error, Result};
pub use rpf9_explorer::{
    dds::{self, DdsTextureInfo},
    model_texture_bindings,
    obj_reader,
    rage_model::{self, RageMaterial, RageMesh, RageModel},
    ttpf as pool,
    Cancel, RageTexturePixels,
};
pub use xbox_assets::IndexProgress;

/// Revision of the rpf9-explorer Core reader that the port follows.
pub const EXPLORER_REVISION: &str = "48557ead00601d3757ba12b894f2595c4faad2a5";
