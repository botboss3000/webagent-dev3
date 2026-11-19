//! Administration: protected operator tools and APIs over the build broker.
//!
//! This crate holds the rules its host crate (`webagent-administration-host`) and the
//! root share: which native tools are Administrator tools, the schema each one reads
//! from its tool folder, how model arguments are projected before they reach the
//! broker, and the checks on broker notices, dashboard requests.
//! Model arguments never choose identity, agent, workspace, credentials, host origin
//! or a Docker command; the host attaches those.
pub mod app_defaults;
pub mod arguments;
pub mod board;
pub mod dashboard;
pub mod find_ranking;
pub mod notices;
pub mod schemas;
pub mod tools;
