//! Independent personal-agent server. PostgreSQL owns durable state.
pub mod api;
pub mod auth;
pub mod authenticated_model;
pub mod config;
pub mod engine;
pub mod mcp;
pub mod runner;
pub mod store;
pub mod web;
