//! Build-side release pipeline and CLI implementation.

#![forbid(unsafe_code)]

pub mod cli;
pub mod commands;
pub mod ignore;
pub mod output;
pub mod pipeline;
pub mod publisher;
