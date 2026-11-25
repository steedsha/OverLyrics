//! QA library for the GPUI experiment. Task 10 splits the tool into modules:
//! [`replay`] feeds recorded packet scripts to the experiment's transport
//! model, `compare` diffs captured images, and the binary keeps the Task 9
//! `video-dump` subcommand for the bit-exact decoder QA.

pub mod acceptance;
pub mod compare;
pub mod replay;
