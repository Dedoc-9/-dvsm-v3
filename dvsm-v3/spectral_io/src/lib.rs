// spectral_io/src/lib.rs  |  Spectral I/O crate root
// Re-exports for engine integration.
#![no_std]

pub mod entropy;
pub mod markov_salience;
pub mod prefetch_governor;

pub use entropy::{SpectralEntropyState, ModeClassifier, GhostClass,
                  DIM, H_MAX, DIVERGENCE_THRESHOLD};
pub use markov_salience::{MarkovSalience, BUCKET_COUNT, SALIENCE_THRESHOLD};
pub use prefetch_governor::{
    SpectralIOGovernor, GovFrameResult,
    PrefetchJob, PrefetchQueue, MipHintBuffer, CompressionFormat,
};
