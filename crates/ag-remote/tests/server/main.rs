//! Public remote control server contracts, linked as one test binary. Every
//! device reaches the server through an in-process relay.

mod lifecycle;
mod pairing;
mod relay;
mod request;
mod support;
