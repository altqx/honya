//! The component kit: everything a screen or overlay composes itself from.
//!
//! One rule holds the kit together — **a component draws and registers its
//! interactive rectangles in the same call**. Every component therefore takes
//! `&mut Zones` alongside the `Frame` and `Rect` it draws into. Nothing here
//! owns app state; components are given what they need and hand back what was
//! clicked via [`zones::ZoneId`].
//!
//! Width is always measured with [`crate::ui::text`] helpers, never
//! `String::len`, so CJK and Thai lay out correctly.

// Scaffolding note: this module is the component kit being built ahead of its
// callers. Screens and overlays are ported onto it one at a time, so parts of
// the surface are legitimately unused between those steps. This allow comes off
// once the last screen is ported — it must not outlive the redesign.
#![allow(dead_code, unused_imports)]
pub mod badge;
pub mod button;
pub mod ctx;
pub mod focus;
pub mod form;
pub mod list;
pub mod modal;
pub mod progress;
pub mod style;
pub mod tabs;
pub mod tokens;
pub mod zones;

pub use ctx::Ui;
pub use focus::{Focus, Hover};
pub use style::State;
pub use tokens::{Breakpoint, Density, Metrics};
pub use zones::{ZoneId, ZoneKind, Zones};
