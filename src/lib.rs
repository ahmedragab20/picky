//! picky: picks which CI tests run for a change.

pub mod commands;
pub mod config;
pub mod decide;
pub mod git;
pub mod glob;
pub mod graph;
pub mod hook;
pub mod init;
pub mod job;
pub mod js;
pub mod json;
pub mod jsonc;
pub mod jsre;
pub mod judge;
pub mod kotlin;
pub mod npath;
pub mod php;
pub mod plan;
pub mod report;
pub mod rust;
pub mod sh;
pub mod shell;
pub mod tsimports;
pub mod yaml;

/// Environment variables, as picky reads them.
pub type Env = std::collections::HashMap<String, String>;
