//! A shop's refund desk under nine attacks, each recorded from the real runtime for the
//! website. Only the model's reading is scripted.
//!
//! | Module | What it holds |
//! |---|---|
//! | [`order`] | the `order` workflow: a refund asked for, confirmed on a card, sent |
//! | [`desk`] | the world of a run: orders, stores, the payment provider, turns |
//! | [`record`] | a run as frames, at the station where each decision was taken |
//! | [`runs`] | the ten runs, and the recording the website plays |

#![forbid(unsafe_code)]
// An example: a broken fixture is a bug to see at once, not an error to carry.
#![allow(clippy::expect_used)]

pub mod desk;
pub mod order;
pub mod record;
pub mod runs;
