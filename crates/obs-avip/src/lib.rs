//! NDI plugin core. No OBS types: the OBS shell (`ndi-obs`) reads settings and
//! calls into this crate, so the logic here can be tested without OBS and
//! without the NDI runtime.

pub mod convert;
pub mod finder;
pub mod module;
pub mod ndi;
pub mod netconfig;
pub mod outputs;
pub mod resume;
pub mod send;
pub mod settings;
pub mod tally;
pub mod visibility;
