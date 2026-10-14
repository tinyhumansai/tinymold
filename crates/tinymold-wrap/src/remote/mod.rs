//! The remote tier, as the wrapper sees it.
//!
//! On a local miss, the wrapper asks a [`Remote`] for the input sets of the
//! pre-key, computes each set's full key against the local files, and asks
//! for the first action it does not hold. The remote places every object the
//! action needs into the local store, which names each one by its own
//! BLAKE3 digest. The wrapper then checks that every object the action
//! record names is present before trusting it. A remote can therefore make a
//! unit miss, but never restore bytes other than the ones the record names.
//!
//! The trait is synchronous and knows nothing of networks, so this crate
//! stays free of async runtimes; `tinymold-remote` implements it over REAPI.
//! See `docs/specs/remote.md`.

use tinymold_cas::Store;
use tinymold_hash::Digest;

/// Why a remote call failed. Any failure only makes the unit miss.
pub type RemoteError = Box<dyn std::error::Error + Send + Sync>;

/// A shared cache the wrapper consults when the local store misses.
pub trait Remote {
    /// The pre-key record stored remotely under `pre`: the input sets other
    /// machines compiled with this pre-key, in the text form of
    /// `prekeys/<pre>`. `None` when the remote has none.
    ///
    /// # Errors
    ///
    /// Returns an error when the remote cannot be reached or answers
    /// something that is not a record.
    fn input_sets(&self, pre: &Digest) -> Result<Option<String>, RemoteError>;

    /// Fetches the action stored remotely under the full key `key`: places
    /// every object it references into `store`, then returns the action
    /// record's text. `None` when the remote has no such action.
    ///
    /// # Errors
    ///
    /// Returns an error when the remote cannot be reached, or a blob it sends
    /// does not match its digest.
    fn fetch_action(&self, key: &Digest, store: &Store) -> Result<Option<String>, RemoteError>;
}
