//! Layer 1: reusable GPUI components and stateless page/form building blocks.

/// Deduplicated bottom-right feedback notifications and semantic severity kinds.
pub mod feedback;
pub(crate) mod form_primitives;
/// Brand-styled controlled switch built on `gpui_kit::base::Switch`.
pub mod mint_switch;
pub(crate) mod page_primitives;
/// Primary navigation sidebar with a compatibility export for outbound modes.
pub mod sidebar;
/// Reusable surface cards, detail rows, status pills, table header cells, and copy buttons.
pub mod surface;
/// Circular percentage indicator with theme-aware water waves.
pub mod wave_percentage;
/// Shared workspace page title and header typography helpers.
pub mod workspace;
