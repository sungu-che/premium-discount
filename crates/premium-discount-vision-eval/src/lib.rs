//! Cross-ecosystem vision evaluation harness (`premium-discount-vision-eval`):
//! links `louis-vision` (`lv-core`, `lv-screen`, `lv-doc`, `lv-spatial`, `lv-domain`, `lv-planar`) at `<LV_LIB_PIN_REV>`
//! and grades precision, calibration, gate A/B savings, WORM release seal, and source-tree hygiene via `premium-discount-eval`.

pub mod cross_hygiene;
pub mod doc_eval;
pub mod release_seal;
pub mod screen_eval;
pub mod spatial_doc_eval;
pub mod spatial_eval;
