//! HTTP API server — mirrors the reference's Fastify HTTP server.

pub mod server;
pub mod routes;

pub use server::HttpServer;
