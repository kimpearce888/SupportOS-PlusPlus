//! HTTP API server — mirrors the reference's Fastify HTTP server.

// Route stubs are being filled in incrementally. Suppress warnings for
// unused imports/variables in the stub modules until they're fully implemented.

#[allow(warnings)]
pub mod event_bus;
#[allow(warnings)]
pub mod rate_limit;
#[allow(warnings)]
pub mod routes;
#[allow(warnings)]
pub mod server;

pub use event_bus::EventBus;
pub use rate_limit::RateLimiter;
pub use server::HttpServer;
