//! src/ui/mod.rs — UI primitives module root.
//!
//! A thin facade over the render helpers: column-safe text math (`text`),
//! the standard screen skeleton + centered overlays (`layout`), the persistent
//! header/tabbar/footer chrome (`chrome`), and reusable widgets (`widgets`).
//! Screens compose these; nothing here owns app state.

pub mod chrome;
pub mod layout;
pub mod text;
pub mod widgets;
