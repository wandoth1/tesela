//! Limpieza de Tesela (PLAN.md §2.5 a §2.7 y §3.7).
#![cfg(windows)]

pub mod basket;
pub mod engine;
pub mod exec;
pub mod log;
pub mod paths;
pub mod plan;
pub mod process;
pub mod protect;
pub mod recycle;
pub mod roots;
pub mod rules;
pub mod sys;
#[cfg(test)]
mod testutil;
pub mod volume;
