mod handlers;
mod identity;
mod ops;

// Glob re-exports: `identity::*` and `ops::*` re-export items used by external
// callers (the integration test crate, registry handlers). The lib-only build
// can't see those uses, so rustc reports the globs as unused — silence that.
#[allow(unused_imports)]
pub use handlers::*;
#[allow(unused_imports)]
pub use identity::*;
#[allow(unused_imports)]
pub use ops::*;
