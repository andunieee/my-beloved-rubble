//! Core library for My Beloved Rubble (`mbr`): content-addressed file
//! folders in the spirit of git-annex.
//!
//! A "rubble folder" is any directory that has been attached with
//! [`Repo::init`]. Regular files dropped into it are, on the next scan,
//! moved into a hidden `.blobs/` store addressed by their SHA-256 hash and
//! replaced by a relative symlink, so they keep opening normally:
//!
//! ```text
//! folder/
//!   .blobs/fa/2e/fa2e38…   actual content
//!   .mbr.db                sqlite metadata (paths, blobs, remotes)
//!   fruits.png -> .blobs/fa/2e/fa2e38…
//! ```
//!
//! Blobs can be pushed to / fetched from rclone remotes, named on the
//! remote purely by hash (sharded `fa/2e/fa2e38…`, no metadata).

pub mod db;
pub mod rclone;
pub mod repo;
pub mod util;

pub use repo::Repo;
