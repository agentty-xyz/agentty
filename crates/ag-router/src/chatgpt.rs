//! "Sign in with `ChatGPT`": OAuth sign-in and refreshing credentials that bill
//! `OpenAI` Responses requests to the user's `ChatGPT` plan instead of API
//! credits.

mod auth;
mod credentials;
mod error;
mod login;
mod token;

pub use auth::ChatGptAuth;
pub use credentials::ChatGptCredentials;
pub use error::ChatGptAuthError;
pub use login::ChatGptLogin;
