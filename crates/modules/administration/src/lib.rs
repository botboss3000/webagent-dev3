//! Administration: protected operator tools and APIs over the build broker.
//!
//! This crate holds the rules its host crate (`webagent-administration-host`) and the
//! root share: which native tools are Administrator tools, the schema each one reads
//! from its tool folder, how model arguments are projected before they reach the
//! broker, and the checks on broker notices, dashboard requests and source reviews.
//! Model arguments never choose identity, agent, workspace, credentials, host origin
//! or a Docker command; the host attaches those.
pub mod arguments;
pub mod dashboard;
pub mod notices;
pub mod review;
pub mod schemas;
pub mod tools;
